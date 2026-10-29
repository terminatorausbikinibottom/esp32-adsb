//! `meta_task` (docs/design/radar-ui.md RD.9, RD.10): looks up airline, type, registration and route
//! of the focus aircraft and the next two (adsbdb) and, for the focus once it is stable, its photo
//! (planespotters JSON, then the image, decoded here), and fills what `ui_task` reads.
//! One HTTPS connection at a time (the socket set has room for one), kept open between adsbdb
//! requests (a handshake costs seconds of CPU and blocks the whole executor). Logs carry ICAO, callsign-derived values and counts, never the
//! contact, coordinates or the feed host.

extern crate alloc;

use core::sync::atomic::Ordering::Relaxed;

use adsb_core::meta::{
    self, FocusMeta, Kind, Lru, Photo, Policy, RateLimiter, Step, TTL_AIRCRAFT_MS, TTL_NEGATIVE_MS,
    TextCache, Want,
};
use adsb_core::photo::MAX_JPEG;
use adsb_core::tracker::Snapshot;
use alloc::{rc::Rc, string::String};
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

/// Decoded photos the UI shows (`ui_task` owns it: `Rc` stays on that side), with the photographer.
pub type PhotoCache = Lru<u32, (Rc<Photo>, String)>;

/// A fresh photo on its way from `meta_task` to the UI cache.
struct Ready {
    icao: u32,
    photographer: String,
    photo: Photo,
}

/// Photos are not refetched for this long after "none" or an error (a started fetch counts as one
/// until it ends, so the next tick does not queue it again).
const TTL_PHOTO_ERROR_MS: u64 = 5 * 60_000;

struct Shared {
    cache: TextCache,
    /// The latest plan of `ui_task`: focus first, then the prefetch aircraft.
    wants: alloc::vec::Vec<Want>,
    /// Focus aircraft that needs a photo (stable, not cached, not recently failed).
    photo_job: Option<u32>,
    photo_skip: Lru<u32, ()>,
    ready: Option<Ready>,
}

static SHARED: Mutex<CriticalSectionRawMutex, Shared> = Mutex::new(Shared {
    cache: TextCache::new(),
    wants: alloc::vec::Vec::new(),
    photo_job: None,
    photo_skip: Lru::new(meta::TEXT_CACHE_ENTRIES),
    ready: None,
});

/// Called by `ui_task` every tick: hands the plan to `meta_task`, returns the panel text of the focus.
pub async fn publish(
    policy: &mut Policy,
    photos: &mut PhotoCache,
    snap: &Snapshot,
    now_ms: u64,
) -> FocusMeta {
    let plan = policy.plan(snap, now_ms);
    let mut sh = SHARED.lock().await;
    if let Some(r) = sh.ready.take() {
        photos.insert(
            r.icao,
            (Rc::new(r.photo), r.photographer),
            TTL_AIRCRAFT_MS,
            now_ms,
        );
    }
    let mut meta = plan
        .text
        .first()
        .map(|w| sh.cache.focus_meta(w, now_ms))
        .unwrap_or_default();
    if let Some((photo, by)) = plan.text.first().and_then(|w| photos.get(&w.icao, now_ms)) {
        meta.photo = Some(photo.clone());
        meta.photographer = Some(by.clone());
    }
    sh.photo_job = plan
        .photo
        .filter(|i| photos.get(i, now_ms).is_none() && sh.photo_skip.get(i, now_ms).is_none());
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
    let Shared { cache, wants, .. } = &mut *sh;
    wants
        .iter()
        .find_map(|w| Some((w.clone(), cache.needs(w, now_ms())?)))
}

async fn photo_pending() -> bool {
    SHARED.lock().await.photo_job.is_some()
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
        while next_job().await.is_none() && !photo_pending().await {
            Timer::after(POLL).await;
        }
        wait_ready(&limiter).await;
        // Text first: the photo flow runs when nothing else is missing.
        if next_job().await.is_none() {
            let job = SHARED.lock().await.photo_job.take();
            if let Some(icao) = job {
                fetch_photo(stack, &mut bufs, icao, &mut limiter, &ua).await;
            }
            continue;
        }
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
                warn!(
                    "tls: failed host={HOST} err={e:?} total_ms={}",
                    t0.elapsed().as_millis()
                );
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
            // A photo needs other hosts (and the one socket): give this connection up for it.
            if conn.last_used.elapsed() > IDLE_CLOSE || photo_pending().await {
                break;
            }
            Timer::after(POLL).await;
            continue;
        };
        // `Policy` only plans valid callsigns, so a path always exists.
        let Some(path) = kind.path(want.icao, want.callsign.as_deref()) else {
            break;
        };
        let t0 = Instant::now();
        limiter.started(t0.as_millis());
        info!(
            "meta: req icao={:06X} kind={} t_ms={}",
            want.icao,
            kind_name(kind),
            t0.as_millis()
        );
        let reply = conn.get(&path, ua, tls::MAX_JSON).await;
        let ms = t0.elapsed().as_millis();
        let usable = match reply {
            Ok(r) if matches!(r.status, 200 | 404) && r.complete => {
                match meta::adsbdb_step(kind, &r.body) {
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
                }
            }
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
    let found = step.aircraft.as_ref().is_some_and(Option::is_some)
        || step.route.as_ref().is_some_and(Option::is_some);
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

const PS_HOST: &str = "api.planespotters.net";
const IMG_HOST: &str = "t.plnspttrs.net";

/// Why a photo fetch ended without a photo. `None` is an answer, the others are errors (back-off).
enum PhotoEnd {
    None,
    TooBig,
    Err(String),
}

impl From<Error> for PhotoEnd {
    fn from(e: Error) -> Self {
        Self::Err(alloc::format!("{e:?}"))
    }
}

/// One GET on a fresh connection to `host`, closed afterwards. 429 pauses the limiter.
async fn get_once(
    stack: Stack<'static>,
    bufs: &mut Bufs,
    host: &'static str,
    path: &str,
    ua: &str,
    max: usize,
    limiter: &mut RateLimiter,
) -> Result<alloc::vec::Vec<u8>, PhotoEnd> {
    let t0 = Instant::now();
    let mut conn = Conn::open(stack, host, bufs)
        .await
        .inspect_err(|e| warn!("tls: failed host={host} err={e:?}"))?;
    info!(
        "meta: tls ok host={host} hs_ms={} internal_free={}",
        conn.handshake_ms,
        tls::internal_free()
    );
    wait_ready(limiter).await;
    limiter.started(now_ms());
    let reply = conn.get(path, ua, max).await;
    conn.close().await;
    let r = reply?;
    match r.status {
        200 if r.complete => Ok(r.body),
        200 if r.body.len() > max => Err(PhotoEnd::TooBig),
        429 => {
            limiter.too_many_requests(now_ms());
            Err(PhotoEnd::Err("status=429".into()))
        }
        s => Err(PhotoEnd::Err(alloc::format!(
            "status={s} host={host} ms={}",
            t0.elapsed().as_millis()
        ))),
    }
}

async fn photo_flow(
    stack: Stack<'static>,
    bufs: &mut Bufs,
    icao: u32,
    limiter: &mut RateLimiter,
    ua: &str,
) -> Result<(String, Photo), PhotoEnd> {
    let json_path = alloc::format!("/pub/photos/hex/{icao:06X}");
    let json = get_once(stack, bufs, PS_HOST, &json_path, ua, tls::MAX_JSON, limiter).await?;
    let found = meta::parse_planespotters(&json)
        .map_err(|e| PhotoEnd::Err(alloc::format!("body={e:?}")))?;
    drop(json);
    let found = found.ok_or(PhotoEnd::None)?;
    let path = found.path().ok_or(PhotoEnd::None)?;
    let jpeg = get_once(stack, bufs, IMG_HOST, path, ua, MAX_JPEG, limiter).await?;
    let t0 = Instant::now();
    let photo = adsb_core::photo::decode(&jpeg).map_err(|e| match e {
        adsb_core::photo::PhotoError::TooLarge => PhotoEnd::TooBig,
        adsb_core::photo::PhotoError::Decode => PhotoEnd::Err("decode".into()),
    })?;
    info!(
        "photo: ok icao={icao:06X} bytes={} decode_ms={}",
        jpeg.len(),
        t0.elapsed().as_millis()
    );
    Ok((found.photographer, photo))
}

/// RD.10: fetch, decode and hand over the photo of `icao`. Decode blocks this executor (K9); the UI
/// keeps its old content meanwhile.
async fn fetch_photo(
    stack: Stack<'static>,
    bufs: &mut Bufs,
    icao: u32,
    limiter: &mut RateLimiter,
    ua: &str,
) {
    SHARED
        .lock()
        .await
        .photo_skip
        .insert(icao, (), TTL_PHOTO_ERROR_MS, now_ms());
    info!("meta: req icao={icao:06X} kind=photo t_ms={}", now_ms());
    tls::MIN_INTERNAL_FREE.store(usize::MAX, Relaxed);
    match photo_flow(stack, bufs, icao, limiter, ua).await {
        Ok((photographer, photo)) => {
            limiter.success();
            SHARED.lock().await.ready = Some(Ready {
                icao,
                photographer,
                photo,
            });
        }
        Err(PhotoEnd::None) => {
            info!("photo: none icao={icao:06X}");
            SHARED
                .lock()
                .await
                .photo_skip
                .insert(icao, (), TTL_NEGATIVE_MS, now_ms());
        }
        Err(PhotoEnd::TooBig) => {
            info!("photo: too big icao={icao:06X}");
            SHARED
                .lock()
                .await
                .photo_skip
                .insert(icao, (), TTL_NEGATIVE_MS, now_ms());
        }
        Err(PhotoEnd::Err(e)) => {
            info!("photo: error icao={icao:06X} {e}");
            if !e.contains("429") {
                failed(limiter, "photo");
            }
        }
    }
    info!(
        "photo: heap internal_min={} internal_free={}",
        tls::MIN_INTERNAL_FREE.load(Relaxed),
        tls::internal_free()
    );
}
