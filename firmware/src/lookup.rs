//! `meta_task` (docs/design/radar-ui.md RD.9): looks up airline, type, registration and route of the
//! focus aircraft and the next two (adsbdb, text only) and fills the cache `ui_task` reads.
//! One HTTPS connection at a time, kept open between requests (a handshake costs seconds of CPU
//! and blocks the whole executor). Logs carry ICAO, callsign-derived values and counts, never the
//! contact, coordinates or the feed host.

extern crate alloc;

use core::sync::atomic::Ordering::Relaxed;

use adsb_core::meta::{self, FocusMeta, Kind, Policy, RateLimiter, Step, TextCache, Want};
use adsb_core::tracker::Snapshot;
use embassy_net::Stack;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Duration, Instant, Timer};
use log::{info, warn};

use crate::config;
use crate::tls::{self, Bufs, Conn, Error};

const HOST: &str = "api.adsbdb.com";
/// The server keeps an idle connection for at least a minute (checked 2026-10-24); an older one is
/// closed here and the next request opens a new one.
const IDLE_CLOSE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(500);

struct Shared {
    cache: TextCache,
    /// The latest plan of `ui_task`: focus first, then the prefetch aircraft.
    wants: alloc::vec::Vec<Want>,
}

static SHARED: Mutex<CriticalSectionRawMutex, Shared> =
    Mutex::new(Shared { cache: TextCache::new(), wants: alloc::vec::Vec::new() });

/// Called by `ui_task` every tick: hands the plan to `meta_task`, returns the panel text of the focus.
pub async fn publish(policy: &mut Policy, snap: &Snapshot, now_ms: u64) -> FocusMeta {
    let plan = policy.plan(snap, now_ms);
    let mut sh = SHARED.lock().await;
    let meta = plan.text.first().map(|w| sh.cache.focus_meta(w, now_ms)).unwrap_or_default();
    sh.wants.clear();
    sh.wants.extend(plan.text);
    meta
}

fn now_ms() -> u64 {
    Instant::now().as_millis()
}

/// The first planned aircraft that still lacks an answer.
async fn next_job() -> Option<(Want, Kind)> {
    let mut sh = SHARED.lock().await;
    let Shared { cache, wants } = &mut *sh;
    wants.iter().find_map(|w| Some((w.clone(), cache.needs(w, now_ms())?)))
}

async fn wait_ready(limiter: &RateLimiter) {
    while !limiter.ready(now_ms()) {
        Timer::after(Duration::from_millis(100)).await;
    }
}

/// An error that is not an answer: back off (the limiter doubles the pause up to 60 s).
fn failed(limiter: &mut RateLimiter, why: &str) {
    limiter.failure(now_ms());
    warn!("meta: error {why}");
    info!("meta: backoff");
}

#[embassy_executor::task]
pub async fn meta_task(stack: Stack<'static>) {
    while tls::unix_time().is_none() {
        Timer::after(Duration::from_secs(1)).await;
    }
    let ua = match config::HTTP_CONTACT {
        Some(contact) => alloc::format!("esp32-adsb/{} (+{contact})", env!("CARGO_PKG_VERSION")),
        None => alloc::format!("esp32-adsb/{}", env!("CARGO_PKG_VERSION")),
    };
    let mut bufs = Bufs::default();
    info!(
        "meta: start contact={} wrong_ca={} bufs_psram={}",
        config::HTTP_CONTACT.is_some(),
        cfg!(feature = "tls-wrong-ca"),
        bufs.in_psram()
    );
    let mut limiter = RateLimiter::default();
    loop {
        // Open a connection only when there is something to ask (the handshake is the expensive part).
        while next_job().await.is_none() {
            Timer::after(POLL).await;
        }
        wait_ready(&limiter).await;
        limiter.started(now_ms());
        tls::MIN_INTERNAL_FREE.store(usize::MAX, Relaxed);
        let t0 = Instant::now();
        match Conn::open(stack, HOST, &mut bufs).await {
            Ok(conn) => {
                info!(
                    "meta: tls ok hs_ms={} total_ms={} internal_min={} internal_free={}",
                    conn.handshake_ms,
                    t0.elapsed().as_millis(),
                    tls::MIN_INTERNAL_FREE.load(Relaxed),
                    tls::internal_free()
                );
                serve(conn, &mut limiter, &ua).await;
            }
            Err(e) => {
                warn!("tls: failed host={HOST} err={e:?} total_ms={}", t0.elapsed().as_millis());
                failed(&mut limiter, "connect");
            }
        }
    }
}

/// Answers jobs on `conn` until it is unusable or has been idle for `IDLE_CLOSE`.
async fn serve(mut conn: Conn<'_>, limiter: &mut RateLimiter, ua: &str) {
    loop {
        wait_ready(limiter).await;
        let Some((want, kind)) = next_job().await else {
            if conn.last_used.elapsed() > IDLE_CLOSE {
                break;
            }
            Timer::after(POLL).await;
            continue;
        };
        // `Policy` only plans valid callsigns, so a path always exists.
        let Some(path) = kind.path(want.icao, want.callsign.as_deref()) else { break };
        let t0 = Instant::now();
        limiter.started(t0.as_millis());
        info!("meta: req icao={:06X} kind={} t_ms={}", want.icao, kind_name(kind), t0.as_millis());
        let reply = conn.get(&path, ua).await;
        let ms = t0.elapsed().as_millis();
        let usable = match reply {
            Ok(r) if matches!(r.status, 200 | 404) && r.complete => match meta::adsbdb_step(kind, &r.body) {
                Ok(step) => {
                    store(&want, step, ms).await;
                    limiter.success();
                    r.reusable
                }
                // Only a genuine "unknown ..." answer is cached as negative; garbage is an error.
                Err(e) => {
                    failed(limiter, &alloc::format!("body={e:?}"));
                    false
                }
            },
            Ok(r) if r.status == 429 => {
                limiter.too_many_requests(now_ms());
                warn!("meta: error status=429");
                info!("meta: backoff 300 s");
                false
            }
            Ok(r) => {
                failed(limiter, &alloc::format!("status={}", r.status));
                false
            }
            // A server that closed the idle connection: nothing is wrong, ask again on a new one.
            Err(Error::Stale) => false,
            Err(e) => {
                failed(limiter, &alloc::format!("{e:?}"));
                false
            }
        };
        if !usable {
            break;
        }
    }
    conn.close().await;
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Combined => "combined",
        Kind::Aircraft => "aircraft",
        Kind::Route => "route",
    }
}

async fn store(want: &Want, step: Step, ms: u64) {
    let found = step.aircraft.as_ref().is_some_and(Option::is_some) || step.route.as_ref().is_some_and(Option::is_some);
    let now = now_ms();
    let mut sh = SHARED.lock().await;
    sh.cache.apply(want, step, now);
    if !found {
        info!("meta: not found icao={:06X} ms={ms}", want.icao);
        return;
    }
    let m = sh.cache.focus_meta(want, now);
    let iata = |a: &Option<meta::Airport>| a.as_ref().and_then(|a| a.iata.clone());
    let route = match (iata(&m.origin), iata(&m.destination)) {
        (None, None) => "-".into(),
        (o, d) => alloc::format!("{}-{}", o.unwrap_or_default(), d.unwrap_or_default()),
    };
    info!(
        "meta: ok icao={:06X} airline={} route={route} ms={ms}",
        want.icao,
        m.airline.as_deref().unwrap_or("-"),
    );
}
