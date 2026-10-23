//! Loopback-only bridge for the browser simulator (design: ADR-002, R23).
//!
//! Every request, including the WebSocket upgrade, passes the Host/Origin check. No CORS headers
//! are ever sent. One upstream TCP connection per WebSocket client (no shared state; the feed
//! is a plain broadcast, so a second client just opens a second connection).

pub mod config;

use axum::{
    Router,
    body::Body,
    extract::{
        State, WebSocketUpgrade,
        ws::{CloseFrame, Message, WebSocket},
    },
    http::{HeaderMap, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use config::Config;
use std::{sync::Arc, time::Duration};
use tokio::{io::AsyncReadExt, net::TcpStream, time::Instant};

type Shared = Arc<Config>;

pub fn router(cfg: Shared) -> Router {
    Router::new()
        .route("/ws/live", get(ws_live))
        .route("/local/config.json", get(local_config))
        .route("/local/capture", get(local_capture))
        .layer(middleware::from_fn_with_state(cfg.clone(), guard))
        .with_state(cfg)
}

/// Why a request is refused, or `None` if allowed.
/// `Origin` must match when present and is mandatory for a WebSocket upgrade (browsers send no
/// `Origin` on same-origin GETs, so requiring it everywhere would break `fetch`).
/// Host: trunk's proxy rewrites `Host` to the backend (observed: bare `127.0.0.1`, no port) and
/// moves the browser's value to `X-Forwarded-Host`. When that header is present, it is the one
/// that must be allowed, and `Host` must then be this loopback backend (never a foreign name).
fn forbidden(cfg: &Config, h: &HeaderMap, is_ws: bool) -> Option<&'static str> {
    // Cross-site browser requests are refused outright; non-browser clients send no header.
    if h.get_all("sec-fetch-site")
        .iter()
        .any(|v| !matches!(v.as_bytes(), b"same-origin" | b"none"))
    {
        return Some("cross-site request");
    }
    let host = h
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_ascii_lowercase);
    // trunk appends to a client-supplied X-Forwarded-Host, so only exactly one plain value is trusted.
    let mut fwds = h.get_all("x-forwarded-host").iter();
    let fwd = match (fwds.next(), fwds.next()) {
        (None, _) => None,
        (Some(v), None) => match v.to_str() {
            Ok(v) if !v.contains(',') => Some(v.to_ascii_lowercase()),
            _ => return Some("host not allowed"),
        },
        _ => return Some("host not allowed"),
    };
    let ok = match fwd {
        Some(fwd) => {
            cfg.hosts.contains(&fwd)
                && host
                    .is_some_and(|v| cfg.hosts.contains(&v) || v == "127.0.0.1" || v == "localhost")
        }
        None => host.is_some_and(|v| cfg.hosts.contains(&v)),
    };
    if !ok {
        return Some("host not allowed");
    }
    match h.get(header::ORIGIN) {
        None if is_ws => Some("origin required"),
        None => None,
        Some(o) if o.to_str().is_ok_and(|o| cfg.origins.contains(o)) => None,
        Some(_) => Some("origin not allowed"),
    }
}

async fn guard(State(cfg): State<Shared>, req: Request<Body>, next: Next) -> Response {
    let is_ws = req
        .headers()
        .get(header::UPGRADE)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
    match forbidden(&cfg, req.headers(), is_ws) {
        Some(why) => (StatusCode::FORBIDDEN, why).into_response(),
        None => next.run(req).await,
    }
}

async fn local_config(State(cfg): State<Shared>) -> Response {
    match &cfg.config_json {
        Some(j) => {
            eprintln!("config served ({} bytes)", j.len());
            ([(header::CONTENT_TYPE, "application/json")], j.clone()).into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn local_capture(State(cfg): State<Shared>) -> Response {
    let Some(path) = &cfg.capture else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match tokio::fs::read(path).await {
        Ok(b) => {
            eprintln!("capture served ({} bytes)", b.len());
            ([(header::CONTENT_TYPE, "text/plain")], b).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn ws_live(State(cfg): State<Shared>, ws: WebSocketUpgrade) -> Response {
    match cfg.feed.clone() {
        Some(feed) => match cfg.slots.clone().try_acquire_owned() {
            // permit lives as long as the relay; refused clients never reach the upstream
            Ok(permit) => ws.on_upgrade(move |s| async move {
                relay(s, feed, cfg.upstream_grace).await;
                drop(permit);
            }),
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Resolves when the client goes away; other client messages are ignored.
async fn closed(ws: &mut WebSocket) {
    while let Some(Ok(m)) = ws.recv().await {
        if matches!(m, Message::Close(_)) {
            break;
        }
    }
}

/// Resolves at `deadline`; never if there is none.
async fn expired(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

/// Raw feed bytes -> binary WS messages, reconnecting upstream with backoff while the client stays.
/// If no data arrives for `grace` after the upstream was lost, the client gets a normal close so
/// it can show the feed as down and reconnect on its own schedule.
async fn relay(mut ws: WebSocket, feed: String, grace: Duration) {
    let mut backoff = Duration::from_millis(500);
    let mut buf = [0u8; 4096];
    let mut deadline = Some(Instant::now() + grace); // also covers a first connect that never succeeds
    loop {
        let conn = tokio::select! {
            c = TcpStream::connect(&feed) => c,
            () = closed(&mut ws) => return,
            () = expired(deadline) => return give_up(ws).await,
        };
        let Ok(mut tcp) = conn else {
            tokio::select! {
                () = tokio::time::sleep(backoff) => {}
                () = closed(&mut ws) => return,
                () = expired(deadline) => return give_up(ws).await,
            }
            backoff = (backoff * 2).min(Duration::from_secs(8));
            continue;
        };
        backoff = Duration::from_millis(500);
        loop {
            tokio::select! {
                n = tcp.read(&mut buf) => match n {
                    Ok(n) if n > 0 => {
                        deadline = None;
                        if ws.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() {
                            return;
                        }
                    }
                    _ => {
                        // upstream gone: reconnect, within the grace period
                        deadline.get_or_insert_with(|| Instant::now() + grace);
                        break;
                    }
                },
                () = closed(&mut ws) => return,
                () = expired(deadline) => return give_up(ws).await,
            }
        }
    }
}

async fn give_up(mut ws: WebSocket) {
    let frame = CloseFrame {
        code: 1000,
        reason: "upstream feed lost".into(),
    };
    let _ = ws.send(Message::Close(Some(frame))).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use tower::ServiceExt;

    fn app() -> Router {
        router(Arc::new(
            Config::from_sources(Some("ADSB_RX_LAT=1\nADSB_RX_LON=2"), |_| None).unwrap(),
        ))
    }

    async fn status_h(
        host: Option<&str>,
        origin: Option<&str>,
        extra: &[(&str, &str)],
        path: &str,
    ) -> StatusCode {
        let mut r = Request::builder().uri(path);
        if let Some(h) = host {
            r = r.header("host", h);
        }
        if let Some(o) = origin {
            r = r.header("origin", o);
        }
        for (k, v) in extra {
            r = r.header(*k, *v);
        }
        app()
            .oneshot(r.body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    async fn status(host: Option<&str>, origin: Option<&str>, path: &str) -> StatusCode {
        status_h(host, origin, &[], path).await
    }

    #[tokio::test]
    async fn trunk_proxy_headers() {
        let p = "/local/config.json";
        let o = Some("http://localhost:8080");
        let via = |fwd| [("x-forwarded-host", fwd)];
        // what trunk 0.22 sends: Host rewritten to the backend, browser's Host forwarded
        assert_eq!(
            status_h(Some("127.0.0.1"), o, &via("localhost:8080"), p).await,
            StatusCode::OK
        );
        // rebinding through trunk: foreign name arrives as X-Forwarded-Host
        assert_eq!(
            status_h(Some("127.0.0.1"), None, &via("evil.example:8080"), p).await,
            StatusCode::FORBIDDEN
        );
        // a foreign Host cannot be laundered by a forwarded header
        assert_eq!(
            status_h(Some("evil.example"), None, &via("localhost:8080"), p).await,
            StatusCode::FORBIDDEN
        );
        // foreign page through trunk: allowed Host, foreign Origin
        assert_eq!(
            status_h(
                Some("127.0.0.1"),
                Some("http://evil.example"),
                &via("localhost:8080"),
                p
            )
            .await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn forwarded_host_and_fetch_site_matrix() {
        let p = "/local/config.json";
        let h = Some("127.0.0.1");
        let ok = StatusCode::OK;
        let no = StatusCode::FORBIDDEN;
        let fwd = ("x-forwarded-host", "localhost:8080");
        // duplicate / list forms, in either order
        for extra in [
            vec![fwd, ("x-forwarded-host", "evil.example")],
            vec![("x-forwarded-host", "evil.example"), fwd],
            vec![fwd, fwd],
            vec![("x-forwarded-host", "localhost:8080, evil.example")],
            vec![("x-forwarded-host", "localhost:8080,")],
        ] {
            assert_eq!(status_h(h, None, &extra, p).await, no, "{extra:?}");
        }
        for (site, want) in [
            ("same-origin", ok),
            ("none", ok),
            ("cross-site", no),
            ("same-site", no),
            ("bogus", no),
        ] {
            assert_eq!(
                status_h(h, None, &[fwd, ("sec-fetch-site", site)], p).await,
                want,
                "{site}"
            );
        }
        assert_eq!(status_h(h, None, &[fwd], p).await, ok); // header absent
        // either value cross-site is enough
        assert_eq!(
            status_h(
                h,
                None,
                &[
                    fwd,
                    ("sec-fetch-site", "none"),
                    ("sec-fetch-site", "cross-site")
                ],
                p
            )
            .await,
            no
        );
    }

    #[tokio::test]
    async fn host_and_origin_checks() {
        let p = "/local/config.json";
        let ok = StatusCode::OK;
        let no = StatusCode::FORBIDDEN;
        assert_eq!(status(Some("localhost:8080"), None, p).await, ok);
        assert_eq!(status(Some("127.0.0.1:8081"), None, p).await, ok);
        assert_eq!(
            status(Some("LOCALHOST:8081"), Some("http://localhost:8080"), p).await,
            ok
        );
        // foreign origin, with a legitimate Host
        assert_eq!(
            status(Some("localhost:8080"), Some("http://evil.example"), p).await,
            no
        );
        // origin of the right host on the wrong scheme/port
        assert_eq!(
            status(Some("localhost:8080"), Some("https://localhost:8080"), p).await,
            no
        );
        assert_eq!(
            status(Some("localhost:8080"), Some("http://localhost:9999"), p).await,
            no
        );
        // DNS rebinding: attacker name resolving to 127.0.0.1, same-origin from its own page
        assert_eq!(status(Some("evil.example:8080"), None, p).await, no);
        assert_eq!(
            status(
                Some("evil.example:8080"),
                Some("http://evil.example:8080"),
                p
            )
            .await,
            no
        );
        assert_eq!(status(Some("localhost"), None, p).await, no);
        assert_eq!(status(None, None, p).await, no);
        // check also guards unknown paths and the other endpoints
        assert_eq!(
            status(Some("evil.example"), None, "/local/capture").await,
            no
        );
        assert_eq!(
            status(Some("localhost:8080"), None, "/local/capture").await,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn ws_upgrade_requires_allowed_origin() {
        let get = |origin: Option<&str>| {
            let mut r = Request::builder()
                .uri("/ws/live")
                .header("host", "localhost:8080")
                .header("upgrade", "websocket");
            if let Some(o) = origin {
                r = r.header("origin", o);
            }
            r.body(Body::empty()).unwrap()
        };
        assert_eq!(
            app().oneshot(get(None)).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
        let r = app()
            .oneshot(get(Some("http://evil.example")))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        assert!(r.headers().get("access-control-allow-origin").is_none());
    }
}
