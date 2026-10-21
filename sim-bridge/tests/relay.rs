use adsb_core::scenario::{Event, Kind, Scenario};
use futures_util::StreamExt;
use sim_bridge::{config::Config, router};
use std::{net::SocketAddr, sync::Arc};
use tokio::{io::AsyncWriteExt, net::TcpListener};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};

/// Start the bridge on an ephemeral loopback port with a temp-`.env` style config.
async fn bridge(env: &str) -> SocketAddr {
    let mut cfg = Config::from_sources(Some(env), |_| None).unwrap();
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    cfg.hosts.insert(addr.to_string());
    tokio::spawn(axum::serve(l, router(Arc::new(cfg))).into_future());
    addr
}

fn ws_req(addr: SocketAddr, origin: Option<&str>) -> tungstenite::handshake::client::Request {
    let mut r = format!("ws://{addr}/ws/live")
        .into_client_request()
        .unwrap();
    if let Some(o) = origin {
        r.headers_mut().insert("origin", o.parse().unwrap());
    }
    r
}

async fn http_get(addr: SocketAddr, path: &str, headers: &str) -> (u16, String) {
    use tokio::io::AsyncReadExt;
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n{headers}\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let code = out[9..12].parse().unwrap();
    (code, out)
}

fn overflight_bytes() -> Vec<u8> {
    let mut sc = Scenario::new(Kind::Overflight, 1);
    let mut all = Vec::new();
    sc.poll(120_000, &mut |_, ev| {
        if let Event::Line(l) = ev {
            all.extend_from_slice(l.as_bytes());
        }
    });
    assert!(all.len() > 1000);
    all
}

#[tokio::test]
async fn ws_relays_feed_byte_exact() {
    let want = overflight_bytes();
    let feed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = feed.local_addr().unwrap().port();
    let data = want.clone();
    tokio::spawn(async move {
        let (mut s, _) = feed.accept().await.unwrap();
        // odd chunking: the relay must not care about line boundaries
        for c in data.chunks(777) {
            s.write_all(c).await.unwrap();
        }
        s.shutdown().await.unwrap();
        // keep the listener alive; the relay will reconnect and wait
        let _ = feed.accept().await;
    });
    let addr = bridge(&format!("ADSB_FEED_HOST=127.0.0.1\nADSB_FEED_PORT={port}")).await;
    let (mut ws, _) = tokio_tungstenite::connect_async(ws_req(addr, Some("http://localhost:8080")))
        .await
        .unwrap();
    let mut got = Vec::new();
    while got.len() < want.len() {
        match ws.next().await.unwrap().unwrap() {
            tungstenite::Message::Binary(b) => got.extend_from_slice(&b),
            m => panic!("unexpected {m:?}"),
        }
    }
    assert_eq!(got, want);
}

#[tokio::test]
async fn ws_rejects_foreign_or_missing_origin() {
    let addr = bridge("ADSB_FEED_HOST=127.0.0.1\nADSB_FEED_PORT=1").await;
    for o in [None, Some("http://evil.example")] {
        match tokio_tungstenite::connect_async(ws_req(addr, o)).await {
            Err(tungstenite::Error::Http(r)) => assert_eq!(r.status(), 403, "{o:?}"),
            other => panic!("{o:?}: {other:?}"),
        }
    }
}

#[tokio::test]
async fn local_config_404_without_env_200_with() {
    let addr = bridge("ADSB_FEED_HOST=127.0.0.1").await;
    assert_eq!(http_get(addr, "/local/config.json", "").await.0, 404);

    let addr =
        bridge("ADSB_RX_LAT=12.25\nADSB_RX_LON=-34.5\nADSB_RX_ELEV_M=30\nADSB_RADIUS_KM=6").await;
    let (code, body) = http_get(addr, "/local/config.json", "").await;
    assert_eq!(code, 200);
    assert!(
        body.ends_with(r#"{"lat":12.25,"lon":-34.5,"elev_m":30,"radius_km":6}"#),
        "{body}"
    );
    assert!(!body.to_ascii_lowercase().contains("access-control"));
    // a foreign page's request is refused
    assert_eq!(
        http_get(
            addr,
            "/local/config.json",
            "Origin: http://evil.example\r\n"
        )
        .await
        .0,
        403
    );
}

#[tokio::test]
async fn local_capture_served_from_configured_path() {
    let p = std::env::temp_dir().join(format!("sim-bridge-test-{}.capture", std::process::id()));
    std::fs::write(&p, "@000000000000;\n").unwrap();
    let addr = bridge(&format!("ADSB_REPLAY_CAPTURE={}", p.display())).await;
    let (code, body) = http_get(addr, "/local/capture", "").await;
    std::fs::remove_file(&p).unwrap();
    assert_eq!(code, 200);
    assert!(body.ends_with("@000000000000;\n"));
    assert_eq!(http_get(addr, "/local/capture", "").await.0, 404); // file gone
}

#[tokio::test]
async fn client_cap_refuses_with_503_and_no_upstream_connection() {
    let feed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = feed.local_addr().unwrap().port();
    let addr = bridge(&format!(
        "ADSB_FEED_HOST=127.0.0.1\nADSB_FEED_PORT={port}\nADSB_SIM_MAX_CLIENTS=1"
    ))
    .await;
    let o = Some("http://localhost:8080");
    let (first, _) = tokio_tungstenite::connect_async(ws_req(addr, o))
        .await
        .unwrap();
    let (_up, _) = feed.accept().await.unwrap(); // first client's upstream
    match tokio_tungstenite::connect_async(ws_req(addr, o)).await {
        Err(tungstenite::Error::Http(r)) => assert_eq!(r.status(), 503),
        other => panic!("{other:?}"),
    }
    let second = tokio::time::timeout(std::time::Duration::from_millis(300), feed.accept()).await;
    assert!(
        second.is_err(),
        "refused client opened an upstream connection"
    );
    // closing the first client frees the slot
    drop(first);
    for _ in 0..50 {
        if tokio_tungstenite::connect_async(ws_req(addr, o))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("slot never freed");
}
