//! Platform glue for the shared Slint UI (`adsb-app`): renders into the PSRAM framebuffer, runs the
//! 1 Hz `ui_task` and (feature `demo`) the scenario `feed_task`. Everything here runs on core 0.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use core::time::Duration;

use adsb_app::{App, HEIGHT, Rgb565Pixel, Status, WIDTH};
use adsb_core::meta::{FocusMeta, Policy};
use adsb_core::pipeline::FeedPipeline;
use adsb_core::tracker::{Config, Snapshot};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::{Instant, Ticker};
use log::info;
use static_cell::ConstStaticCell;

use crate::config;
use crate::display;

pub type Pipeline = Mutex<CriticalSectionRawMutex, FeedPipeline>;

/// Set by the feed task; the demo feed also publishes the scenario clock (the UI must snapshot at
/// the time the scenario was last polled to, see `Scenario::poll`).
pub static FEED_OK: AtomicBool = AtomicBool::new(false);
/// Aircraft in the last snapshot, for the feed statistics line.
pub static AIRCRAFT: AtomicU32 = AtomicU32::new(0);
#[cfg(feature = "demo")]
static DEMO_NOW_MS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0); // ms; 49 days of range

pub const DEMO: bool = cfg!(feature = "demo") && config::DEMO.is_some();

fn radius_km() -> f64 {
    #[cfg(feature = "demo")]
    if DEMO {
        return adsb_core::scenario::RADIUS_KM;
    }
    config::RADIUS_KM
}

fn now_ms() -> u64 {
    #[cfg(feature = "demo")]
    if DEMO {
        return u64::from(DEMO_NOW_MS.load(Relaxed));
    }
    Instant::now().as_millis()
}

/// Seed 1, like the sim keyframes, so photos compare with `sim/target/ui-snapshots`.
#[cfg(feature = "demo")]
fn demo_scenario() -> adsb_core::scenario::Scenario {
    use adsb_core::scenario::{Kind, Scenario};
    let kind = match config::DEMO {
        Some(config::Demo::Busy) => Kind::Busy,
        _ => Kind::Overflight,
    };
    Scenario::new(kind, 1)
}

/// The pipeline for the configured receiver (the fictitious one in demo mode).
pub fn new_pipeline() -> FeedPipeline {
    #[cfg(feature = "demo")]
    if DEMO {
        return FeedPipeline::new(demo_scenario().receiver());
    }
    FeedPipeline::new(Config {
        rx_lat_deg: config::RX_LAT,
        rx_lon_deg: config::RX_LON,
        rx_elev_m: config::RX_ELEV_M,
    })
}
fn status() -> Status {
    Status {
        now_ms: now_ms(),
        wifi_ok: DEMO || crate::net::WIFI_OK.load(Relaxed),
        feed_ok: FEED_OK.load(Relaxed),
        radius_km: radius_km(),
    }
}

/// The App plus the framebuffer it renders into.
pub struct FbUi {
    app: App,
    fb: *mut u16,
    line: &'static mut [Rgb565Pixel; WIDTH],
    full_ms: Option<u64>,
    ticks: u32,
    policy: Policy,
    photos: crate::lookup::PhotoCache,
    last_focus: Option<u32>,
}

impl FbUi {
    /// `fb` points to the WIDTH x HEIGHT RGB565 framebuffer (64-byte aligned, never freed).
    pub fn new(fb: *mut u16) -> Self {
        static LINE: ConstStaticCell<[Rgb565Pixel; WIDTH]> =
            ConstStaticCell::new([Rgb565Pixel(0); WIDTH]);
        let app =
            App::new(|| Duration::from_micros(Instant::now().as_micros())).expect("slint platform");
        Self {
            app,
            fb,
            line: LINE.take(),
            full_ms: None,
            ticks: 0,
            policy: Policy::default(),
            photos: adsb_core::meta::Lru::new(adsb_core::meta::PHOTO_CACHE_ENTRIES),
            last_focus: None,
        }
    }

    /// Push the snapshot to Slint, render what changed and make it visible to the scanout DMA.
    /// Returns whether anything was drawn.
    pub fn update(&mut self, snap: &Snapshot, st: &Status, meta: Option<&FocusMeta>) -> bool {
        let kind = self.app.ui().kind;
        AIRCRAFT.store(snap.list.len() as u32, Relaxed);
        // Callsign, distance and bearing only (no coordinates, see Privacy).
        if self.ticks.is_multiple_of(30) {
            let focus = snap.focus.and_then(|i| snap.list.get(i));
            match focus.and_then(|a| Some((a.callsign.as_ref()?, a.position.as_ref()?))) {
                Some((cs, p)) => info!(
                    "ui: focus callsign={cs} dist_km={} bearing={}",
                    (p.ground_m / 1000.0) as u32,
                    p.bearing_deg as u32
                ),
                None => info!("ui: focus none"),
            }
        }
        self.app.update(snap, st, meta);
        // Every 30 s and on focus change; the focus is what the panel title shows (no coordinates).
        let focus = snap.focus.and_then(|i| snap.list.get(i));
        let focus_icao = focus.map(|a| a.icao);
        if self.ticks.is_multiple_of(30) || focus_icao != self.last_focus {
            let ui = self.app.ui();
            match focus {
                Some(a) => match a.callsign.as_ref() {
                    Some(cs) => info!("radar: range={} blips={} focus={cs}", ui.range_km, ui.blips.len()),
                    None => info!("radar: range={} blips={} focus={:06X}", ui.range_km, ui.blips.len(), a.icao),
                },
                None => info!("radar: range={} blips={} focus=none", ui.range_km, ui.blips.len()),
            }
        }
        if DEMO {
            info!("demo: scenario_ms={}", st.now_ms); // matches a photo to a sim frame
        }
        self.last_focus = focus_icao;
        self.ticks = self.ticks.wrapping_add(1);
        if self.full_ms.is_none() || kind != self.app.ui().kind {
            info!("ui: card={:?}", self.app.ui().kind);
        }
        // SAFETY: exclusive CPU access; the DMA only reads. A torn row shows for one frame at worst.
        let fb = unsafe { core::slice::from_raw_parts_mut(self.fb, WIDTH * HEIGHT) };
        let (mut y0, mut y1) = (usize::MAX, 0);
        let t0 = Instant::now();
        let drawn = self.app.render(&mut self.line[..], |y, x, px| {
            let row = &mut fb[y * WIDTH + x.start..y * WIDTH + x.end];
            for (d, p) in row.iter_mut().zip(px) {
                *d = p.0;
            }
            y0 = y0.min(y);
            y1 = y1.max(y);
        });
        if drawn {
            #[cfg(feature = "diag-border")]
            draw_border(fb, y0, y1);
            // SAFETY: `fb` is the framebuffer (see `new`).
            unsafe { display::writeback_rows(self.fb, y0, y1 + 1) };
            let ms = t0.elapsed().as_millis();
            let full = *self.full_ms.get_or_insert_with(|| {
                info!("ui: first frame");
                ms
            });
            info!(
                "ui: render full_ms={full} partial_ms={ms} rows={}",
                y1 + 1 - y0
            );
        }
        drawn
    }
}

/// Drift check: 1-px white frame, redrawn on the rows just rendered (the rest is untouched).
#[cfg(feature = "diag-border")]
fn draw_border(fb: &mut [u16], y0: usize, y1: usize) {
    for y in y0..=y1 {
        let row = &mut fb[y * WIDTH..(y + 1) * WIDTH];
        if y == 0 || y == HEIGHT - 1 {
            row.fill(0xFFFF);
        }
        (row[0], row[WIDTH - 1]) = (0xFFFF, 0xFFFF);
    }
}

/// One UI step: snapshot, update, render.
pub async fn tick(ui: &mut FbUi, pipeline: &Pipeline) -> bool {
    let st = status();
    let snap = pipeline.lock().await.snapshot(st.now_ms, st.radius_km);
    // The demo scenario has no network; real builds show what `meta_task` found.
    let meta = if DEMO { None } else { Some(crate::lookup::publish(&mut ui.policy, &mut ui.photos, &snap, st.now_ms).await) };
    ui.update(&snap, &st, meta.as_ref())
}

#[embassy_executor::task]
pub async fn ui_task(mut ui: FbUi, pipeline: &'static Pipeline) {
    let mut ticker = Ticker::every(embassy_time::Duration::from_secs(1));
    let mut last = Instant::now();
    loop {
        ticker.next().await;
        // A late tick means the executor was blocked (a TLS handshake, RD.9).
        let gap = last.elapsed().as_millis();
        last = Instant::now();
        if gap > 1500 {
            info!("ui: tick gap_ms={gap}");
        }
        tick(&mut ui, pipeline).await;
    }
}

/// Plays the fictitious demo scenario into the pipeline in real time.
#[cfg(feature = "demo")]
#[embassy_executor::task]
pub async fn feed_task(pipeline: &'static Pipeline) {
    use adsb_core::scenario::Event;
    let mut scenario = demo_scenario();
    let start = Instant::now();
    FEED_OK.store(true, Relaxed);
    let mut tick = Ticker::every(embassy_time::Duration::from_millis(500));
    loop {
        tick.next().await;
        let now = start.elapsed().as_millis();
        let mut p = pipeline.lock().await;
        scenario.poll(now, &mut |at, ev| match ev {
            Event::Line(l) => p.feed(l.as_bytes(), at),
            Event::FeedLost => FEED_OK.store(false, Relaxed),
            Event::FeedRestored => FEED_OK.store(true, Relaxed),
        });
        DEMO_NOW_MS.store(now as u32, Relaxed);
    }
}
