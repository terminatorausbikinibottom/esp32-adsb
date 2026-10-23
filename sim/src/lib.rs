//! Simulator platform layer: runs a synthetic scenario through the same pipeline and `App` as the
//! firmware and keeps the rendered 800x480 RGB565 framebuffer. The browser glue is in `web`.
pub mod live;
#[cfg(target_arch = "wasm32")]
mod web;

use adsb_app::{App, HEIGHT, PointerEvent, Rgb565Pixel, Status, WIDTH};
use adsb_core::pipeline::FeedPipeline;
use adsb_core::scenario::{Event, Kind, RADIUS_KM, Scenario};
use core::time::Duration;

pub struct Sim {
    pub app: App,
    scenario: Scenario,
    pipeline: FeedPipeline,
    feed_ok: bool,
    radius_km: f64,
    /// Persistent framebuffer: the renderer only delivers dirty spans.
    fb: Vec<Rgb565Pixel>,
    line: Vec<Rgb565Pixel>,
}

impl Sim {
    /// One `Sim` per process (the Slint platform is installed once). Frozen animation clock, so
    /// output depends on scenario time only.
    pub fn new(kind: Kind, seed: u64) -> Self {
        Self::with_clock(kind, seed, || Duration::ZERO)
    }

    /// `now` is the monotonic clock Slint animations run on (wall clock in the browser).
    pub fn with_clock(kind: Kind, seed: u64, now: fn() -> Duration) -> Self {
        let scenario = Scenario::new(kind, seed);
        let pipeline = FeedPipeline::new(scenario.receiver());
        Self {
            app: App::new(now).expect("slint platform"),
            scenario,
            pipeline,
            feed_ok: true,
            radius_km: RADIUS_KM,
            fb: vec![Rgb565Pixel(0); WIDTH * HEIGHT],
            line: vec![Rgb565Pixel(0); WIDTH],
        }
    }

    /// Restart at t=0 with a new scenario, keeping the window (and so the Slint platform).
    pub fn reset(&mut self, kind: Kind, seed: u64) {
        self.scenario = Scenario::new(kind, seed);
        self.pipeline = FeedPipeline::new(self.scenario.receiver());
        self.feed_ok = true;
        self.radius_km = RADIUS_KM;
        self.app.reset().expect("slint window");
        // Repaint now: the framebuffer still holds the previous scenario or live frame.
        self.render();
    }

    /// Restart and replay in whole-second steps up to `ms`: the same sequence of states the
    /// browser (which ticks once per scenario second) and the PNG test go through.
    pub fn seek(&mut self, kind: Kind, seed: u64, ms: u64) {
        self.reset(kind, seed);
        for t in (1000..=ms).step_by(1000) {
            self.advance_to(t);
        }
    }

    /// Run the scenario up to `now_ms` (non-decreasing), update the UI and repaint.
    pub fn advance_to(&mut self, now_ms: u64) {
        let (pipeline, feed_ok) = (&mut self.pipeline, &mut self.feed_ok);
        self.scenario.poll(now_ms, &mut |at, ev| match ev {
            Event::Line(l) => pipeline.feed(l.as_bytes(), at),
            Event::FeedLost => *feed_ok = false,
            Event::FeedRestored => *feed_ok = true,
        });
        let st = Status {
            now_ms,
            wifi_ok: true,
            feed_ok: self.feed_ok,
            radius_km: self.radius_km,
        };
        self.refresh(&st);
    }

    fn refresh(&mut self, st: &Status) {
        let snap = self.pipeline.snapshot(st.now_ms, st.radius_km);
        self.app.update(&snap, st, None);
        self.render();
    }

    /// Switch to live input for the given receiver (fresh pipeline and `App`, like `reset`).
    /// The scenario is not polled until the next `reset`.
    pub fn start_live(&mut self, rx: adsb_core::tracker::Config, radius_km: f64) {
        self.pipeline = FeedPipeline::new(rx);
        self.feed_ok = false;
        self.radius_km = radius_km;
        self.app.reset().expect("slint window");
    }

    /// Live bytes from the transport; `now_ms` must not go backwards.
    pub fn feed(&mut self, bytes: &[u8], now_ms: u64) {
        self.pipeline.feed(bytes, now_ms);
    }

    /// A transport (re)connected: see `FeedPipeline::reset_clock`.
    pub fn reset_clock(&mut self) {
        self.pipeline.reset_clock();
    }

    /// Live 1 Hz tick.
    pub fn tick_live(&mut self, now_ms: u64, feed_ok: bool) {
        self.feed_ok = feed_ok;
        let st = Status {
            now_ms,
            wifi_ok: true,
            feed_ok,
            radius_km: self.radius_km,
        };
        self.refresh(&st);
    }

    /// Returns whether the framebuffer changed.
    pub fn pointer(&mut self, ev: PointerEvent) -> bool {
        self.app.pointer(ev);
        self.render()
    }

    /// Repaint whatever is dirty (timers, animations); returns whether the framebuffer changed.
    pub fn render(&mut self) -> bool {
        let fb = &mut self.fb;
        self.app.render(&mut self.line, |y, x, px| {
            fb[y * WIDTH + x.start..][..px.len()].copy_from_slice(px);
        })
    }

    /// FNV-1a 64 over the framebuffer as little-endian RGB565 words, row-major. The browser's
    /// "Dump frame" shows this value; `tests/hash.rs` prints the same for each keyframe.
    pub fn frame_hash(&self) -> u64 {
        self.fb
            .iter()
            .flat_map(|p| p.0.to_le_bytes())
            .fold(0xcbf2_9ce4_8422_2325, |h, b| {
                (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
            })
    }

    pub fn framebuffer(&self) -> &[Rgb565Pixel] {
        &self.fb
    }

    /// RGBA8 copy of the framebuffer (bit replication, the usual 565 -> 888 expansion).
    pub fn rgba(&self) -> Vec<u8> {
        let ex = |v: u16, bits: u32| {
            let v = v as u32;
            ((v << (8 - bits)) | (v >> (2 * bits - 8))) as u8
        };
        self.fb
            .iter()
            .flat_map(|p| {
                [
                    ex(p.0 >> 11, 5),
                    ex((p.0 >> 5) & 63, 6),
                    ex(p.0 & 31, 5),
                    255,
                ]
            })
            .collect()
    }
}
