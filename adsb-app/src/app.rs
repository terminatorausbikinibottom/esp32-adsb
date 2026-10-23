use crate::{CardKind, HEIGHT, Screen, WIDTH, render_by_line, ui::AppWindow};
use adsb_core::geo::compass_label;
use adsb_core::tracker::{AircraftView, Card, Marker, Snapshot};
use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use core::cell::Cell;
use core::ops::Range;
use core::time::Duration;
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType, Rgb565Pixel};
use slint::platform::{Platform, PointerEventButton, WindowAdapter, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, PhysicalSize, PlatformError};

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
    /// False forces the "No data" card (feed known to be down), whatever the tracker says.
    pub feed_ok: bool,
    pub radius_km: f64,
}

/// Everything the Slint window displays besides the navigation state. Diffed field by field.
#[derive(Clone, Debug, PartialEq)]
pub struct Ui {
    pub wifi_ok: bool,
    pub feed_ok: bool,
    pub counts: String,
    pub rate: String,
    pub clock: String,
    pub kind: CardKind,
    pub title: String,
    pub line1: String,
    pub line2: String,
    pub line3: String,
    pub more: String,
    pub dim: bool,
    /// Position decoded against the receiver only (`PositionView::provisional`).
    pub flagged: bool,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            wifi_ok: false,
            feed_ok: false,
            counts: String::new(),
            rate: String::new(),
            clock: String::new(),
            kind: CardKind::NoData,
            title: String::new(),
            line1: String::new(),
            line2: String::new(),
            line3: String::new(),
            more: String::new(),
            dim: false,
            flagged: false,
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

fn secs(s: f64) -> String {
    if s < 90.0 {
        format!("{:.0} s", s)
    } else {
        format!("{:.0} min", s / 60.0)
    }
}

/// "7.3 km NE" for an aircraft with a position.
fn dist_dir(a: &AircraftView) -> Option<String> {
    let p = a.position.as_ref()?;
    Some(format!(
        "{} {}",
        km(p.ground_m),
        compass_label(p.bearing_deg)
    ))
}

/// The card part of the UI for the tracker's current state. Layout details of the full card
/// (route, airline, chips) arrive with step 4.3.
fn card(snap: &Snapshot, radius_km: f64, ui: &mut Ui) {
    match snap.card {
        Card::NoData => card_no_data(ui),
        Card::Overhead => {
            ui.kind = CardKind::Overhead;
            let Some(a) = snap.primary.and_then(|i| snap.list.get(i)) else {
                return;
            };
            ui.title = name(a);
            let arrow = match a.vertical_rate_fpm {
                Some(v) if v > 300 => " \u{2191}",
                Some(v) if v < -300 => " \u{2193}",
                _ => "",
            };
            ui.line1 = match a.altitude_ft {
                Some(ft) => format!("FL{:03} \u{b7} {} ft{}", ft.max(0) / 100, ft, arrow),
                None => "altitude unknown".into(),
            };
            let gs = a.ground_speed_kt.map(|v| format!("{:.0} kt", v));
            let trk = a.track_deg.map(|t| format!("track {:03.0}\u{b0}", t));
            ui.line2 = [gs, trk]
                .into_iter()
                .flatten()
                .collect::<alloc::vec::Vec<_>>()
                .join(" \u{b7} ");
            if let Some(p) = &a.position {
                let mark = match p.marker {
                    Marker::Exact => "",
                    Marker::Approx => "~",
                    Marker::Unknown => "?",
                };
                let up = p
                    .elevation_deg
                    .map(|e| format!(", {:.0}\u{b0} up", e))
                    .unwrap_or_default();
                ui.line3 = format!("{mark}{}{up}", dist_dir(a).unwrap_or_default());
                ui.flagged = p.provisional;
            }
            let n = snap.others().count();
            if n > 0 {
                ui.more = format!("+{n} more");
            }
        }
        Card::JustPassed => {
            ui.kind = CardKind::JustPassed;
            ui.dim = true;
            let Some(jp) = snap.just_passed else { return };
            if let Some(a) = snap.list.get(jp.index) {
                ui.title = name(a);
                ui.line2 = dist_dir(a).map(|d| format!("now {d}")).unwrap_or_default();
            }
            ui.line1 = format!(
                "Left the {radius_km} km zone {} ago",
                secs(jp.left_ago_ms as f64 / 1000.0)
            );
        }
        Card::Quiet => {
            ui.kind = CardKind::Quiet;
            ui.title = format!("Nothing within {radius_km} km");
            let next = snap
                .approaching
                .first()
                .and_then(|&i| snap.list.get(i as usize));
            if let Some((a, c)) = next.and_then(|a| Some((a, a.position.as_ref()?.approach?))) {
                ui.kind = CardKind::Approaching;
                ui.line1 = format!(
                    "Approaching: closest {} in ~{}",
                    km(c.d_m),
                    secs(c.t_s.max(0.0))
                );
                ui.line2 = format!("{} {}", name(a), dist_dir(a).unwrap_or_default());
                ui.flagged = a.position.is_some_and(|p| p.provisional);
            } else if let Some(a) = snap.nearest_beyond() {
                ui.dim = true;
                ui.line1 = format!("Nearest: {} {}", name(a), dist_dir(a).unwrap_or_default());
                ui.flagged = a.position.is_some_and(|p| p.provisional);
            } else {
                ui.dim = true;
                ui.line1 = "No positioned aircraft".into();
            }
        }
    }
}

struct Rate {
    since_ms: u64,
    since_n: u32,
    per_s: u32,
}

/// View-model plus the Slint window. Owns navigation state and the last values pushed to Slint.
pub struct App {
    window: Rc<MinimalSoftwareWindow>,
    ui: AppWindow,
    screen: Rc<Cell<Screen>>,
    shown: Ui,
    rate: Rate,
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
        let screen = Rc::new(Cell::new(Screen::Card));
        let s = screen.clone();
        let weak = ui.as_weak();
        ui.on_navigate(move |to| {
            s.set(to);
            if let Some(ui) = weak.upgrade() {
                ui.set_screen(to);
            }
        });
        ui.show()?;
        Ok(Self {
            window,
            ui,
            screen,
            shown: Ui::default(),
            rate: Rate {
                since_ms: 0,
                since_n: 0,
                per_s: 0,
            },
        })
    }

    /// The values last pushed to Slint.
    pub fn ui(&self) -> &Ui {
        &self.shown
    }

    pub fn screen(&self) -> Screen {
        self.screen.get()
    }

    /// Back to the start-up state (card screen, rate zeroed) for a restarted feed; the window and
    /// platform are kept. Slint is returned to its initial properties and a full repaint is forced
    /// (partial repaints blend anti-aliased edges over stale pixels, so the output would depend on
    /// history), so the next `update` renders exactly what a new `App` would.
    pub fn reset(&mut self) -> Result<(), PlatformError> {
        self.rate = Rate {
            since_ms: 0,
            since_n: 0,
            per_s: 0,
        };
        self.screen.set(Screen::Card);
        self.ui.set_screen(Screen::Card);
        self.apply(Ui::default());
        self.ui.hide()?;
        self.ui.show()
    }

    /// Recompute the view from the latest snapshot and push only what changed to Slint.
    /// Returns whether anything changed.
    pub fn update(&mut self, snap: &Snapshot, st: &Status) -> bool {
        let n = snap.counters.messages;
        let dt = st.now_ms.saturating_sub(self.rate.since_ms);
        if dt >= 1000 {
            self.rate.per_s = (u64::from(n.wrapping_sub(self.rate.since_n)) * 1000 / dt) as u32;
            self.rate.since_ms = st.now_ms;
            self.rate.since_n = n;
        }
        let mut new = Ui {
            wifi_ok: st.wifi_ok,
            feed_ok: st.feed_ok,
            counts: format!("{} / {}", snap.list.len(), snap.in_radius),
            rate: format!("{} msg/s", self.rate.per_s),
            clock: {
                let s = st.now_ms / 1000;
                format!("{:02}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
            },
            ..Ui::default()
        };
        if st.feed_ok {
            card(snap, st.radius_km, &mut new);
        } else {
            card_no_data(&mut new);
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
            line1 => set_line1, line2 => set_line2, line3 => set_line3, more => set_more,
            dim => set_dim, flagged => set_flagged);
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

fn card_no_data(ui: &mut Ui) {
    ui.kind = CardKind::NoData;
    ui.title = "No data".into();
    ui.line1 = "Feed disconnected or silent".into();
    ui.dim = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use adsb_core::pipeline::FeedPipeline;
    use adsb_core::scenario::{RX_LAT_DEG, RX_LON_DEG};
    use adsb_core::tracker::Config;

    fn tap(app: &App, x: f32, y: f32) {
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            app.pointer(PointerEvent { x, y, phase });
        }
    }

    // One test only: the Slint platform can be installed once per process.
    #[test]
    fn pointer_events_drive_navigation_and_updates_are_diffed() {
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
        assert!(app.update(&snap, &st));
        assert!(
            !app.update(&snap, &st),
            "identical input must not change anything"
        );
        assert_eq!(app.ui().kind, CardKind::NoData);

        assert_eq!(app.screen(), Screen::Card);
        tap(&app, 100.0, 200.0); // empty card area: no navigation
        assert_eq!(app.screen(), Screen::Card);
        tap(&app, 724.0, 440.0); // "List" button
        assert_eq!(app.screen(), Screen::List);
        // Press on the button but release away from it: not a tap.
        app.pointer(PointerEvent {
            x: 724.0,
            y: 440.0,
            phase: PointerPhase::Down,
        });
        app.pointer(PointerEvent {
            x: 100.0,
            y: 100.0,
            phase: PointerPhase::Move,
        });
        app.pointer(PointerEvent {
            x: 100.0,
            y: 100.0,
            phase: PointerPhase::Up,
        });
        assert_eq!(app.screen(), Screen::List);
        tap(&app, 724.0, 440.0); // "Card" button
        assert_eq!(app.screen(), Screen::Card);
    }

    #[test]
    fn quiet_card_flags_provisional_nearest() {
        use adsb_core::tracker::{Counters, PositionView};
        let pos = |provisional| PositionView {
            ground_m: 23_000.0,
            bearing_deg: 10.0,
            height_m: None,
            elevation_deg: None,
            marker: Marker::Exact,
            provisional,
            age_ms: 0,
            in_radius: false,
            approach: None,
        };
        let view = |provisional| AircraftView {
            icao: 0xABCDEF,
            callsign: None,
            category: None,
            squawk: None,
            altitude_ft: None,
            vertical_rate_fpm: None,
            ground_speed_kt: None,
            track_deg: None,
            last_seen_age_ms: 0,
            position: Some(pos(provisional)),
        };
        for provisional in [false, true] {
            let mut snap = Box::new(Snapshot {
                card: Card::Quiet,
                list: Default::default(),
                in_radius: 0,
                primary: None,
                just_passed: None,
                approaching: Default::default(),
                counters: Counters::default(),
            });
            snap.list.push(view(provisional)).unwrap();
            let mut ui = Ui::default();
            card(&snap, 5.0, &mut ui);
            assert_eq!(ui.flagged, provisional);
            assert!(ui.dim);
            assert_eq!(ui.line1, "Nearest: ABCDEF 23 km N");
        }
    }
}
