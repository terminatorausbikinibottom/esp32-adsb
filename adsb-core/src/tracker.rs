//! Aircraft table: ingests decoded `modes::Message`s, resolves CPR positions, applies the
//! known-ICAO filter (R15), ages entries and builds the vicinity `Snapshot`.
//!
//! Clock: everything here, including the 10 s CPR pair window, uses the caller's monotonic
//! `now_ms`. Frame timestamps (`Message::timestamp`) are ignored: their unit and zero point
//! depend on the source (R25).
//!
//! Receiver coordinates come in through [`Config`]; nothing is hardcoded.

use crate::geo::{bearing_deg, distance_m, elevation_deg};
use crate::kinematics::{
    Cpa, DR_HORIZON_MS, Fix, KT_TO_MS, STATIONARY_KT, TimedVelocity, approaching, cpa, predict,
};
use heapless::{String, Vec};
use modes::{
    Address, Body, CprPosition, EmitterCategory, Horizontal, Message, cpr_global, cpr_local,
};

pub const CAPACITY: usize = 128;
/// Entries with no message for this long are removed by [`AircraftTable::age`].
pub const REMOVE_AFTER_MS: u64 = 60_000;
/// A position older than this no longer counts as positioned.
pub const STALE_POSITION_MS: u64 = 30_000;
/// Even/odd CPR frames further apart than this are not paired for a global decode.
pub const CPR_PAIR_WINDOW_MS: u64 = 10_000;
/// A `Recovered` address is accepted only if the aircraft was `Verified` this recently.
pub const KNOWN_ICAO_MS: u64 = 60_000;
/// No decoded message for this long: the card shows "no data".
pub const NO_DATA_AFTER_MS: u64 = 30_000;
/// "Just passed" lasts this long after the primary left the radius.
pub const JUST_PASSED_MS: u64 = 60_000;
/// Positions farther than this from the receiver are rejected (independent of the display radius).
pub const MAX_RANGE_M: f64 = 450_000.0;
/// A new fix implying more than this speed from the previous one is rejected.
pub const MAX_SPEED_MS: f64 = 1000.0 * KT_TO_MS;
/// Allowance on the speed gate for CPR quantisation.
const GATE_SLACK_M: f64 = 500.0;
/// Position age up to which a displayed position is exact; beyond it "~".
const EXACT_AGE_MS: u64 = crate::kinematics::APPROX_AFTER_MS;
/// A rival replaces the primary only if closer by more than this fraction and this many metres.
pub const SWITCH_FRACTION: f64 = 0.20;
pub const SWITCH_MIN_M: f64 = 300.0;
const FT_TO_M: f64 = 0.3048;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub rx_lat_deg: f64,
    pub rx_lon_deg: f64,
    /// Receiver ground elevation, metres above MSL.
    pub rx_elev_m: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Messages passed to `update`.
    pub messages: u32,
    /// Dropped by the known-ICAO filter (R15).
    pub filtered: u32,
    /// Positions rejected by the plausibility gate (R10).
    pub implausible: u32,
}

struct Aircraft {
    icao: u32,
    last_seen_ms: u64,
    verified_ms: u64,
    callsign: Option<String<8>>,
    category: Option<EmitterCategory>,
    squawk: Option<u16>,
    altitude_ft: Option<i32>,
    velocity: Option<TimedVelocity>,
    fix: Option<Fix>,
    /// The fix came from a local decode against the receiver only (may be aliased, R10).
    provisional: bool,
    even: Option<(CprPosition, u64)>,
    odd: Option<(CprPosition, u64)>,
}

impl Aircraft {
    fn new(icao: u32, now_ms: u64) -> Self {
        Aircraft {
            icao,
            last_seen_ms: now_ms,
            verified_ms: now_ms,
            callsign: None,
            category: None,
            squawk: None,
            altitude_ft: None,
            velocity: None,
            fix: None,
            provisional: false,
            even: None,
            odd: None,
        }
    }

    /// Resolve a position from `cpr`. Returns false if the plausibility gate rejected it.
    fn on_position(&mut self, cfg: &Config, cpr: CprPosition, now_ms: u64) -> bool {
        let other = if cpr.odd { self.even } else { self.odd };
        *(if cpr.odd {
            &mut self.odd
        } else {
            &mut self.even
        }) = Some((cpr, now_ms));
        let last = self
            .fix
            .filter(|f| now_ms.saturating_sub(f.t_ms) <= STALE_POSITION_MS);
        let global = other
            .filter(|&(_, t)| now_ms.saturating_sub(t) <= CPR_PAIR_WINDOW_MS)
            .and_then(|(o, _)| {
                let (e, od) = if cpr.odd { (o, cpr) } else { (cpr, o) };
                cpr_global(e, od, cpr.odd)
            });
        let (lat, lon, provisional) = match (global, last) {
            (Some((la, lo)), _) => (la, lo, false),
            (None, Some(f)) => {
                let (la, lo) = cpr_local(cpr, f.lat_deg, f.lon_deg);
                (la, lo, self.provisional)
            }
            (None, None) => {
                let (la, lo) = cpr_local(cpr, cfg.rx_lat_deg, cfg.rx_lon_deg);
                (la, lo, true)
            }
        };
        if distance_m(cfg.rx_lat_deg, cfg.rx_lon_deg, lat, lon) > MAX_RANGE_M {
            return false;
        }
        // The speed gate only compares against a trustworthy previous fix.
        if let Some(f) = last.filter(|_| !self.provisional) {
            let dt_s = now_ms.saturating_sub(f.t_ms) as f64 / 1000.0;
            if distance_m(f.lat_deg, f.lon_deg, lat, lon) > MAX_SPEED_MS * dt_s + GATE_SLACK_M {
                return false;
            }
        }
        self.fix = Some(Fix {
            lat_deg: lat,
            lon_deg: lon,
            t_ms: now_ms,
        });
        self.provisional = provisional;
        true
    }

    fn view(&self, cfg: &Config, now_ms: u64, radius_m: f64) -> AircraftView {
        let vel = self
            .velocity
            .filter(|v| now_ms.saturating_sub(v.t_ms) <= DR_HORIZON_MS);
        let position = self
            .fix
            .filter(|f| now_ms.saturating_sub(f.t_ms) <= STALE_POSITION_MS)
            .map(|f| {
                let age_ms = now_ms.saturating_sub(f.t_ms);
                // `predict` is None for "expired" and "no ground vector"; markers come from age.
                let pred = vel.and_then(|v| predict(&f, &v, now_ms));
                let (lat, lon) = pred.map_or((f.lat_deg, f.lon_deg), |p| (p.lat_deg, p.lon_deg));
                let ground_m = distance_m(cfg.rx_lat_deg, cfg.rx_lon_deg, lat, lon);
                let height_m = self
                    .altitude_ft
                    .map(|ft| ft as f64 * FT_TO_M - cfg.rx_elev_m);
                // CPA only from a live prediction; a receding aircraft's CPA is a past
                // hypothetical, so only an approaching one is exposed.
                let approach = pred.zip(vel).and_then(|(_, v)| {
                    cpa(cfg.rx_lat_deg, cfg.rx_lon_deg, lat, lon, &v.velocity)
                        .filter(|c| approaching(c, radius_m))
                });
                PositionView {
                    ground_m,
                    bearing_deg: bearing_deg(cfg.rx_lat_deg, cfg.rx_lon_deg, lat, lon),
                    height_m,
                    elevation_deg: height_m.map(|h| elevation_deg(h, ground_m)),
                    marker: if age_ms <= EXACT_AGE_MS {
                        Marker::Exact
                    } else if age_ms <= DR_HORIZON_MS {
                        Marker::Approx
                    } else {
                        Marker::Unknown
                    },
                    provisional: self.provisional,
                    age_ms,
                    in_radius: ground_m <= radius_m,
                    approach,
                }
            });
        let h = vel.and_then(|v| v.velocity.horizontal);
        AircraftView {
            icao: self.icao,
            callsign: self.callsign.clone(),
            category: self.category,
            squawk: self.squawk,
            altitude_ft: self.altitude_ft,
            vertical_rate_fpm: vel.and_then(|v| v.velocity.vertical_rate_fpm),
            ground_speed_kt: match h {
                Some(Horizontal::Ground { speed_kt, .. }) => Some(speed_kt),
                _ => None,
            },
            // Stationary has no direction; airspeed subtypes only offer a magnetic heading.
            track_deg: match h {
                Some(Horizontal::Ground {
                    speed_kt,
                    track_deg,
                }) if speed_kt >= STATIONARY_KT => Some(track_deg),
                Some(Horizontal::Air { heading_deg, .. }) => heading_deg,
                _ => None,
            },
            last_seen_age_ms: now_ms.saturating_sub(self.last_seen_ms),
            position,
        }
    }
}

/// How much to trust a displayed position, from the age of its fix (not from `predict`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Marker {
    /// Fix at most 3 s old.
    Exact,
    /// Older than 3 s: show "~".
    Approx,
    /// Older than the 15 s dead-reckoning horizon: last known position, show "?".
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PositionView {
    /// Great-circle (ground) distance from the receiver, metres, of the predicted position.
    pub ground_m: f64,
    pub bearing_deg: f64,
    /// Altitude minus receiver elevation.
    pub height_m: Option<f64>,
    pub elevation_deg: Option<f64>,
    pub marker: Marker,
    /// Decoded against the receiver only; may be wrong for aircraft beyond ~330 km.
    pub provisional: bool,
    pub age_ms: u64,
    pub in_radius: bool,
    /// Set only when the aircraft is approaching (CPA ahead, inside the radius).
    pub approach: Option<Cpa>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AircraftView {
    pub icao: u32,
    pub callsign: Option<String<8>>,
    pub category: Option<EmitterCategory>,
    pub squawk: Option<u16>,
    pub altitude_ft: Option<i32>,
    pub vertical_rate_fpm: Option<i32>,
    pub ground_speed_kt: Option<f32>,
    /// Track over ground, or magnetic heading for airspeed subtypes; `None` when stationary.
    pub track_deg: Option<f32>,
    pub last_seen_age_ms: u64,
    /// `None` when never positioned or the position is older than [`STALE_POSITION_MS`].
    pub position: Option<PositionView>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Card {
    /// No decoded message for [`NO_DATA_AFTER_MS`] (or ever). The firmware may also force this
    /// when the feed is known to be disconnected.
    NoData,
    Overhead,
    JustPassed,
    Quiet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JustPassed {
    /// Index into `Snapshot::list`.
    pub index: usize,
    /// Measured from the snapshot that noticed the aircraft had left, so up to one snapshot
    /// interval late.
    pub left_ago_ms: u64,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub card: Card,
    /// Positioned aircraft by ground distance (in-radius ones first), then the rest, most
    /// recently seen first.
    pub list: Vec<AircraftView, CAPACITY>,
    /// The first `in_radius` rows of `list` are inside the radius, nearest first.
    pub in_radius: usize,
    /// Index into `list`; one of the in-radius rows, chosen with hysteresis.
    pub primary: Option<usize>,
    /// Index into `list`: the nearest positioned aircraft (in or outside the radius), chosen
    /// with the same hysteresis as `primary`. Equals `primary` whenever that is set.
    pub focus: Option<usize>,
    pub just_passed: Option<JustPassed>,
    /// Indices into `list` of aircraft with a CPA inside the radius, soonest first.
    pub approaching: Vec<u8, CAPACITY>,
    pub counters: Counters,
}

impl Snapshot {
    /// In-radius aircraft other than the primary ("+N more").
    pub fn others(&self) -> impl Iterator<Item = &AircraftView> {
        self.list[..self.in_radius]
            .iter()
            .enumerate()
            .filter(|(i, _)| Some(*i) != self.primary)
            .map(|(_, v)| v)
    }

    /// Nearest positioned aircraft outside the radius.
    pub fn nearest_beyond(&self) -> Option<&AircraftView> {
        self.list
            .get(self.in_radius)
            .filter(|v| v.position.is_some())
    }
}

pub struct AircraftTable {
    cfg: Config,
    table: Vec<Aircraft, CAPACITY>,
    counters: Counters,
    last_message_ms: Option<u64>,
    primary: Option<u32>,
    focus: Option<u32>,
    just_passed: Option<(u32, u64)>,
}

impl AircraftTable {
    pub fn new(cfg: Config) -> Self {
        AircraftTable {
            cfg,
            table: Vec::new(),
            counters: Counters::default(),
            last_message_ms: None,
            primary: None,
            focus: None,
            just_passed: None,
        }
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    pub fn counters(&self) -> Counters {
        self.counters
    }

    pub fn update(&mut self, msg: &Message, now_ms: u64) {
        self.counters.messages = self.counters.messages.saturating_add(1);
        self.last_message_ms = Some(now_ms);
        let icao = msg.address.icao();
        let idx = match self.table.iter().position(|a| a.icao == icao) {
            Some(i) => i,
            None => {
                if !msg.address.is_verified() {
                    self.counters.filtered += 1;
                    return;
                }
                if self.table.is_full() {
                    let oldest = (0..self.table.len())
                        .min_by_key(|&i| self.table[i].last_seen_ms)
                        .unwrap_or(0);
                    self.table.swap_remove(oldest);
                }
                let _ = self.table.push(Aircraft::new(icao, now_ms));
                self.table.len() - 1
            }
        };
        let cfg = self.cfg;
        let a = &mut self.table[idx];
        match msg.address {
            Address::Verified(_) => a.verified_ms = now_ms,
            Address::Recovered(_) if now_ms.saturating_sub(a.verified_ms) > KNOWN_ICAO_MS => {
                self.counters.filtered = self.counters.filtered.saturating_add(1);
                return;
            }
            Address::Recovered(_) => {}
        }
        a.last_seen_ms = now_ms;
        match &msg.body {
            Body::Identification(id) => {
                a.callsign = Some(id.callsign.clone());
                a.category = id.category;
            }
            Body::AirbornePosition {
                cpr, altitude_ft, ..
            } => {
                if altitude_ft.is_some() {
                    a.altitude_ft = *altitude_ft;
                }
                if !a.on_position(&cfg, *cpr, now_ms) {
                    self.counters.implausible = self.counters.implausible.saturating_add(1);
                }
            }
            Body::Velocity(v) => {
                a.velocity = Some(TimedVelocity {
                    velocity: *v,
                    t_ms: now_ms,
                })
            }
            Body::Altitude { altitude_ft, .. } => {
                if altitude_ft.is_some() {
                    a.altitude_ft = *altitude_ft;
                }
            }
            Body::Squawk { code, .. } => a.squawk = Some(*code),
            Body::AllCall { .. } | Body::Other { .. } => {}
        }
    }

    /// Remove entries not heard from for [`REMOVE_AFTER_MS`].
    pub fn age(&mut self, now_ms: u64) {
        self.table
            .retain(|a| now_ms.saturating_sub(a.last_seen_ms) <= REMOVE_AFTER_MS);
    }

    /// Build the vicinity snapshot. `&mut self` because primary selection has hysteresis and
    /// "just passed" memory. `radius_km` is converted to metres once, here.
    pub fn snapshot(&mut self, now_ms: u64, radius_km: f64) -> Snapshot {
        let radius_m = radius_km * 1000.0;
        let mut list: Vec<AircraftView, CAPACITY> = Vec::new();
        for a in &self.table {
            let _ = list.push(a.view(&self.cfg, now_ms, radius_m));
        }
        list.sort_unstable_by(|a, b| {
            match (&a.position, &b.position) {
                (Some(p), Some(q)) => p.ground_m.total_cmp(&q.ground_m),
                (Some(_), None) => core::cmp::Ordering::Less,
                (None, Some(_)) => core::cmp::Ordering::Greater,
                (None, None) => a.last_seen_age_ms.cmp(&b.last_seen_age_ms),
            }
            .then(a.icao.cmp(&b.icao))
        });
        let ground = |v: &AircraftView| v.position.map_or(f64::INFINITY, |p| p.ground_m);
        let in_radius = list
            .iter()
            .take_while(|v| v.position.is_some_and(|p| p.in_radius))
            .count();

        // Primary with hysteresis: keep the current one while it is in the radius unless the
        // nearest is closer by both SWITCH_FRACTION and SWITCH_MIN_M.
        let cur = self
            .primary
            .and_then(|i| list[..in_radius].iter().position(|v| v.icao == i));
        let primary = match cur {
            Some(c) => {
                let (g0, gc) = (ground(&list[0]), ground(&list[c]));
                if g0 < gc * (1.0 - SWITCH_FRACTION) && gc - g0 > SWITCH_MIN_M {
                    Some(0)
                } else {
                    Some(c)
                }
            }
            None => (in_radius > 0).then_some(0),
        };

        // The old primary left the radius but is still positioned: remember it.
        if let (Some(old), None) = (self.primary, cur)
            && list
                .iter()
                .any(|v| v.icao == old && v.position.is_some_and(|p| !p.in_radius))
        {
            self.just_passed = Some((old, now_ms));
        }
        self.primary = primary.map(|i| list[i].icao);

        // Focus: same rule over all positioned aircraft. A stale or removed one has no
        // position (or is gone), so it drops out of the search and the nearest takes over.
        let positioned = list.iter().take_while(|v| v.position.is_some()).count();
        let cur = self
            .focus
            .and_then(|i| list[..positioned].iter().position(|v| v.icao == i));
        let focus = match cur {
            Some(c) => {
                let (g0, gc) = (ground(&list[0]), ground(&list[c]));
                if g0 < gc * (1.0 - SWITCH_FRACTION) && gc - g0 > SWITCH_MIN_M {
                    Some(0)
                } else {
                    Some(c)
                }
            }
            None => (positioned > 0).then_some(0),
        };
        self.focus = focus.map(|i| list[i].icao);

        let just_passed = self.just_passed.and_then(|(icao, left_ms)| {
            let left_ago_ms = now_ms.saturating_sub(left_ms);
            let index = list
                .iter()
                .position(|v| v.icao == icao && v.position.is_some_and(|p| !p.in_radius));
            index
                .filter(|_| left_ago_ms <= JUST_PASSED_MS)
                .map(|index| JustPassed { index, left_ago_ms })
        });
        if just_passed.is_none() {
            self.just_passed = None;
        }

        let mut approaching: Vec<u8, CAPACITY> = Vec::new();
        for (i, v) in list.iter().enumerate() {
            if v.position.is_some_and(|p| p.approach.is_some()) {
                let _ = approaching.push(i as u8);
            }
        }
        let t = |i: &u8| {
            list[*i as usize]
                .position
                .and_then(|p| p.approach)
                .map_or(0.0, |c| c.t_s)
        };
        approaching.sort_unstable_by(|a, b| t(a).total_cmp(&t(b)));

        let no_data = self
            .last_message_ms
            .is_none_or(|t| now_ms.saturating_sub(t) > NO_DATA_AFTER_MS);
        let card = if no_data {
            Card::NoData
        } else if primary.is_some() {
            Card::Overhead
        } else if just_passed.is_some() {
            Card::JustPassed
        } else {
            Card::Quiet
        };
        Snapshot {
            card,
            list,
            in_radius,
            primary,
            focus,
            just_passed,
            approaching,
            counters: self.counters,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use modes::{Identification, Velocity};

    const CFG: Config = Config {
        rx_lat_deg: 50.0,
        rx_lon_deg: 10.0,
        rx_elev_m: 100.0,
    };
    const M_PER_DEG: f64 = 111_195.0;

    /// Fictitious point `north_m` / `east_m` of the receiver (flat approximation).
    fn pt(north_m: f64, east_m: f64) -> (f64, f64) {
        (
            50.0 + north_m / M_PER_DEG,
            10.0 + east_m / (M_PER_DEG * libm::cos(50f64.to_radians())),
        )
    }

    /// Test-only CPR encoder (17-bit fields).
    fn cpr(lat: f64, lon: f64, odd: bool) -> CprPosition {
        let i = odd as u32 as f64;
        let enc =
            |x: f64, d: f64| libm::floor(131072.0 * x.rem_euclid(d) / d + 0.5) as u32 & 0x1FFFF;
        let dlat = 360.0 / (60.0 - i);
        let yz = enc(lat, dlat);
        let rlat = dlat * (yz as f64 / 131072.0 + libm::floor(lat / dlat));
        let dlon = 360.0 / (modes::nl(rlat) as f64 - i).max(1.0);
        CprPosition {
            odd,
            lat_cpr: yz,
            lon_cpr: enc(lon, dlon),
        }
    }

    fn msg(address: Address, body: Body) -> Message {
        Message {
            address,
            body,
            timestamp: None,
            signal: None,
        }
    }
    fn ver(icao: u32, body: Body) -> Message {
        msg(Address::Verified(icao), body)
    }
    fn pos_msg(icao: u32, lat: f64, lon: f64, odd: bool, alt_ft: i32) -> Message {
        ver(
            icao,
            Body::AirbornePosition {
                cpr: cpr(lat, lon, odd),
                altitude_ft: Some(alt_ft),
                gnss: false,
            },
        )
    }
    /// Even then odd frame at `now`: a global fix.
    fn fix(t: &mut AircraftTable, icao: u32, north: f64, east: f64, alt_ft: i32, now: u64) {
        let (la, lo) = pt(north, east);
        t.update(&pos_msg(icao, la, lo, false, alt_ft), now);
        t.update(&pos_msg(icao, la, lo, true, alt_ft), now);
    }
    fn vel(t: &mut AircraftTable, icao: u32, speed_kt: f32, track_deg: f32, now: u64) {
        let v = Velocity {
            horizontal: Some(Horizontal::Ground {
                speed_kt,
                track_deg,
            }),
            vertical_rate_fpm: Some(0),
            vr_baro: true,
            supersonic: false,
        };
        t.update(&ver(icao, Body::Velocity(v)), now);
    }
    fn ident(t: &mut AircraftTable, icao: u32, cs: &str, now: u64) {
        let id = Identification {
            callsign: String::try_from(cs).unwrap(),
            category: None,
        };
        t.update(&ver(icao, Body::Identification(id)), now);
    }
    fn table() -> AircraftTable {
        AircraftTable::new(CFG)
    }
    fn row(s: &Snapshot, icao: u32) -> &AircraftView {
        s.list.iter().find(|v| v.icao == icao).unwrap()
    }
    fn gd(s: &Snapshot, icao: u32) -> f64 {
        row(s, icao).position.unwrap().ground_m
    }
    fn close(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() <= tol, "{a} vs {b}");
    }
    /// 250 m/s in kt.
    const KT250: f32 = 486.0;

    #[test]
    fn position_from_pair() {
        let mut t = table();
        fix(&mut t, 1, 3000.0, 4000.0, 30_000, 1000);
        let s = t.snapshot(1000, 5.0);
        let p = row(&s, 1).position.unwrap();
        close(p.ground_m, 5000.0, 30.0);
        close(p.bearing_deg, 53.13, 0.5);
        assert!(!p.provisional);
        assert_eq!(p.marker, Marker::Exact);
        assert_eq!(s.counters.implausible, 0);
    }

    #[test]
    fn position_from_single_message_with_reference() {
        let mut t = table();
        let (la, lo) = pt(2000.0, 0.0);
        t.update(&pos_msg(1, la, lo, true, 10_000), 0);
        let p = row(&t.snapshot(0, 5.0), 1).position.unwrap();
        close(p.ground_m, 2000.0, 30.0);
        assert!(p.provisional);
        // A same-parity-free follow-up beyond the pair window decodes locally against the last
        // fix and stays provisional.
        let (la, lo) = pt(2500.0, 0.0);
        t.update(&pos_msg(1, la, lo, false, 10_000), 20_000);
        let p = row(&t.snapshot(20_000, 5.0), 1).position.unwrap();
        close(p.ground_m, 2500.0, 30.0);
        assert!(p.provisional);
        // A fresh pair makes it final.
        fix(&mut t, 1, 2600.0, 0.0, 10_000, 21_000);
        assert!(
            !row(&t.snapshot(21_000, 5.0), 1)
                .position
                .unwrap()
                .provisional
        );
    }

    #[test]
    fn provisional_fix_replaced_by_contradicting_global() {
        let mut t = table();
        // 400 km away: the receiver-relative local decode aliases to the wrong zone.
        let (la, lo) = pt(400_000.0, 0.0);
        t.update(&pos_msg(1, la, lo, false, 35_000), 0);
        let p = row(&t.snapshot(0, 5.0), 1).position.unwrap();
        assert!(p.provisional);
        assert!((p.ground_m - 400_000.0).abs() > 100_000.0);
        t.update(&pos_msg(1, la, lo, true, 35_000), 1000);
        let p = row(&t.snapshot(1000, 5.0), 1).position.unwrap();
        assert!(!p.provisional);
        close(p.ground_m, 400_000.0, 200.0);
    }

    #[test]
    fn plausibility_gate() {
        let mut t = table();
        // Beyond 450 km: the global decode is rejected whatever the display radius. (The first
        // frame's receiver-relative local decode aliases into range and stays provisional.)
        fix(&mut t, 1, 500_000.0, 0.0, 35_000, 0);
        let s = t.snapshot(0, 50.0);
        assert_eq!(s.counters.implausible, 1);
        let p = row(&s, 1).position.unwrap();
        assert!(p.provisional && p.ground_m <= MAX_RANGE_M);
        // A jump of 50 km in 1 s is faster than 1000 kt.
        fix(&mut t, 2, 10_000.0, 0.0, 10_000, 0);
        fix(&mut t, 2, 60_000.0, 0.0, 10_000, 1000);
        close(gd(&t.snapshot(1000, 5.0), 2), 10_000.0, 50.0);
        // ... but the same jump after 120 s (stale fix) is fine.
        fix(&mut t, 2, 60_000.0, 0.0, 10_000, 121_000);
        close(gd(&t.snapshot(121_000, 5.0), 2), 60_000.0, 100.0);
    }

    #[test]
    fn stale_and_expiry_transitions() {
        let mut t = table();
        fix(&mut t, 1, 1000.0, 0.0, 5000, 0);
        let m = |t: &mut AircraftTable, now| {
            let s = t.snapshot(now, 5.0);
            row(&s, 1).position.map(|p| p.marker)
        };
        assert_eq!(m(&mut t, 2000), Some(Marker::Exact));
        assert_eq!(m(&mut t, 5000), Some(Marker::Approx));
        assert_eq!(m(&mut t, 20_000), Some(Marker::Unknown));
        assert_eq!(m(&mut t, 31_000), None);
        t.age(31_000);
        assert_eq!(t.len(), 1);
        t.age(60_001);
        assert!(t.is_empty());
        assert!(t.snapshot(60_001, 5.0).list.is_empty());
    }

    #[test]
    fn eviction_when_full() {
        let mut t = table();
        for i in 0..CAPACITY as u32 {
            ident(&mut t, i, "X", i as u64 * 10);
        }
        assert_eq!(t.len(), CAPACITY);
        ident(&mut t, 1000, "NEW", 5000);
        assert_eq!(t.len(), CAPACITY);
        let s = t.snapshot(5000, 5.0);
        assert!(s.list.iter().all(|v| v.icao != 0));
        assert!(s.list.iter().any(|v| v.icao == 1000));
    }

    #[test]
    fn known_icao_filter() {
        let mut t = table();
        let alt = |a, ft| {
            msg(
                a,
                Body::Altitude {
                    df: 4,
                    altitude_ft: Some(ft),
                    mb: None,
                },
            )
        };
        t.update(&alt(Address::Recovered(7), 1000), 0);
        assert!(t.is_empty());
        assert_eq!(t.counters().filtered, 1);
        // Any Verified message (here Other) makes the address known.
        t.update(&ver(7, Body::Other { df: 17, tc: 28 }), 1000);
        t.update(&alt(Address::Recovered(7), 2000), 2000);
        assert_eq!(row(&t.snapshot(2000, 5.0), 7).altitude_ft, Some(2000));
        // 61 s after the last Verified message it is unknown again.
        t.update(&alt(Address::Recovered(7), 3000), 62_000);
        assert_eq!(row(&t.snapshot(62_000, 5.0), 7).altitude_ft, Some(2000));
        assert_eq!(t.counters().filtered, 2);
        // AllCall refreshes the window.
        t.update(
            &ver(
                7,
                Body::AllCall {
                    capability: 5,
                    iid: None,
                },
            ),
            62_000,
        );
        t.update(&alt(Address::Recovered(7), 4000), 100_000);
        assert_eq!(row(&t.snapshot(100_000, 5.0), 7).altitude_ft, Some(4000));
        // Squawk from a known Recovered frame is stored.
        t.update(
            &msg(
                Address::Recovered(7),
                Body::Squawk {
                    code: 7700,
                    mb: None,
                },
            ),
            100_000,
        );
        assert_eq!(row(&t.snapshot(100_000, 5.0), 7).squawk, Some(7700));
    }

    #[test]
    fn ground_distance_not_slant_and_units() {
        let mut t = table();
        // Directly overhead at 36,000 ft: ground 0, slant 11 km.
        fix(&mut t, 1, 0.0, 0.0, 36_000, 0);
        let s = t.snapshot(0, 5.0);
        let p = row(&s, 1).position.unwrap();
        assert!(p.in_radius);
        assert!(p.ground_m < 30.0);
        close(p.height_m.unwrap(), 36_000.0 * 0.3048 - 100.0, 1e-6);
        assert!(p.elevation_deg.unwrap() > 89.0);
        assert_eq!((s.card, s.primary), (Card::Overhead, Some(0)));
        // km -> m: 5.0 km radius includes 4.9 km, 4.8 km radius does not.
        fix(&mut t, 2, 4900.0, 0.0, 5000, 0);
        assert!(row(&t.snapshot(0, 5.0), 2).position.unwrap().in_radius);
        assert!(!row(&t.snapshot(0, 4.8), 2).position.unwrap().in_radius);
    }

    #[test]
    fn hysteresis_primary() {
        let mut t = table();
        let primary = |t: &mut AircraftTable, now| {
            let s = t.snapshot(now, 5.0);
            s.list[s.primary.unwrap()].icao
        };
        let step = |t: &mut AircraftTable, b_north: f64, now| {
            fix(t, 1, 1000.0, 0.0, 10_000, now);
            fix(t, 2, b_north, 0.0, 10_000, now);
        };
        step(&mut t, 4000.0, 0);
        assert_eq!(primary(&mut t, 0), 1);
        step(&mut t, 900.0, 10_000); // 10 % closer
        assert_eq!(primary(&mut t, 10_000), 1);
        step(&mut t, 790.0, 20_000); // 21 % closer but only 210 m
        assert_eq!(primary(&mut t, 20_000), 1);
        step(&mut t, 650.0, 30_000); // 35 % and 350 m
        assert_eq!(primary(&mut t, 30_000), 2);
        step(&mut t, 650.0, 40_000);
        assert_eq!(primary(&mut t, 40_000), 2);
    }

    #[test]
    fn focus_hysteresis_and_stale() {
        let mut t = table();
        let focus = |t: &mut AircraftTable, now| {
            let s = t.snapshot(now, 5.0);
            s.focus.map(|i| s.list[i].icao)
        };
        assert_eq!(focus(&mut t, 0), None);
        // Both outside the 5 km radius: focus still picks the nearest. Moves are slow enough
        // for the speed gate.
        fix(&mut t, 1, 20_000.0, 0.0, 10_000, 0);
        fix(&mut t, 2, 26_000.0, 0.0, 10_000, 0);
        assert_eq!(focus(&mut t, 0), Some(1));
        fix(&mut t, 1, 20_000.0, 0.0, 10_000, 20_000);
        fix(&mut t, 2, 17_000.0, 0.0, 10_000, 20_000); // 15 % closer
        assert_eq!(focus(&mut t, 20_000), Some(1));
        fix(&mut t, 1, 20_000.0, 0.0, 10_000, 40_000);
        fix(&mut t, 2, 10_000.0, 0.0, 10_000, 40_000); // 50 % and 10 km
        assert_eq!(focus(&mut t, 40_000), Some(2));
        // Aircraft 2 goes stale (no fix for more than 30 s): focus moves to 1.
        fix(&mut t, 1, 20_000.0, 0.0, 10_000, 75_000);
        assert_eq!(focus(&mut t, 75_000), Some(1));
        // A new aircraft inside the radius takes the focus, and it is the primary.
        fix(&mut t, 3, 3000.0, 0.0, 10_000, 80_000);
        let s = t.snapshot(80_000, 5.0);
        assert_eq!(s.list[s.focus.unwrap()].icao, 3);
        assert_eq!(s.focus, s.primary);
    }

    #[test]
    fn just_passed_then_quiet() {
        let mut t = table();
        fix(&mut t, 1, 4000.0, 0.0, 30_000, 0);
        assert_eq!(t.snapshot(0, 5.0).card, Card::Overhead);
        fix(&mut t, 1, 8000.0, 0.0, 30_000, 10_000);
        let s = t.snapshot(10_000, 5.0);
        assert_eq!(s.card, Card::JustPassed);
        assert_eq!(s.just_passed.unwrap().left_ago_ms, 0);
        assert_eq!(s.list[s.just_passed.unwrap().index].icao, 1);
        fix(&mut t, 1, 9000.0, 0.0, 30_000, 30_000);
        let s = t.snapshot(30_000, 5.0);
        assert_eq!(s.just_passed.unwrap().left_ago_ms, 20_000);
        fix(&mut t, 1, 12_000.0, 0.0, 30_000, 60_000);
        assert_eq!(t.snapshot(60_000, 5.0).card, Card::JustPassed);
        fix(&mut t, 1, 12_500.0, 0.0, 30_000, 71_000);
        let s = t.snapshot(71_000, 5.0);
        assert_eq!((s.card, s.just_passed), (Card::Quiet, None));
        assert!(s.nearest_beyond().is_some());
    }

    #[test]
    fn approaching_receding_and_dead_reckoning() {
        let mut t = table();
        // 10 km north, flying south straight at the receiver; one flying away.
        fix(&mut t, 1, 10_000.0, 0.0, 10_000, 0);
        vel(&mut t, 1, KT250, 180.0, 0);
        fix(&mut t, 2, 10_000.0, 0.0, 10_000, 0);
        vel(&mut t, 2, KT250, 0.0, 0);
        let s = t.snapshot(0, 5.0);
        assert_eq!(s.card, Card::Quiet);
        assert_eq!(s.approaching.len(), 1);
        let a = &s.list[s.approaching[0] as usize];
        assert_eq!(a.icao, 1);
        let c = a.position.unwrap().approach.unwrap();
        close(c.t_s, 40.0, 1.0);
        assert!(c.d_m < 50.0);
        assert!(row(&s, 2).position.unwrap().approach.is_none());
        assert_eq!(row(&s, 1).track_deg, Some(180.0));
        // After 2 s the dead-reckoned position is 500 m closer (exact marker, age <= 3 s).
        let s = t.snapshot(2000, 5.0);
        close(gd(&s, 1), 9500.0, 60.0);
        close(gd(&s, 2), 10_500.0, 60.0);
        // Past the 15 s horizon: last fix, "?", no CPA.
        let s = t.snapshot(16_000, 5.0);
        close(gd(&s, 1), 10_000.0, 60.0);
        assert_eq!(row(&s, 1).position.unwrap().marker, Marker::Unknown);
        assert!(s.approaching.is_empty());
    }

    #[test]
    fn no_data_card() {
        let mut t = table();
        assert_eq!(t.snapshot(0, 5.0).card, Card::NoData);
        ident(&mut t, 1, "A", 1000);
        assert_eq!(t.snapshot(2000, 5.0).card, Card::Quiet);
        assert_eq!(t.snapshot(31_001, 5.0).card, Card::NoData);
    }

    #[test]
    fn stationary_has_no_track() {
        let mut t = table();
        fix(&mut t, 1, 1000.0, 0.0, 0, 0);
        vel(&mut t, 1, 0.5, 90.0, 0);
        let s = t.snapshot(1000, 5.0);
        assert_eq!(row(&s, 1).track_deg, None);
        assert!(row(&s, 1).position.unwrap().approach.is_none());
    }

    #[test]
    fn list_sort_order() {
        let mut t = table();
        fix(&mut t, 1, 3000.0, 0.0, 5000, 0);
        fix(&mut t, 2, 1000.0, 0.0, 5000, 0);
        fix(&mut t, 3, 8000.0, 0.0, 5000, 0);
        ident(&mut t, 4, "NOPOS", 5000); // never positioned
        ident(&mut t, 5, "OLDPOS", 1000); // seen earlier than 4
        let s = t.snapshot(5000, 5.0);
        let order: std::vec::Vec<u32> = s.list.iter().map(|v| v.icao).collect();
        // 5 has no fix either; both unpositioned, most recently seen first.
        assert_eq!(order, [2, 1, 3, 4, 5]);
        assert_eq!(s.in_radius, 2);
        // Stale position sorts with the unpositioned ones.
        let s = t.snapshot(31_000, 5.0);
        assert!(s.list.iter().all(|v| v.position.is_none()));
        assert_eq!(s.in_radius, 0);
        assert_eq!(s.counters.messages, 8);
    }
}
