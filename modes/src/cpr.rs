//! Compact Position Reporting for airborne positions (TC 9-18, 20-22): pure functions.
//!
//! Pair/time bookkeeping (even/odd within 10 s, provisional fixes, plausibility gate) is the
//! tracker's job. Surface positions are not supported.

use crate::FrameData;
use libm::{acos, cos, floor};

const NZ: f64 = 15.0;
const SCALE: f64 = 131072.0; // 2^17

/// Raw airborne-position CPR fields of one message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CprPosition {
    /// F bit: true = odd frame.
    pub odd: bool,
    /// 17-bit encoded latitude.
    pub lat_cpr: u32,
    /// 17-bit encoded longitude.
    pub lon_cpr: u32,
}

impl FrameData {
    /// CPR fields of an airborne position (TC 9-18, 20-22). `None` for other type codes.
    /// Does not check CRC.
    pub fn cpr_position(&self) -> Option<CprPosition> {
        let me = self.me()?;
        matches!(me[0] >> 3, 9..=18 | 20..=22).then_some(())?;
        Some(CprPosition {
            odd: me[2] & 0x04 != 0,
            lat_cpr: (me[2] as u32 & 3) << 15 | (me[3] as u32) << 7 | (me[4] >> 1) as u32,
            lon_cpr: (me[4] as u32 & 1) << 16 | (me[5] as u32) << 8 | me[6] as u32,
        })
    }
}

/// Number of longitude zones at `lat` degrees (1..=59).
pub fn nl(lat: f64) -> u8 {
    let lat = lat.abs();
    if lat == 0.0 {
        return 59;
    }
    if lat == 87.0 {
        return 2;
    }
    if lat > 87.0 {
        return 1;
    }
    let a = 1.0
        - (1.0 - cos(core::f64::consts::PI / (2.0 * NZ))) / {
            let c = cos(lat.to_radians());
            c * c
        };
    floor(2.0 * core::f64::consts::PI / acos(a)) as u8
}

fn modulo(x: f64, y: f64) -> f64 {
    x - y * floor(x / y)
}

fn wrap_lon(lon: f64) -> f64 {
    modulo(lon + 180.0, 360.0) - 180.0
}

/// Global decode of an even/odd pair, returning the position of the newer message as
/// `(lat, lon)` in degrees (lon in [-180, 180)). `None` if the two latitudes fall in different
/// NL zones (aircraft crossed a zone boundary; discard the pair) or the result is out of range.
/// The caller must enforce the 10 s pair window.
pub fn cpr_global(even: CprPosition, odd: CprPosition, newest_is_odd: bool) -> Option<(f64, f64)> {
    let (lat_e, lon_e) = (even.lat_cpr as f64 / SCALE, even.lon_cpr as f64 / SCALE);
    let (lat_o, lon_o) = (odd.lat_cpr as f64 / SCALE, odd.lon_cpr as f64 / SCALE);
    let j = floor(59.0 * lat_e - 60.0 * lat_o + 0.5);
    let fix = |l: f64| if l >= 270.0 { l - 360.0 } else { l };
    let rlat_e = fix(360.0 / 60.0 * (modulo(j, 60.0) + lat_e));
    let rlat_o = fix(360.0 / 59.0 * (modulo(j, 59.0) + lat_o));
    if !(-90.0..=90.0).contains(&rlat_e) || !(-90.0..=90.0).contains(&rlat_o) {
        return None;
    }
    let n = nl(rlat_e);
    if n != nl(rlat_o) {
        return None;
    }
    let (lat, lon_c, i) = if newest_is_odd {
        (rlat_o, lon_o, 1)
    } else {
        (rlat_e, lon_e, 0)
    };
    let ni = (n as i32 - i).max(1) as f64;
    let m = floor(lon_e * (n as f64 - 1.0) - lon_o * n as f64 + 0.5);
    let lon = wrap_lon(360.0 / ni * (modulo(m, ni) + lon_c));
    Some((lat, lon))
}

/// Local decode of a single message against a reference position, `(lat, lon)` in degrees.
///
/// Only valid if the true position is within 180 NM (airborne) of the reference; farther away
/// it silently returns a wrong position, so callers must use a reference that is close
/// (the aircraft's recent fix, or the receiver) and sanity-gate the result.
pub fn cpr_local(cpr: CprPosition, ref_lat: f64, ref_lon: f64) -> (f64, f64) {
    let i = cpr.odd as i32;
    let (y, x) = (cpr.lat_cpr as f64 / SCALE, cpr.lon_cpr as f64 / SCALE);
    let dlat = 360.0 / (60 - i) as f64;
    let j = floor(ref_lat / dlat) + floor(0.5 + modulo(ref_lat, dlat) / dlat - y);
    let lat = dlat * (j + y);
    let ni = (nl(lat) as i32 - i).max(1) as f64;
    let dlon = 360.0 / ni;
    let m = floor(ref_lon / dlon) + floor(0.5 + modulo(ref_lon, dlon) / dlon - x);
    (lat, wrap_lon(dlon * (m + x)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::basics::synth::frame;

    fn long(hex: &str) -> FrameData {
        let mut b = [0u8; 14];
        for (i, o) in b.iter_mut().enumerate() {
            *o = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
        }
        FrameData::Long(b)
    }

    fn encode(lat: f64, lon: f64, odd: bool) -> CprPosition {
        crate::encode::cpr_encode(lat, lon, odd)
    }

    fn close(a: (f64, f64), lat: f64, lon: f64, tol: f64) -> bool {
        let dlon = (a.1 - lon + 540.0) % 360.0 - 180.0;
        (a.0 - lat).abs() < tol && dlon.abs() < tol
    }

    const EVEN: &str = "8D40621D58C382D690C8AC2863A7";
    const ODD: &str = "8D40621D58C386435CC412692AD6";

    #[test]
    fn extracts_fields_from_vectors() {
        let e = long(EVEN).cpr_position().unwrap();
        let o = long(ODD).cpr_position().unwrap();
        assert_eq!((e.odd, e.lat_cpr, e.lon_cpr), (false, 93000, 51372));
        assert_eq!((o.odd, o.lat_cpr, o.lon_cpr), (true, 74158, 50194));
    }

    #[test]
    fn cpr_position_only_for_airborne_type_codes() {
        for tc in 0..32u8 {
            let mut body = [0u8; 10];
            body[3] = tc << 3;
            let f = frame(17 << 3, &body, 0);
            let want = matches!(tc, 9..=18 | 20..=22);
            assert_eq!(f.cpr_position().is_some(), want, "tc {tc}");
        }
        assert_eq!(
            FrameData::Short([0x88, 0, 0, 0, 0, 0, 0]).cpr_position(),
            None
        );
    }

    #[test]
    fn global_vector() {
        let e = long(EVEN).cpr_position().unwrap();
        let o = long(ODD).cpr_position().unwrap();
        let p = cpr_global(e, o, true).unwrap();
        assert!(close(p, 52.26578, 3.93891, 0.0005), "{p:?}");
        // newest = even gives the (earlier) even-frame position, as in the book
        let p = cpr_global(e, o, false).unwrap();
        assert!(close(p, 52.2572, 3.9194, 0.0005), "{p:?}");
    }

    #[test]
    fn local_vector() {
        let e = long(EVEN).cpr_position().unwrap();
        let p = cpr_local(e, 52.258, 3.918);
        assert!(close(p, 52.2572, 3.9194, 0.001), "{p:?}");
    }

    #[test]
    fn nl_transition_latitudes() {
        // Spec transition table: NL = n for lat below the listed value (and above the previous).
        let table = [
            (59, 10.47047130),
            (58, 14.82817437),
            (57, 18.18626357),
            (56, 21.02939493),
            (3, 86.53536998),
        ];
        for (n, up) in table {
            assert_eq!(nl(up - 1e-6), n, "below {up}");
            assert_eq!(nl(up + 1e-6), n - 1, "above {up}");
            assert_eq!(nl(-(up - 1e-6)), n);
        }
        assert_eq!(nl(0.0), 59);
        assert_eq!(nl(86.9), 2);
        assert_eq!(nl(87.0), 2);
        assert_eq!(nl(87.1), 1);
        assert_eq!(nl(90.0), 1);
        assert_eq!(nl(-90.0), 1);
        let mut prev = 59;
        for k in 0..=900 {
            let n = nl(k as f64 / 10.0);
            assert!(n <= prev && n >= 1);
            prev = n;
        }
    }

    #[test]
    fn global_round_trip_edges() {
        for (lat, lon) in [
            (86.5, 10.0),
            (-86.5, -10.0),
            (86.9, 100.0),
            (-86.9, 100.0),
            (0.01, 0.01),
            (-0.01, -0.01),
            (45.0, 179.99),
            (45.0, -179.99),
            (-33.0, 180.0),
            (-33.0, -180.0),
            (52.25, 3.9),
        ] {
            let (e, o) = (encode(lat, lon, false), encode(lat + 0.001, lon, true));
            for newest_odd in [true, false] {
                let p = cpr_global(e, o, newest_odd).unwrap_or_else(|| panic!("{lat} {lon}"));
                let tol = if lat.abs() > 86.0 { 0.02 } else { 0.002 };
                let (tl, tn) = if newest_odd {
                    (lat + 0.001, lon)
                } else {
                    (lat, lon)
                };
                assert!(close(p, tl, tn, tol), "{lat} {lon}: {p:?}");
                assert!((-180.0..180.0).contains(&p.1));
            }
        }
    }

    #[test]
    fn zone_mismatch_rejected() {
        // NL changes from 59 to 58 at 10.47 deg
        let (e, o) = (encode(10.46, 20.0, false), encode(10.48, 20.0, true));
        assert_eq!(cpr_global(e, o, true), None);
        // same latitudes on one side of the boundary decode fine
        let (e, o) = (encode(10.30, 20.0, false), encode(10.35, 20.0, true));
        assert!(cpr_global(e, o, true).is_some());
    }

    #[test]
    fn local_round_trip_and_wrap() {
        for (lat, lon, rlat, rlon) in [
            (52.2572, 3.9194, 52.258, 3.918),
            (10.0, 179.99, 10.5, -179.5),
            (10.0, -179.99, 10.5, 179.5),
            (-45.0, 0.0, -44.0, -2.0),
            (86.5, 170.0, 86.0, 160.0),
            (-86.5, -170.0, -86.0, -160.0),
            (30.0, 100.0, 30.0 + 2.9, 100.0),
        ] {
            for odd in [false, true] {
                let p = cpr_local(encode(lat, lon, odd), rlat, rlon);
                assert!(close(p, lat, lon, 0.002), "{lat} {lon} odd={odd}: {p:?}");
                assert!((-180.0..180.0).contains(&p.1));
            }
        }
    }
}
