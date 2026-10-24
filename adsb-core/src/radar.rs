//! Radar screen logic: range selection, projection, blip list and sprite rasteriser.
//!
//! Pure functions on a [`Snapshot`]; pixel values are relative to the top-left of the
//! 448x448 radar box.

use heapless::{String, Vec};

use crate::tracker::{Marker, Snapshot};

/// Radar range in km; always one of [`RANGE_STEPS_KM`].
pub type RangeKm = u32;
pub const RANGE_STEPS_KM: [RangeKm; 4] = [10, 25, 50, 100];
pub const DEFAULT_RANGE_KM: RangeKm = 25;
/// The n-th nearest aircraft decides the range (n = min(this, count)).
const RANGE_RANK: usize = 10;
/// Range must be at least this factor beyond that aircraft.
const RANGE_MARGIN: f64 = 1.1;
/// A smaller step must hold this long before the range shrinks.
pub const STEP_DOWN_MS: u64 = 15_000;

pub const RADAR_SIZE_PX: i32 = 448;
pub const CENTRE_PX: i32 = 224;
/// Physical pixel width / height of the 800x480 panel: (154.08 / 800) / (85.92 / 480) from the usual 7"
/// active area (radar-ui RD.14; the bench border check of RD.14.1 agrees, the user chose not to ruler-measure).
pub const PIXEL_ASPECT: f64 = 1.076;
/// Outer ring radius.
pub const RING_PX: f64 = 208.0;
pub const MAX_BLIPS: usize = 48;
/// Only the nearest this many aircraft (and the focus) get a label.
pub const LABELLED: usize = 12;
pub const SPRITE_PX: usize = 20;
/// Number of rotated plane sprites; the round "no track" sprite is index `SPRITES`.
pub const SPRITES: u8 = 24;
pub type Sprite = [u8; SPRITE_PX * SPRITE_PX * 4];

/// Hysteresis memory for [`select_range`].
#[derive(Clone, Copy, Debug, Default)]
pub struct RangeState {
    current: Option<RangeKm>,
    /// A smaller step that has been valid since the given time.
    shrink: Option<(RangeKm, u64)>,
}

fn wanted_range(s: &Snapshot) -> RangeKm {
    let positioned = s.list.iter().take_while(|v| v.position.is_some()).count();
    if positioned == 0 {
        return DEFAULT_RANGE_KM;
    }
    let d = s.list[positioned.min(RANGE_RANK) - 1]
        .position
        .map_or(0.0, |p| p.ground_m);
    RANGE_STEPS_KM
        .into_iter()
        .find(|&k| RANGE_MARGIN * d <= k as f64 * 1000.0)
        .unwrap_or(RANGE_STEPS_KM[RANGE_STEPS_KM.len() - 1])
}

/// Smallest step that fits the 10th nearest aircraft. Steps up at once, down only after the
/// smaller step was valid for [`STEP_DOWN_MS`] without a break. The first call takes the
/// wanted step directly.
pub fn select_range(s: &Snapshot, now_ms: u64, st: &mut RangeState) -> RangeKm {
    let wanted = wanted_range(s);
    let cur = st.current.unwrap_or(wanted);
    let next = if wanted >= cur {
        st.shrink = None;
        wanted
    } else {
        // `p` is the largest step that stayed sufficient since `since`.
        let (p, since) = match st.shrink {
            Some((p, since)) if wanted <= p => (p, since),
            _ => (wanted, now_ms),
        };
        if now_ms.saturating_sub(since) >= STEP_DOWN_MS {
            st.shrink = None;
            p
        } else {
            st.shrink = Some((p, since));
            cur
        }
    };
    st.current = Some(next);
    next
}

/// North-up azimuthal projection to radar-box pixels (y grows downwards).
pub fn project(ground_m: f64, bearing_deg: f64, range: RangeKm) -> (i32, i32) {
    let k = RING_PX / (range as f64 * 1000.0);
    let b = bearing_deg.to_radians();
    let r = ground_m * k;
    (
        CENTRE_PX + libm::round(r * libm::sin(b)) as i32,
        CENTRE_PX - libm::round(r * libm::cos(b)) as i32,
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blip {
    pub x: i32,
    pub y: i32,
    /// `0..SPRITES` plane rotated by `15 * sprite` degrees, or `SPRITES` for the round sprite.
    pub sprite: u8,
    /// Callsign, else ICAO24 in hex; empty when the aircraft is not labelled.
    pub label: String<8>,
    pub focus: bool,
    /// Position older than the dead-reckoning horizon.
    pub dim: bool,
    /// Provisional position (amber outline).
    pub flag: bool,
}

pub fn sprite_index(track_deg: Option<f32>) -> u8 {
    track_deg.map_or(SPRITES, |t| {
        (libm::roundf(t / 15.0) as i32).rem_euclid(SPRITES as i32) as u8
    })
}

/// Blips for the positioned aircraft inside `range`, nearest first, at most [`MAX_BLIPS`].
/// Use [`beyond`] for the "+k beyond" count.
pub fn blips(s: &Snapshot, range: RangeKm) -> Vec<Blip, MAX_BLIPS> {
    let mut out = Vec::new();
    for (i, v) in s.list.iter().enumerate() {
        let Some(p) = v.position else { break };
        if p.ground_m > range as f64 * 1000.0 {
            break; // sorted by distance
        }
        let focus = s.focus == Some(i);
        let mut label = String::new();
        if focus || i < LABELLED {
            match &v.callsign {
                Some(c) if !c.trim().is_empty() => {
                    let _ = label.push_str(c.trim());
                }
                _ => {
                    let _ =
                        core::fmt::write(&mut label, format_args!("{:06X}", v.icao & 0xFF_FFFF));
                }
            }
        }
        let (x, y) = project(p.ground_m, p.bearing_deg, range);
        let blip = Blip {
            x,
            y,
            sprite: sprite_index(v.track_deg),
            label,
            focus,
            dim: p.marker == Marker::Unknown,
            flag: p.provisional,
        };
        if out.push(blip).is_err() {
            break;
        }
    }
    out
}

/// Positioned aircraft outside `range`, plus those inside it beyond the blip limit.
pub fn beyond(s: &Snapshot, range: RangeKm) -> usize {
    let positioned = s.list.iter().filter(|v| v.position.is_some()).count();
    let drawn = s
        .list
        .iter()
        .take_while(|v| {
            v.position
                .is_some_and(|p| p.ground_m <= range as f64 * 1000.0)
        })
        .count()
        .min(MAX_BLIPS);
    positioned - drawn
}

/// Plane outline, nose up, centre at the origin, y down. Right half; mirrored by `in_plane`.
const HALF: [(f32, f32); 9] = [
    (0.0, -9.5),
    (1.5, -4.0),
    (8.5, 2.0),
    (8.5, 4.0),
    (1.5, 2.5),
    (1.0, 7.0),
    (4.0, 8.5),
    (4.0, 9.5),
    (0.0, 8.5),
];
const ROUND_RADIUS: f32 = 6.0;
/// The outline of a flagged sprite is the part outside this fraction of the shape.
const INNER: f32 = 0.6;
const SUBSAMPLES: usize = 4;
const FILL: [u32; 3] = [80, 220, 255];
const FILL_DIM: [u32; 3] = [110, 120, 130];
const AMBER: [u32; 3] = [255, 180, 0];

/// Even-odd test against the mirrored outline.
fn in_plane(x: f32, y: f32) -> bool {
    let n = HALF.len();
    let mut inside = false;
    // Walk the closed polygon: HALF[0..n], then HALF[n-2..=1] mirrored.
    let pt = |k: usize| {
        if k < n {
            HALF[k]
        } else {
            let (hx, hy) = HALF[2 * n - 2 - k];
            (-hx, hy)
        }
    };
    let total = 2 * n - 2;
    for k in 0..total {
        let ((x0, y0), (x1, y1)) = (pt(k), pt((k + 1) % total));
        if (y0 > y) != (y1 > y) && x < x0 + (y - y0) / (y1 - y0) * (x1 - x0) {
            inside = !inside;
        }
    }
    inside
}

/// RGBA (straight alpha) sprite, anti-aliased by 4x4 supersampling. `angle_deg` turns the
/// plane clockwise from north-up; `None` gives the round "no track" sprite.
pub fn sprite(angle_deg: Option<f32>, dim: bool, flag: bool) -> Sprite {
    let (sin, cos) = angle_deg.map_or((0.0, 1.0), |a| {
        let r = a.to_radians();
        (libm::sinf(r), libm::cosf(r))
    });
    let fill = if dim { FILL_DIM } else { FILL };
    let shape = |x: f32, y: f32| match angle_deg {
        Some(_) => in_plane(x, y),
        None => x * x + y * y <= ROUND_RADIUS * ROUND_RADIUS,
    };
    let mut out = [0u8; SPRITE_PX * SPRITE_PX * 4];
    let half = SPRITE_PX as f32 / 2.0;
    for py in 0..SPRITE_PX {
        for px in 0..SPRITE_PX {
            let (mut n, mut rgb) = (0u32, [0u32; 3]);
            for sy in 0..SUBSAMPLES {
                for sx in 0..SUBSAMPLES {
                    let step = 1.0 / SUBSAMPLES as f32;
                    let (dx, dy) = (
                        px as f32 + (sx as f32 + 0.5) * step - half,
                        py as f32 + (sy as f32 + 0.5) * step - half,
                    );
                    // Inverse rotation into the nose-up frame.
                    let (x, y) = (dx * cos + dy * sin, -dx * sin + dy * cos);
                    if !shape(x, y) {
                        continue;
                    }
                    let colour = if flag && !shape(x / INNER, y / INNER) {
                        AMBER
                    } else {
                        fill
                    };
                    n += 1;
                    for c in 0..3 {
                        rgb[c] += colour[c];
                    }
                }
            }
            let o = (py * SPRITE_PX + px) * 4;
            for c in 0..3 {
                out[o + c] = rgb[c].checked_div(n).unwrap_or(0) as u8;
            }
            out[o + 3] = (n * 255 / (SUBSAMPLES * SUBSAMPLES) as u32) as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::{AircraftView, Card, Counters, PositionView};

    fn view(icao: u32, ground_m: f64) -> AircraftView {
        AircraftView {
            icao,
            callsign: None,
            category: None,
            squawk: None,
            altitude_ft: None,
            vertical_rate_fpm: None,
            ground_speed_kt: None,
            track_deg: None,
            last_seen_age_ms: 0,
            position: Some(PositionView {
                ground_m,
                bearing_deg: 0.0,
                height_m: None,
                elevation_deg: None,
                marker: Marker::Exact,
                provisional: false,
                age_ms: 0,
                in_radius: false,
                approach: None,
            }),
        }
    }

    /// Distances in metres, nearest first.
    fn snap(dist: &[f64]) -> Snapshot {
        let mut list = Vec::new();
        for (i, d) in dist.iter().enumerate() {
            list.push(view(0xA00000 + i as u32, *d)).unwrap();
        }
        Snapshot {
            card: Card::Quiet,
            list,
            in_radius: 0,
            primary: None,
            focus: (!dist.is_empty()).then_some(0),
            just_passed: None,
            approaching: Vec::new(),
            counters: Counters::default(),
        }
    }

    fn range(dist: &[f64]) -> RangeKm {
        select_range(&snap(dist), 0, &mut RangeState::default())
    }

    #[test]
    fn range_steps_and_boundaries() {
        assert_eq!(range(&[]), 25);
        assert_eq!(range(&[8000.0; 10]), 10);
        assert_eq!(range(&[9090.0; 10]), 10); // 1.1 * 9090 = 9999
        assert_eq!(range(&[9100.0; 10]), 25);
        let mut d = vec![1000.0; 9];
        d.push(30_000.0);
        assert_eq!(range(&d), 50);
        assert_eq!(range(&[22_700.0; 10]), 25);
        assert_eq!(range(&[22_800.0; 10]), 50);
        assert_eq!(range(&[90_909.0; 10]), 100);
        assert_eq!(range(&[200_000.0; 10]), 100);
        // Fewer than 10 aircraft: the farthest one decides.
        assert_eq!(range(&[1000.0, 20_000.0]), 25);
        // Only the 10th counts; farther ones do not.
        let mut d = vec![1000.0; 10];
        d.push(80_000.0);
        assert_eq!(range(&d), 10);
    }

    #[test]
    fn range_up_at_once_down_after_15_s() {
        let mut st = RangeState::default();
        let near = snap(&[5000.0; 10]);
        let far = snap(&[30_000.0; 10]);
        assert_eq!(select_range(&near, 0, &mut st), 10);
        assert_eq!(select_range(&far, 1000, &mut st), 50);
        assert_eq!(select_range(&near, 2000, &mut st), 50);
        assert_eq!(select_range(&near, 16_999, &mut st), 50);
        assert_eq!(select_range(&near, 17_000, &mut st), 10);
        // A break restarts the timer.
        assert_eq!(select_range(&far, 18_000, &mut st), 50);
        assert_eq!(select_range(&near, 19_000, &mut st), 50);
        assert_eq!(select_range(&far, 30_000, &mut st), 50);
        assert_eq!(select_range(&near, 31_000, &mut st), 50);
        assert_eq!(select_range(&near, 45_999, &mut st), 50);
        assert_eq!(select_range(&near, 46_000, &mut st), 10);
    }

    #[test]
    fn range_steps_down_via_larger_valid_step() {
        let mut st = RangeState::default();
        assert_eq!(select_range(&snap(&[60_000.0; 10]), 0, &mut st), 100);
        let mid = snap(&[20_000.0; 10]); // wants 25
        assert_eq!(select_range(&mid, 1000, &mut st), 100);
        assert_eq!(select_range(&snap(&[5000.0; 10]), 10_000, &mut st), 100);
        assert_eq!(select_range(&mid, 16_000, &mut st), 25); // 25 held since 1 s
    }

    #[test]
    fn projection_axes() {
        let near = |a: (i32, i32), b: (i32, i32)| {
            assert!(
                (a.0 - b.0).abs() <= 1 && (a.1 - b.1).abs() <= 1,
                "{a:?} vs {b:?}"
            );
        };
        near(project(25_000.0, 0.0, 25), (224, 16));
        near(project(25_000.0, 90.0, 25), (432, 224));
        near(project(25_000.0, 180.0, 25), (224, 432));
        near(project(25_000.0, 270.0, 25), (16, 224));
        assert_eq!(project(0.0, 123.0, 25), (224, 224));
    }

    #[test]
    fn blip_labels_and_styles() {
        let mut d = Vec::<f64, 20>::new();
        for i in 0..20 {
            d.push(1000.0 + i as f64 * 100.0).unwrap();
        }
        let mut s = snap(&d);
        s.list[0].callsign = Some(String::try_from("DLH12  ").unwrap());
        s.list[1].callsign = Some(String::try_from("   ").unwrap());
        s.list[2].track_deg = Some(97.0);
        s.list[3].position.as_mut().unwrap().marker = Marker::Unknown;
        s.list[4].position.as_mut().unwrap().provisional = true;
        s.list[3].track_deg = Some(359.0);
        s.focus = Some(15);
        let b = blips(&s, 10);
        assert_eq!(b.len(), 20);
        assert_eq!(b[0].label.as_str(), "DLH12");
        assert_eq!(b[1].label.as_str(), "A00001"); // blank callsign falls back to ICAO
        assert_eq!(b.iter().filter(|x| !x.label.is_empty()).count(), 13);
        assert!(b[11].label.as_str() == "A0000B" && b[12].label.is_empty());
        assert!(b[15].focus && !b[15].label.is_empty());
        assert_eq!(b.iter().filter(|x| x.focus).count(), 1);
        assert_eq!((b[0].sprite, b[2].sprite, b[3].sprite), (SPRITES, 6, 0)); // 97/15 -> 6; 359 -> 24 mod 24
        assert!(b[3].dim && !b[2].dim && b[4].flag && !b[3].flag);
    }

    #[test]
    fn blips_limits_and_beyond() {
        let d: Vec<f64, 60> = (0..60).map(|i| 1000.0 + i as f64 * 100.0).collect();
        let s = snap(&d);
        assert_eq!(blips(&s, 10).len(), MAX_BLIPS);
        assert_eq!(beyond(&s, 10), 12);
        let s = snap(&[1000.0, 5000.0, 20_000.0, 60_000.0]);
        assert_eq!(blips(&s, 25).len(), 3);
        assert_eq!(beyond(&s, 25), 1);
        assert_eq!(blips(&snap(&[]), 25).len(), 0);
    }

    fn alpha(sp: &Sprite, x: usize, y: usize) -> u8 {
        sp[(y * SPRITE_PX + x) * 4 + 3]
    }

    #[test]
    fn sprite_orientation() {
        let n = sprite(Some(0.0), false, false);
        assert!(alpha(&n, 10, 1) > 0 && alpha(&n, 10, 18) > 0);
        let e = sprite(Some(90.0), false, false);
        assert!(alpha(&e, 18, 10) > 0 || alpha(&e, 19, 10) > 0);
        // Nose is the farthest covered pixel: top for 0 deg, right for 90 deg.
        let top = |s: &Sprite| (0..SPRITE_PX).find(|&y| (0..SPRITE_PX).any(|x| alpha(s, x, y) > 0));
        let right = |s: &Sprite| {
            (0..SPRITE_PX)
                .rev()
                .find(|&x| (0..SPRITE_PX).any(|y| alpha(s, x, y) > 0))
        };
        assert!(top(&n) <= Some(1)); // the nose tip is thin
        assert!(right(&e) >= Some(SPRITE_PX - 2));
        assert!(alpha(&n, 0, 0) == 0 && alpha(&n, 19, 19) == 0);
    }

    #[test]
    fn sprites_distinct_and_styled() {
        let all: std::vec::Vec<Sprite> = (0..SPRITES)
            .map(|i| sprite(Some(i as f32 * 15.0), false, false))
            .collect();
        for i in 0..all.len() {
            for j in i + 1..all.len() {
                assert_ne!(all[i], all[j], "{i} vs {j}");
            }
        }
        let round = sprite(None, false, false);
        assert!(all.iter().all(|s| *s != round));
        assert!(alpha(&round, 10, 10) == 255 && alpha(&round, 0, 0) == 0);
        let rgb = |s: &Sprite, x, y| {
            let o = (y * SPRITE_PX + x) * 4;
            [s[o], s[o + 1], s[o + 2]]
        };
        assert_ne!(rgb(&sprite(None, true, false), 10, 10), rgb(&round, 10, 10));
        // Flagged: amber on the rim, normal fill inside.
        let f = sprite(None, false, true);
        assert_eq!(rgb(&f, 10, 10), [80, 220, 255]);
        assert_eq!(rgb(&f, 10, 5), [255, 180, 0]);
    }
}
