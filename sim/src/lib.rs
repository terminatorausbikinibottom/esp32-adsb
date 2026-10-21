//! Simulator platform layer: runs a synthetic scenario through the same pipeline and `App` as the
//! firmware and keeps the rendered 800x480 RGB565 framebuffer. Web glue (S.3) builds on this.
use adsb_app::{App, HEIGHT, PointerEvent, Rgb565Pixel, Status, WIDTH};
use adsb_core::pipeline::FeedPipeline;
use adsb_core::scenario::{Event, Kind, RADIUS_KM, Scenario};
use core::time::Duration;

pub struct Sim {
    pub app: App,
    scenario: Scenario,
    pipeline: FeedPipeline,
    feed_ok: bool,
    /// Persistent framebuffer: the renderer only delivers dirty spans.
    fb: Vec<Rgb565Pixel>,
    line: Vec<Rgb565Pixel>,
}

impl Sim {
    /// One `Sim` per process (the Slint platform is installed once).
    pub fn new(kind: Kind, seed: u64) -> Self {
        let scenario = Scenario::new(kind, seed);
        let pipeline = FeedPipeline::new(scenario.receiver());
        Self {
            app: App::new(|| Duration::ZERO).expect("slint platform"),
            scenario,
            pipeline,
            feed_ok: true,
            fb: vec![Rgb565Pixel(0); WIDTH * HEIGHT],
            line: vec![Rgb565Pixel(0); WIDTH],
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
            radius_km: RADIUS_KM,
        };
        let snap = self.pipeline.snapshot(now_ms, st.radius_km);
        self.app.update(&snap, &st);
        self.render();
    }

    pub fn pointer(&mut self, ev: PointerEvent) {
        self.app.pointer(ev);
        self.render();
    }

    fn render(&mut self) {
        let fb = &mut self.fb;
        self.app.render(&mut self.line, |y, x, px| {
            fb[y * WIDTH + x.start..][..px.len()].copy_from_slice(px);
        });
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
