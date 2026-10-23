//! Dead reckoning and closest point of approach (CPA). Metres, seconds, degrees; speeds in kt.
//!
//! Only a *ground* vector (`Horizontal::Ground`: ground speed + track over ground) is used.
//! `Horizontal::Air` is airspeed + magnetic heading; wind makes it a different vector, so
//! the aircraft is not dead-reckoned from it.

use crate::geo::EARTH_RADIUS_M;
use modes::{Horizontal, Velocity};

/// 1 kt in m/s.
pub const KT_TO_MS: f64 = 1852.0 / 3600.0;
/// Dead reckoning is refused when the fix or the velocity is older than this.
pub const DR_HORIZON_MS: u64 = 15_000;
/// Predictions from a fix older than this are shown with "~".
pub const APPROX_AFTER_MS: u64 = 3_000;
/// Below this ground speed the aircraft counts as stationary (track is meaningless).
pub const STATIONARY_KT: f32 = 1.0;
/// An aircraft only counts as approaching if its CPA is at most this far ahead.
pub const APPROACH_MAX_S: f64 = 300.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fix {
    pub lat_deg: f64,
    pub lon_deg: f64,
    /// Time the position was received.
    pub t_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimedVelocity {
    pub velocity: Velocity,
    /// Time the velocity was received.
    pub t_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Predicted {
    pub lat_deg: f64,
    pub lon_deg: f64,
    pub age_ms: u64,
    /// Fix older than [`APPROX_AFTER_MS`]: display with "~".
    pub approximate: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cpa {
    /// Seconds from now to closest approach; negative = already passed / receding.
    pub t_s: f64,
    /// Ground distance at closest approach, metres.
    pub d_m: f64,
}

/// Ground velocity as (east, north) in m/s. `None` without a ground vector (no data, airspeed
/// subtypes); speeds under [`STATIONARY_KT`] give exactly (0, 0) whatever the track says.
pub fn ground_vector(v: &Velocity) -> Option<(f64, f64)> {
    let Some(Horizontal::Ground {
        speed_kt,
        track_deg,
    }) = v.horizontal
    else {
        return None;
    };
    if !speed_kt.is_finite() || !track_deg.is_finite() {
        return None;
    }
    if speed_kt < STATIONARY_KT {
        return Some((0.0, 0.0));
    }
    let (s, t) = (speed_kt as f64 * KT_TO_MS, (track_deg as f64).to_radians());
    Some((s * libm::sin(t), s * libm::cos(t)))
}

/// Wrap a longitude (difference) of magnitude < 540 into [-180, 180).
fn wrap_deg(d: f64) -> f64 {
    libm::fmod(d + 540.0, 360.0) - 180.0
}

/// Point reached from (lat, lon) after `dist_m` along initial bearing `bearing_rad` on the sphere.
/// Exact great circle, so it is safe across the antimeridian and the poles.
fn destination(lat_deg: f64, lon_deg: f64, bearing_rad: f64, dist_m: f64) -> (f64, f64) {
    let (p1, d) = (lat_deg.to_radians(), dist_m / EARTH_RADIUS_M);
    let (sp1, cp1, sd, cd) = (libm::sin(p1), libm::cos(p1), libm::sin(d), libm::cos(d));
    let sp2 = (sp1 * cd + cp1 * sd * libm::cos(bearing_rad)).clamp(-1.0, 1.0);
    let p2 = libm::asin(sp2);
    let dl = libm::atan2(libm::sin(bearing_rad) * sd * cp1, cd - sp1 * sp2);
    let lon = wrap_deg(lon_deg + dl.to_degrees());
    (p2.to_degrees(), lon)
}

/// Dead-reckoned position at `now_ms`: the fix moved along the ground vector for its age.
///
/// `None` when the fix or the velocity is older than [`DR_HORIZON_MS`] (the caller shows the last
/// fix marked "?"), or when there is no ground vector (see [`ground_vector`]). A stationary
/// aircraft is returned at its fix. Timestamps in the future are treated as age 0.
pub fn predict(fix: &Fix, velocity: &TimedVelocity, now_ms: u64) -> Option<Predicted> {
    let age_ms = now_ms.saturating_sub(fix.t_ms);
    if age_ms > DR_HORIZON_MS || now_ms.saturating_sub(velocity.t_ms) > DR_HORIZON_MS {
        return None;
    }
    let (ve, vn) = ground_vector(&velocity.velocity)?;
    let dist = libm::hypot(ve, vn) * age_ms as f64 / 1000.0;
    let (lat_deg, lon_deg) = if dist == 0.0 {
        (fix.lat_deg, fix.lon_deg)
    } else {
        destination(fix.lat_deg, fix.lon_deg, libm::atan2(ve, vn), dist)
    };
    Some(Predicted {
        lat_deg,
        lon_deg,
        age_ms,
        approximate: age_ms > APPROX_AFTER_MS,
    })
}

/// Closest point of approach to the receiver, assuming straight constant-velocity flight.
///
/// Flat-earth local tangent plane at the receiver: `r` = aircraft offset (east, north), then
/// `t = -(r.v)/|v|^2`, `d = |r + v t|`. Approximation: east is scaled by cos(receiver lat), which
/// errs by about `(range/R) * tan(lat)` relative (< 0.2 % within 10 km, < 1 % within 50 km up to
/// 60 deg latitude) and breaks down at the poles. `None` without a ground vector or when
/// stationary (no direction, so no CPA).
pub fn cpa(rx_lat: f64, rx_lon: f64, lat: f64, lon: f64, velocity: &Velocity) -> Option<Cpa> {
    let (ve, vn) = ground_vector(velocity)?;
    let v2 = ve * ve + vn * vn;
    if v2 == 0.0 {
        return None;
    }
    let dlon = wrap_deg(lon - rx_lon);
    let re = dlon.to_radians() * EARTH_RADIUS_M * libm::cos(rx_lat.to_radians());
    let rn = (lat - rx_lat).to_radians() * EARTH_RADIUS_M;
    let t_s = -(re * ve + rn * vn) / v2;
    Some(Cpa {
        t_s,
        d_m: libm::hypot(re + ve * t_s, rn + vn * t_s),
    })
}

/// Approaching: CPA ahead (within [`APPROACH_MAX_S`]) and inside `radius_m` (metres, not km).
pub fn approaching(cpa: &Cpa, radius_m: f64) -> bool {
    cpa.t_s > 0.0 && cpa.t_s <= APPROACH_MAX_S && cpa.d_m <= radius_m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::{bearing_deg, distance_m};

    fn close(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() <= tol, "{a} vs {b}");
    }
    fn gnd(speed_kt: f32, track_deg: f32) -> Velocity {
        Velocity {
            horizontal: Some(Horizontal::Ground {
                speed_kt,
                track_deg,
            }),
            vertical_rate_fpm: None,
            vr_baro: true,
            supersonic: false,
        }
    }
    fn tv(v: Velocity, t_ms: u64) -> TimedVelocity {
        TimedVelocity { velocity: v, t_ms }
    }
    /// 250 m/s in kt.
    const KT250: f32 = (250.0 / KT_TO_MS) as f32;

    #[test]
    fn kt_conversion() {
        close(KT_TO_MS * 3600.0, 1852.0, 1e-9);
        let (e, n) = ground_vector(&gnd(100.0, 90.0)).unwrap();
        close(e, 51.4444, 1e-3);
        close(n, 0.0, 1e-3);
    }

    #[test]
    fn dr_matches_great_circle() {
        // Eiffel Tower as a fictitious start; 250 m/s on track 60 for 15 s = 3750 m
        let fix = Fix {
            lat_deg: 48.8584,
            lon_deg: 2.2945,
            t_ms: 1000,
        };
        let p = predict(&fix, &tv(gnd(KT250, 60.0), 1000), 16_000).unwrap();
        close(
            distance_m(fix.lat_deg, fix.lon_deg, p.lat_deg, p.lon_deg),
            3750.0,
            1.0,
        );
        close(
            bearing_deg(fix.lat_deg, fix.lon_deg, p.lat_deg, p.lon_deg),
            60.0,
            0.01,
        );
        assert_eq!(p.age_ms, 15_000);
        assert!(p.approximate);
    }

    #[test]
    fn dr_approximate_flag() {
        let fix = Fix {
            lat_deg: 0.0,
            lon_deg: 0.0,
            t_ms: 0,
        };
        let v = tv(gnd(300.0, 0.0), 0);
        assert!(!predict(&fix, &v, 3000).unwrap().approximate);
        assert!(predict(&fix, &v, 3001).unwrap().approximate);
        // same instant and clock skew: no movement, no panic
        assert_eq!(predict(&fix, &v, 0).unwrap().lat_deg, 0.0);
        let late = Fix { t_ms: 500, ..fix };
        assert_eq!(predict(&late, &v, 0).unwrap().age_ms, 0);
    }

    #[test]
    fn horizon_expiry() {
        let fix = Fix {
            lat_deg: 0.0,
            lon_deg: 0.0,
            t_ms: 0,
        };
        let v = tv(gnd(300.0, 0.0), 0);
        assert!(predict(&fix, &v, 15_000).is_some());
        assert!(predict(&fix, &v, 15_001).is_none());
        // fresh fix, stale velocity
        let fix2 = Fix {
            t_ms: 20_000,
            ..fix
        };
        assert!(predict(&fix2, &v, 20_000).is_none());
        assert!(predict(&fix2, &tv(gnd(300.0, 0.0), 6_000), 21_000).is_some());
    }

    #[test]
    fn no_ground_vector() {
        let fix = Fix {
            lat_deg: 0.0,
            lon_deg: 0.0,
            t_ms: 0,
        };
        let air = Velocity {
            horizontal: Some(Horizontal::Air {
                airspeed_kt: Some(300.0),
                is_tas: true,
                heading_deg: Some(90.0),
            }),
            ..gnd(0.0, 0.0)
        };
        assert!(predict(&fix, &tv(air, 0), 1000).is_none());
        assert!(cpa(0.0, 0.0, 0.1, 0.0, &air).is_none());
        let none = Velocity {
            horizontal: None,
            ..air
        };
        assert!(predict(&fix, &tv(none, 0), 1000).is_none());
        assert!(predict(&fix, &tv(gnd(f32::NAN, 0.0), 0), 1000).is_none());
    }

    #[test]
    fn sub_1kt_is_stationary() {
        let fix = Fix {
            lat_deg: 10.0,
            lon_deg: 20.0,
            t_ms: 0,
        };
        for (s, t) in [(0.0, 180.0), (0.0, -0.0), (0.99, 270.0), (0.5, 45.0)] {
            let p = predict(&fix, &tv(gnd(s, t), 0), 10_000).unwrap();
            assert_eq!((p.lat_deg, p.lon_deg), (10.0, 20.0), "{s} {t}");
            assert!(cpa(10.0, 20.0, 10.1, 20.0, &gnd(s, t)).is_none());
        }
        // 1 kt moves (about 7.7 m in 15 s)
        let p = predict(&fix, &tv(gnd(1.0, 0.0), 0), 15_000).unwrap();
        close(distance_m(10.0, 20.0, p.lat_deg, p.lon_deg), 7.72, 0.05);
    }

    #[test]
    fn overflight_head_on() {
        // receiver 0/0, aircraft 10 km north heading south at 250 m/s
        let (lat, lon) = destination(0.0, 0.0, 0.0, 10_000.0);
        let c = cpa(0.0, 0.0, lat, lon, &gnd(KT250, 180.0)).unwrap();
        close(c.t_s, 40.0, 0.1);
        close(c.d_m, 0.0, 1.0);
        assert!(approaching(&c, 5000.0));
    }

    #[test]
    fn offset_pass() {
        // 2 km abeam, 10 km before the receiver, heading south
        let (lat, lon) = destination(0.0, 0.0, 90.0_f64.to_radians(), 2000.0);
        let (lat, lon) = destination(lat, lon, 0.0, 10_000.0);
        let c = cpa(0.0, 0.0, lat, lon, &gnd(KT250, 180.0)).unwrap();
        close(c.d_m, 2000.0, 5.0);
        close(c.t_s, 40.0, 0.1);
        assert!(approaching(&c, 5000.0));
        assert!(!approaching(&c, 1000.0));
    }

    #[test]
    fn receding_and_too_far_ahead() {
        let (lat, lon) = destination(0.0, 0.0, 0.0, 10_000.0);
        let c = cpa(0.0, 0.0, lat, lon, &gnd(KT250, 0.0)).unwrap();
        assert!(c.t_s < 0.0);
        assert!(!approaching(&c, 50_000.0));
        // 100 km out at 250 m/s: 400 s away, over the 5 min limit
        let (lat, lon) = destination(0.0, 0.0, 0.0, 100_000.0);
        let c = cpa(0.0, 0.0, lat, lon, &gnd(KT250, 180.0)).unwrap();
        close(c.t_s, 400.0, 1.0);
        assert!(!approaching(&c, 5000.0));
    }

    #[test]
    fn cpa_vs_great_circle_at_latitude() {
        // Eiffel receiver; aircraft 30 km to the NE-ish heading west, 3 km north of the receiver
        let (rlat, rlon) = (48.8584, 2.2945);
        let (lat, lon) = destination(rlat, rlon, 0.0, 3000.0);
        let (lat, lon) = destination(lat, lon, 90.0_f64.to_radians(), 30_000.0);
        let c = cpa(rlat, rlon, lat, lon, &gnd(KT250, 270.0)).unwrap();
        // reference: constant-track (parallel) path sampled every 0.5 s; a great circle with
        // initial bearing 270 would curve ~80 m off it over 30 km and is not what a track is
        let mut best = f64::MAX;
        for i in 0..=400 {
            let dlon = (250.0 * i as f64 * 0.5 / (EARTH_RADIUS_M * libm::cos(lat.to_radians())))
                .to_degrees();
            best = best.min(distance_m(rlat, rlon, lat, lon - dlon));
        }
        close(c.d_m, best, 15.0);
        close(c.t_s, 120.0, 1.5);
    }

    #[test]
    fn antimeridian() {
        let fix = Fix {
            lat_deg: 0.0,
            lon_deg: 179.99,
            t_ms: 0,
        };
        let p = predict(&fix, &tv(gnd(KT250, 90.0), 0), 15_000).unwrap();
        assert!(p.lon_deg < -179.0 && p.lon_deg >= -180.0, "{}", p.lon_deg);
        close(distance_m(0.0, 179.99, p.lat_deg, p.lon_deg), 3750.0, 1.0);
        // receiver just across the line sees the aircraft approaching
        let c = cpa(0.0, -179.99, 0.0, 179.99, &gnd(KT250, 90.0)).unwrap();
        assert!(c.t_s > 0.0);
        close(c.d_m, 0.0, 1.0);
    }

    #[test]
    fn over_the_pole() {
        // 1 km short of the north pole heading north: ends 2.75 km past it
        let lat = 90.0 - 1000.0 / EARTH_RADIUS_M.to_radians();
        let fix = Fix {
            lat_deg: lat,
            lon_deg: 10.0,
            t_ms: 0,
        };
        let p = predict(&fix, &tv(gnd(KT250, 0.0), 0), 15_000).unwrap();
        assert!(p.lat_deg.is_finite() && p.lon_deg.is_finite());
        close(
            distance_m(fix.lat_deg, fix.lon_deg, p.lat_deg, p.lon_deg),
            3750.0,
            1.0,
        );
        close(distance_m(90.0, 0.0, p.lat_deg, p.lon_deg), 2750.0, 1.0);
    }
}
