use crate::ui::{self, AppWindow};
use crate::{Badge, CardKind, HEIGHT, WIDTH, render_by_line};
use adsb_core::geo::compass_label;
use adsb_core::meta::FocusMeta;
use adsb_core::radar::{self, Blip, RangeKm, RangeState};
use adsb_core::tracker::{AircraftView, Card, Marker, Snapshot};
use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ops::Range;
use core::time::Duration;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{
    ComponentHandle, Image, LogicalPosition, Model, ModelRc, PhysicalSize, PlatformError,
    Rgba8Pixel, SharedPixelBuffer, VecModel,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerPhase {
    Down,
    Move,
    Up,
}

/// Touch (GT911) and mouse share this; coordinates are logical pixels, origin top-left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PointerEvent {
    pub x: f32,
    pub y: f32,
    pub phase: PointerPhase,
}

/// What the platform layer knows and the tracker doesn't.
#[derive(Clone, Copy, Debug)]
pub struct Status {
    /// Milliseconds on the same clock as the snapshot (uptime until SNTP, Phase 5).
    pub now_ms: u64,
    pub wifi_ok: bool,
    /// False forces the "No data" overlay (feed known to be down), whatever the tracker says.
    pub feed_ok: bool,
    pub radius_km: f64,
}

/// Everything the Slint window displays. Diffed field by field.
#[derive(Clone, Debug, PartialEq)]
pub struct Ui {
    pub wifi_ok: bool,
    pub feed_ok: bool,
    pub counts: String,
    pub rate: String,
    pub clock: String,
    pub kind: CardKind,
    /// Radar range in km (one of `radar::RANGE_STEPS_KM`).
    pub range_km: RangeKm,
    pub range_label: String,
    pub half_label: String,
    /// "+k beyond", empty when nothing is outside the range.
    pub beyond: String,
    /// Radius in px of the configured-radius ring; 0 when it is not drawn.
    pub radius_ring_px: i32,
    pub blips: Vec<Blip>,
    /// Focus aircraft: callsign or ICAO, an em dash without one.
    pub title: String,
    pub badge: Badge,
    pub alt: String,
    pub gs: String,
    pub trk: String,
    pub dist: String,
}

impl Default for Ui {
    fn default() -> Self {
        let dash = || "\u{2014}".to_string();
        Self {
            wifi_ok: false,
            feed_ok: false,
            counts: String::new(),
            rate: String::new(),
            clock: String::new(),
            kind: CardKind::NoData,
            range_km: radar::DEFAULT_RANGE_KM,
            range_label: String::new(),
            half_label: String::new(),
            beyond: String::new(),
            radius_ring_px: 0,
            blips: Vec::new(),
            title: dash(),
            badge: Badge::None,
            alt: dash(),
            gs: dash(),
            trk: dash(),
            dist: dash(),
        }
    }
}

fn name(a: &AircraftView) -> String {
    match &a.callsign {
        Some(c) if !c.trim().is_empty() => c.trim().to_string(),
        _ => format!("{:06X}", a.icao),
    }
}

fn km(m: f64) -> String {
    if m < 1000.0 {
        format!("{:.0} m", m)
    } else if m < 10_000.0 {
        format!("{:.1} km", m / 1000.0)
    } else {
        format!("{:.0} km", m / 1000.0)
    }
}

/// Tracker card state, with "approaching" when the next aircraft has a CPA inside the radius.
fn kind(snap: &Snapshot) -> CardKind {
    match snap.card {
        Card::NoData => CardKind::NoData,
        Card::Overhead => CardKind::Overhead,
        Card::JustPassed => CardKind::JustPassed,
        Card::Quiet => {
            let next = snap
                .approaching
                .first()
                .and_then(|&i| snap.list.get(i as usize));
            if next.is_some_and(|a| a.position.is_some_and(|p| p.approach.is_some())) {
                CardKind::Approaching
            } else {
                CardKind::Quiet
            }
        }
    }
}

/// Info panel values of the focus aircraft (the dash placeholders stay when it has none).
fn panel(snap: &Snapshot, ui: &mut Ui) {
    let Some((i, a)) = snap.focus.and_then(|i| Some((i, snap.list.get(i)?))) else {
        return;
    };
    ui.title = name(a);
    let arrow = match a.vertical_rate_fpm {
        Some(v) if v > 300 => " \u{2191}",
        Some(v) if v < -300 => " \u{2193}",
        _ => "",
    };
    if let Some(ft) = a.altitude_ft {
        ui.alt = format!("FL{:03} \u{b7} {} ft{}", ft.max(0) / 100, ft, arrow);
    }
    if let Some(v) = a.ground_speed_kt {
        ui.gs = format!("{:.0} kt", v);
    }
    if let Some(t) = a.track_deg {
        ui.trk = format!("{:03.0}\u{b0}", t);
    }
    if let Some(p) = &a.position {
        let mark = match p.marker {
            Marker::Exact => "",
            Marker::Approx => "~",
            Marker::Unknown => "?",
        };
        let up = p
            .elevation_deg
            .map(|e| format!(" \u{b7} {:.0}\u{b0} up", e))
            .unwrap_or_default();
        ui.dist = format!(
            "{mark}{} {}{up}",
            km(p.ground_m),
            compass_label(p.bearing_deg)
        );
        ui.badge = if p.in_radius {
            Badge::Overhead
        } else if snap.approaching.contains(&(i as u8)) {
            Badge::Approaching
        } else {
            Badge::Nearest
        };
    }
}

struct Rate {
    since_ms: u64,
    since_n: u32,
    per_s: u32,
}

/// Slint images for the radar sprites, made on first use: index = sprite + 25 * dim + 50 * flag.
struct Sprites(Vec<Option<Image>>);

impl Sprites {
    const PER_STYLE: usize = radar::SPRITES as usize + 1;

    fn new() -> Self {
        Self(alloc::vec![None; 4 * Self::PER_STYLE])
    }

    fn get(&mut self, b: &Blip) -> Image {
        let i = usize::from(b.sprite)
            + Self::PER_STYLE * usize::from(b.dim)
            + 2 * Self::PER_STYLE * usize::from(b.flag);
        self.0[i]
            .get_or_insert_with(|| {
                let angle = (b.sprite < radar::SPRITES).then(|| f32::from(b.sprite) * 15.0);
                let px = radar::sprite(angle, b.dim, b.flag);
                Image::from_rgba8(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
                    &px,
                    radar::SPRITE_PX as u32,
                    radar::SPRITE_PX as u32,
                ))
            })
            .clone()
    }
}

/// View-model plus the Slint window. Owns the last values pushed to Slint.
pub struct App {
    window: Rc<MinimalSoftwareWindow>,
    ui: AppWindow,
    shown: Ui,
    rate: Rate,
    range: RangeState,
    blips: Rc<VecModel<ui::Blip>>,
    sprites: Sprites,
}

struct SlintPlatform {
    window: Rc<MinimalSoftwareWindow>,
    now: fn() -> Duration,
}

impl Platform for SlintPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> Duration {
        (self.now)()
    }
}

impl App {
    /// Installs the Slint platform (once per process) and creates the 800x480 window. `now` is the
    /// monotonic clock Slint animations run on.
    pub fn new(now: fn() -> Duration) -> Result<Self, PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        slint::platform::set_platform(Box::new(SlintPlatform {
            window: window.clone(),
            now,
        }))
        .map_err(|_| PlatformError::Other("platform already set".into()))?;
        window.set_size(PhysicalSize::new(WIDTH as u32, HEIGHT as u32));
        let ui = AppWindow::new()?;
        let mut sprites = Sprites::new();
        // Dim north-up plane for the "no photo" placeholder.
        ui.set_plane_icon(sprites.get(&Blip {
            x: 0,
            y: 0,
            sprite: 0,
            label: Default::default(),
            focus: false,
            dim: true,
            flag: false,
        }));
        let blips = Rc::new(VecModel::default());
        ui.set_blips(ModelRc::from(blips.clone()));
        ui.show()?;
        Ok(Self {
            window,
            ui,
            shown: Ui::default(),
            rate: Rate {
                since_ms: 0,
                since_n: 0,
                per_s: 0,
            },
            range: RangeState::default(),
            blips,
            sprites,
        })
    }

    /// The values last pushed to Slint.
    pub fn ui(&self) -> &Ui {
        &self.shown
    }

    /// Back to the start-up state (rate and range memory zeroed) for a restarted feed; the window
    /// and platform are kept. Slint is returned to its initial properties and a full repaint is
    /// forced (partial repaints blend anti-aliased edges over stale pixels, so the output would
    /// depend on history), so the next `update` renders exactly what a new `App` would.
    pub fn reset(&mut self) -> Result<(), PlatformError> {
        self.rate = Rate {
            since_ms: 0,
            since_n: 0,
            per_s: 0,
        };
        self.range = RangeState::default();
        self.apply(Ui::default());
        self.ui.hide()?;
        self.ui.show()
    }

    /// Recompute the view from the latest snapshot and push only what changed to Slint.
    /// Returns whether anything changed. `_meta` is the focus aircraft's metadata (RD.7).
    pub fn update(&mut self, snap: &Snapshot, st: &Status, _meta: Option<&FocusMeta>) -> bool {
        let n = snap.counters.messages;
        let dt = st.now_ms.saturating_sub(self.rate.since_ms);
        if dt >= 1000 {
            self.rate.per_s = (u64::from(n.wrapping_sub(self.rate.since_n)) * 1000 / dt) as u32;
            self.rate.since_ms = st.now_ms;
            self.rate.since_n = n;
        }
        let range = radar::select_range(snap, st.now_ms, &mut self.range);
        let mut new = Ui {
            wifi_ok: st.wifi_ok,
            feed_ok: st.feed_ok,
            counts: format!("{} / {}", snap.list.len(), snap.in_radius),
            rate: format!("{} msg/s", self.rate.per_s),
            clock: {
                let s = st.now_ms / 1000;
                format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
            },
            range_km: range,
            range_label: format!("{range} km"),
            // Steps are 10/25/50/100: only 25 has a half step with a fraction.
            half_label: format!("{}", f64::from(range) / 2.0),
            ..Ui::default()
        };
        if st.feed_ok {
            new.kind = kind(snap);
        }
        if new.kind != CardKind::NoData {
            let ring = st.radius_km / f64::from(range) * radar::RING_PX;
            if ring < radar::RING_PX / 2.0 {
                new.radius_ring_px = (ring + 0.5) as i32;
            }
            new.blips = radar::blips(snap, range).into_iter().collect();
            let beyond = radar::beyond(snap, range);
            if beyond > 0 {
                new.beyond = format!("+{beyond} beyond");
            }
            panel(snap, &mut new);
        }
        self.apply(new)
    }

    fn apply(&mut self, new: Ui) -> bool {
        let (ui, old) = (&self.ui, &self.shown);
        macro_rules! diff {
            ($($f:ident => $set:ident),*) => {
                $(if old.$f != new.$f { ui.$set(new.$f.clone().into()); })*
            };
        }
        diff!(wifi_ok => set_wifi_ok, feed_ok => set_feed_ok, counts => set_counts,
            rate => set_rate, clock => set_clock, kind => set_kind, title => set_heading,
            badge => set_badge, alt => set_alt, gs => set_gs, trk => set_trk, dist => set_dist,
            range_label => set_range_label, half_label => set_half_label,
            beyond => set_beyond);
        if old.radius_ring_px != new.radius_ring_px {
            ui.set_radius_ring(new.radius_ring_px as f32);
        }
        if old.blips != new.blips {
            for (i, b) in new.blips.iter().enumerate() {
                if old.blips.get(i) == Some(b) {
                    continue;
                }
                let row = ui::Blip {
                    x: b.x as f32,
                    y: b.y as f32,
                    sprite: self.sprites.get(b),
                    label: b.label.as_str().into(),
                    focus: b.focus,
                };
                if i < self.blips.row_count() {
                    self.blips.set_row_data(i, row);
                } else {
                    self.blips.push(row);
                }
            }
            while self.blips.row_count() > new.blips.len() {
                self.blips.remove(self.blips.row_count() - 1);
            }
        }
        let changed = *old != new;
        self.shown = new;
        changed
    }

    /// Feed a touch/mouse event to the UI.
    pub fn pointer(&self, ev: PointerEvent) {
        let position = LogicalPosition::new(ev.x, ev.y);
        let button = PointerEventButton::Left;
        self.window.dispatch_event(match ev.phase {
            PointerPhase::Down => WindowEvent::PointerPressed { position, button },
            PointerPhase::Move => WindowEvent::PointerMoved { position },
            PointerPhase::Up => WindowEvent::PointerReleased { position, button },
        });
    }

    /// Advance timers/animations, then render whatever is dirty; see [`render_by_line`].
    pub fn render(
        &self,
        line_buf: &mut [Rgb565Pixel],
        sink: impl FnMut(usize, Range<usize>, &[Rgb565Pixel]),
    ) -> bool {
        slint::platform::update_timers_and_animations();
        render_by_line(&self.window, line_buf, sink)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use adsb_core::pipeline::FeedPipeline;
    use adsb_core::scenario::{RX_LAT_DEG, RX_LON_DEG};
    use adsb_core::tracker::{Config, Counters, PositionView};

    fn view(icao: u32, ground_m: f64, in_radius: bool) -> AircraftView {
        AircraftView {
            icao,
            callsign: None,
            category: None,
            squawk: None,
            altitude_ft: Some(35_000),
            vertical_rate_fpm: None,
            ground_speed_kt: Some(450.0),
            track_deg: Some(90.0),
            last_seen_age_ms: 0,
            position: Some(PositionView {
                ground_m,
                bearing_deg: 10.0,
                height_m: None,
                elevation_deg: None,
                marker: Marker::Exact,
                provisional: false,
                age_ms: 0,
                in_radius,
                approach: None,
            }),
        }
    }

    // One test only: the Slint platform can be installed once per process.
    #[test]
    fn updates_are_diffed_and_panel_follows_focus() {
        let mut app = App::new(|| Duration::ZERO).unwrap();
        let mut p = FeedPipeline::new(Config {
            rx_lat_deg: RX_LAT_DEG,
            rx_lon_deg: RX_LON_DEG,
            rx_elev_m: 0.0,
        });
        let st = Status {
            now_ms: 5000,
            wifi_ok: true,
            feed_ok: true,
            radius_km: 5.0,
        };
        let snap = p.snapshot(st.now_ms, st.radius_km);
        assert!(app.update(&snap, &st, None));
        assert!(
            !app.update(&snap, &st, None),
            "identical input must not change anything"
        );
        assert_eq!(app.ui().kind, CardKind::NoData);
        assert_eq!(app.ui().range_label, "25 km");

        // Focus is the far aircraft's neighbour, not `primary`: the panel follows `focus`.
        let mut snap = Box::new(Snapshot {
            card: Card::Quiet,
            list: Default::default(),
            in_radius: 0,
            primary: None,
            focus: Some(1),
            just_passed: None,
            approaching: Default::default(),
            counters: Counters::default(),
        });
        snap.list.push(view(0x111111, 3000.0, false)).unwrap();
        snap.list.push(view(0xABCDEF, 20_000.0, false)).unwrap();
        assert!(app.update(&snap, &st, None));
        let ui = app.ui();
        assert_eq!(ui.title, "ABCDEF");
        assert_eq!(ui.badge, Badge::Nearest);
        assert_eq!(ui.alt, "FL350 \u{b7} 35000 ft");
        assert_eq!(ui.dist, "20 km N");
        assert_eq!((ui.range_km, ui.blips.len()), (25, 2));
        assert_eq!(ui.radius_ring_px, 42); // 5 km of 25 km
        assert_eq!(app.blips.row_count(), 2);

        // No focus blip (focus outside the range): panel still shows it, radar has no ring.
        snap.list[1].position.as_mut().unwrap().ground_m = 60_000.0;
        snap.list[0].position.as_mut().unwrap().ground_m = 60_000.0;
        app.update(&snap, &st, None);
        assert_eq!(app.ui().range_km, 100);
        snap.list.pop();
        snap.focus = None;
        app.update(&snap, &st, None);
        assert_eq!(app.ui().title, "\u{2014}");
        assert_eq!(app.blips.row_count(), 1);
        assert_eq!(app.ui().beyond, "");
    }
}
