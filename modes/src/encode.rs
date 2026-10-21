//! Frame encoders (feature `encode`): the inverse of `decode()`, for scenarios and tests.
//! Every function returns a frame with a correct CRC / address-parity overlay.
//!
//! Fixed choices, since the decoder does not distinguish them: DF17 CA = 5, airborne position
//! as TC 11 (barometric, 25 ft steps, Q = 1), velocity as subtype 1 with barometric vertical rate.

use crate::{CategorySet, CprPosition, EmitterCategory, FrameData};
use libm::{cos, floor, round, sin};

const CPR_SCALE: f64 = 131072.0; // 2^17
const DF17_HEAD: u8 = 17 << 3 | 5;

/// Build a frame with correct parity. `head` is the first byte (DF + 3 low bits), `body` the
/// rest of the data bits (3 bytes for a short frame, 10 for a long one). `overlay` is XORed into
/// the parity field (the address, for AP formats).
pub fn frame(head: u8, body: &[u8], overlay: u32) -> FrameData {
    assert!(
        body.len() == 3 || body.len() == 10,
        "body must be 3 or 10 bytes"
    );
    let n = body.len() + 1;
    let mut out = [0u8; 14];
    out[0] = head;
    out[1..n].copy_from_slice(body);
    let crc = |out: &[u8; 14]| match n {
        4 => FrameData::Short(out[..7].try_into().unwrap()).crc_remainder(),
        _ => FrameData::Long(*out).crc_remainder(),
    };
    // With zeroed parity the remainder is the CRC of the data bits.
    let p = (crc(&out) ^ overlay).to_be_bytes();
    out[n..n + 3].copy_from_slice(&p[1..]);
    match n {
        4 => FrameData::Short(out[..7].try_into().unwrap()),
        _ => FrameData::Long(out),
    }
}

fn modulo(x: f64, y: f64) -> f64 {
    x - y * floor(x / y)
}

/// 17-bit CPR fields of a position (spec encoding). Quantisation error is at most ~3.7 m.
pub fn cpr_encode(lat_deg: f64, lon_deg: f64, odd: bool) -> CprPosition {
    let i = odd as i32;
    let dlat = 360.0 / (60 - i) as f64;
    let yz = floor(CPR_SCALE * modulo(lat_deg, dlat) / dlat + 0.5);
    let rlat = dlat * (yz / CPR_SCALE + floor(lat_deg / dlat));
    let dlon = 360.0 / (crate::nl(rlat) as i32 - i).max(1) as f64;
    let xz = floor(CPR_SCALE * modulo(lon_deg, dlon) / dlon + 0.5);
    CprPosition {
        odd,
        lat_cpr: mask17(yz),
        lon_cpr: mask17(xz),
    }
}

fn mask17(v: f64) -> u32 {
    v as u32 & 0x1FFFF
}

fn aa(icao: u32) -> [u8; 3] {
    let b = icao.to_be_bytes();
    [b[1], b[2], b[3]]
}

/// DF17 with the 7-byte ME field.
fn df17(icao: u32, me: [u8; 7]) -> FrameData {
    let a = aa(icao);
    frame(
        DF17_HEAD,
        &[
            a[0], a[1], a[2], me[0], me[1], me[2], me[3], me[4], me[5], me[6],
        ],
        0,
    )
}

/// DF17 identification. `callsign` is upper-cased `A-Z 0-9 space`; other characters become
/// spaces, anything past 8 characters is dropped. `None` category encodes CA = 0 in set A.
pub fn identification(icao: u32, callsign: &str, category: Option<EmitterCategory>) -> FrameData {
    let (tc, ca) = match category {
        Some(EmitterCategory { set, ca }) => (
            match set {
                CategorySet::A => 4,
                CategorySet::B => 3,
                CategorySet::C => 2,
                CategorySet::D => 1,
            },
            ca & 7,
        ),
        None => (4, 0),
    };
    let mut bits = 0u64;
    let mut chars = callsign.bytes();
    for _ in 0..8 {
        let code = match chars.next().map(|c| c.to_ascii_uppercase()) {
            Some(c @ b'A'..=b'Z') => c - b'A' + 1,
            Some(c @ b'0'..=b'9') => c - b'0' + 48,
            _ => 32,
        };
        bits = bits << 6 | code as u64;
    }
    let b = bits.to_be_bytes();
    df17(icao, [tc << 3 | ca, b[2], b[3], b[4], b[5], b[6], b[7]])
}

/// 12-bit altitude field, Q = 1 (25 ft steps, clamped to -1000..=50175 ft).
fn altitude_field(altitude_ft: i32) -> u16 {
    let n = ((altitude_ft + 1000) as f64 / 25.0).clamp(0.0, 2047.0);
    let n = round(n) as u16;
    (n & 0x7F0) << 1 | 0x10 | n & 0xF
}

/// DF17 airborne position, TC 11.
pub fn airborne_position(
    icao: u32,
    altitude_ft: i32,
    lat_deg: f64,
    lon_deg: f64,
    odd: bool,
) -> FrameData {
    let c = cpr_encode(lat_deg, lon_deg, odd);
    let alt = altitude_field(altitude_ft);
    df17(
        icao,
        [
            11 << 3,
            (alt >> 4) as u8,
            (alt << 4) as u8 | (odd as u8) << 2 | (c.lat_cpr >> 15) as u8,
            (c.lat_cpr >> 7) as u8,
            (c.lat_cpr << 1) as u8 | (c.lon_cpr >> 16) as u8,
            (c.lon_cpr >> 8) as u8,
            c.lon_cpr as u8,
        ],
    )
}

/// DF17 ground-speed velocity, subtype 1 (1 kt steps, 64 fpm steps, saturating).
pub fn velocity(icao: u32, speed_kt: f32, track_deg: f32, vertical_rate_fpm: i32) -> FrameData {
    let (s, t) = (speed_kt as f64, (track_deg as f64).to_radians());
    // (sign, raw) with raw = |v| + 1 (0 means "no data")
    let comp =
        |v: f64, unit: f64, max: f64| (v < 0.0, (round(v.abs() / unit) + 1.0).min(max) as u64);
    let (sew, vew) = comp(s * sin(t), 1.0, 1023.0);
    let (sns, vns) = comp(s * cos(t), 1.0, 1023.0);
    let (svr, vr) = comp(vertical_rate_fpm as f64, 64.0, 511.0);
    let bits = (sew as u64) << 42
        | vew << 32
        | (sns as u64) << 31
        | vns << 21
        | 1 << 20 // barometric vertical rate
        | (svr as u64) << 19
        | vr << 10;
    let me = (19u64 << 51 | 1 << 48 | bits).to_be_bytes();
    df17(icao, me[1..].try_into().unwrap())
}

/// DF11 all-call reply (interrogator ID 0, so the CRC remainder is 0 and the address verified).
pub fn all_call(icao: u32, capability: u8) -> FrameData {
    frame(11 << 3 | capability & 7, &aa(icao), 0)
}

/// DF4 altitude reply with AP overlay. The AC field uses Q = 1 (M = 0).
pub fn altitude_reply(icao: u32, altitude_ft: i32) -> FrameData {
    let f = altitude_field(altitude_ft);
    let ac = (f & 0xFC0) << 1 | f & 0x3F;
    frame(4 << 3, &[0, (ac >> 8) as u8, ac as u8], icao)
}

/// DF5 identity reply with AP overlay. `squawk` is four octal digits written in decimal
/// (7700 = octal 7700), as in `Body::Squawk`; digits above 7 are masked.
pub fn identity_reply(icao: u32, squawk: u16) -> FrameData {
    let d = |div: u16| (squawk / div % 10) & 7;
    let (a, b, c, dd) = (d(1000), d(100), d(10), d(1));
    let bit = |digit: u16, m: u16, n: u32| ((digit & m != 0) as u16) << n;
    let field = bit(c, 1, 12)
        | bit(a, 1, 11)
        | bit(c, 2, 10)
        | bit(a, 2, 9)
        | bit(c, 4, 8)
        | bit(a, 4, 7)
        | bit(b, 1, 5)
        | bit(dd, 1, 4)
        | bit(b, 2, 3)
        | bit(dd, 2, 2)
        | bit(b, 4, 1)
        | bit(dd, 4, 0);
    frame(5 << 3, &[0, (field >> 8) as u8, field as u8], icao)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Address, Body, ClockKind, Horizontal, RawFrame, Timestamp, cpr_global, cpr_local, decode,
    };

    /// xorshift64*; the tests need reproducible "random" inputs, not quality.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (hi - lo) * (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }
        fn icao(&mut self) -> u32 {
            (self.next() >> 20) as u32 & 0xFF_FFFF
        }
    }

    fn msg(f: FrameData) -> crate::Message {
        decode(&RawFrame {
            data: f,
            timestamp: Some(Timestamp {
                ticks: 1,
                clock: ClockKind::Counter12MHz,
            }),
            signal: None,
        })
        .expect("decodes")
    }

    #[test]
    fn identification_roundtrip() {
        let mut r = Rng(1);
        for (cs, cat) in [
            (
                "KLM1023",
                Some(EmitterCategory {
                    set: CategorySet::A,
                    ca: 3,
                }),
            ),
            (
                "AB",
                Some(EmitterCategory {
                    set: CategorySet::B,
                    ca: 1,
                }),
            ),
            (
                "12345678",
                Some(EmitterCategory {
                    set: CategorySet::C,
                    ca: 7,
                }),
            ),
            (
                "Z",
                Some(EmitterCategory {
                    set: CategorySet::D,
                    ca: 2,
                }),
            ),
            ("", None),
            ("TOOLONG123", None),
        ] {
            let icao = r.icao();
            let m = msg(identification(icao, cs, cat));
            assert_eq!(m.address, Address::Verified(icao));
            let Body::Identification(id) = m.body else {
                panic!()
            };
            assert_eq!(id.callsign.as_str(), &cs[..cs.len().min(8)]);
            assert_eq!(id.category, cat);
        }
    }

    #[test]
    fn airborne_position_roundtrip_with_cpr_error_below_5_m() {
        let mut r = Rng(2);
        let mut global = 0;
        for _ in 0..500 {
            let (lat, lon) = (r.uniform(-85.0, 85.0), r.uniform(-180.0, 180.0));
            let alt = (r.uniform(-1000.0, 50000.0) / 25.0) as i32 * 25;
            let icao = r.icao();
            let (e, o) = (
                msg(airborne_position(icao, alt, lat, lon, false)),
                msg(airborne_position(icao, alt, lat, lon, true)),
            );
            let (
                Body::AirbornePosition {
                    cpr: ce,
                    altitude_ft: ae,
                    gnss: false,
                },
                Body::AirbornePosition {
                    cpr: co,
                    altitude_ft: ao,
                    ..
                },
            ) = (e.body, o.body)
            else {
                panic!()
            };
            assert_eq!((ae, ao), (Some(alt), Some(alt)));
            assert!(!ce.odd && co.odd);
            let err = |(la, lo): (f64, f64)| {
                let dn = (la - lat).to_radians() * 6_371_000.0;
                let de = ((lo - lon + 540.0) % 360.0 - 180.0).to_radians()
                    * 6_371_000.0
                    * cos(lat.to_radians());
                libm::sqrt(dn * dn + de * de)
            };
            // Local decode against a reference ~50 km off, both parities.
            for c in [ce, co] {
                let loc = cpr_local(c, lat + 0.3, lon + 0.3 / cos(lat.to_radians()).max(0.05));
                assert!(err(loc) < 5.0, "local {lat} {lon}: {}", err(loc));
            }
            // Global decode: None only when the pair straddles an NL zone boundary.
            if let Some(p) = cpr_global(ce, co, true) {
                global += 1;
                assert!(err(p) < 5.0, "global {lat} {lon}: {}", err(p));
            }
        }
        assert!(global >= 475, "{global}/500");
    }

    #[test]
    fn velocity_roundtrip() {
        let mut r = Rng(3);
        for _ in 0..500 {
            let (s, t) = (r.uniform(0.0, 700.0) as f32, r.uniform(0.0, 360.0) as f32);
            let vr = (r.uniform(-6000.0, 6000.0) / 64.0) as i32 * 64;
            let m = msg(velocity(r.icao(), s, t, vr));
            let Body::Velocity(v) = m.body else { panic!() };
            let Some(Horizontal::Ground {
                speed_kt,
                track_deg,
            }) = v.horizontal
            else {
                panic!()
            };
            assert!((speed_kt - s).abs() < 1.5, "{speed_kt} vs {s}");
            if s > 50.0 {
                let d = (track_deg - t + 540.0) % 360.0 - 180.0;
                assert!(d.abs() < 1.5, "{track_deg} vs {t}");
            }
            assert_eq!(v.vertical_rate_fpm, Some(vr));
            assert!(v.vr_baro && !v.supersonic);
        }
    }

    #[test]
    fn short_formats_roundtrip() {
        let mut r = Rng(4);
        for _ in 0..200 {
            let icao = r.icao();
            let m = msg(all_call(icao, 5));
            assert_eq!(m.address, Address::Verified(icao));
            assert!(matches!(
                m.body,
                Body::AllCall {
                    capability: 5,
                    iid: None
                }
            ));

            let alt = (r.uniform(-1000.0, 50000.0) / 25.0) as i32 * 25;
            let m = msg(altitude_reply(icao, alt));
            assert_eq!(m.address, Address::Recovered(icao));
            assert!(
                matches!(m.body, Body::Altitude { df: 4, altitude_ft: Some(a), mb: None } if a == alt)
            );

            let sq = [7700, 7600, 7500, 1200, 0, 7777, 4321][(r.next() % 7) as usize];
            let m = msg(identity_reply(icao, sq));
            assert_eq!(m.address, Address::Recovered(icao));
            assert!(matches!(m.body, Body::Squawk { code, mb: None } if code == sq));
        }
        // every squawk
        for a in 0..8u16 {
            for b in 0..8 {
                for c in 0..8 {
                    for d in 0..8 {
                        let sq = a * 1000 + b * 100 + c * 10 + d;
                        assert_eq!(identity_reply(1, sq).squawk(), Some(sq));
                    }
                }
            }
        }
    }

    #[test]
    fn frames_have_valid_crc_and_overlay() {
        assert!(identification(0xABCDEF, "X", None).crc_ok());
        assert!(velocity(0xABCDEF, 100.0, 90.0, 0).crc_ok());
        assert!(all_call(0xABCDEF, 5).crc_ok());
        assert_eq!(
            altitude_reply(0xABCDEF, 1500).recovered_address(),
            Some(0xABCDEF)
        );
    }
}
