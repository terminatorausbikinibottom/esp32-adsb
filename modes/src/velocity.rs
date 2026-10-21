//! Airborne velocity (TC 19, subtypes 1-4).

use crate::FrameData;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Horizontal {
    /// ST1/2: ground speed and track over ground in degrees `[0, 360)`.
    Ground { speed_kt: f32, track_deg: f32 },
    /// ST3/4: airspeed (IAS or TAS) and magnetic heading; each `None` when not available.
    Air {
        airspeed_kt: Option<f32>,
        is_tas: bool,
        heading_deg: Option<f32>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Velocity {
    /// ST1/2 give `None` when either velocity component is "no data".
    pub horizontal: Option<Horizontal>,
    /// Positive = climb; `None` when no data.
    pub vertical_rate_fpm: Option<i32>,
    /// Vertical rate source: true = barometric, false = GNSS.
    pub vr_baro: bool,
    /// ST2/4 (speeds in 4 kt units).
    pub supersonic: bool,
}

impl FrameData {
    /// Airborne velocity (TC 19). `None` for other type codes or reserved subtypes (0, 5-7).
    /// Does not check CRC.
    pub fn velocity(&self) -> Option<Velocity> {
        let me = self.me()?;
        if me[0] >> 3 != 19 {
            return None;
        }
        let st = me[0] & 7;
        let v = me.iter().fold(0u64, |a, &b| a << 8 | b as u64);
        // Field at 1-indexed ME bit `from` (MSB first), `len` bits wide.
        let f = |from: u32, len: u32| (v >> (57 - from - len) & ((1 << len) - 1)) as i32;
        let scale = if st == 2 || st == 4 { 4.0 } else { 1.0 };
        // Raw 0 = no data, else value + 1.
        let mag = |raw: i32, unit: f32| (raw != 0).then(|| (raw - 1) as f32 * unit);
        let signed =
            |neg: i32, raw: i32, unit: f32| mag(raw, unit).map(|m| if neg == 1 { -m } else { m });
        let horizontal = match st {
            1 | 2 => signed(f(14, 1), f(15, 10), scale)
                .zip(signed(f(25, 1), f(26, 10), scale))
                .map(|(ew, ns)| {
                    let track = libm::atan2f(ew, ns).to_degrees();
                    Horizontal::Ground {
                        speed_kt: libm::sqrtf(ew * ew + ns * ns),
                        track_deg: if track < 0.0 { track + 360.0 } else { track },
                    }
                }),
            3 | 4 => Some(Horizontal::Air {
                airspeed_kt: mag(f(26, 10), scale),
                is_tas: f(25, 1) == 1,
                heading_deg: (f(14, 1) == 1).then(|| f(15, 10) as f32 * 360.0 / 1024.0),
            }),
            _ => return None,
        };
        let vertical_rate_fpm = signed(f(37, 1), f(38, 9), 64.0).map(|r| r as i32);
        Some(Velocity {
            horizontal,
            vertical_rate_fpm,
            vr_baro: f(36, 1) == 1,
            supersonic: scale == 4.0,
        })
    }
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

    /// ST1/2 ME: signs, magnitudes (raw), VR sign/raw.
    fn ground(st: u8, sew: u64, vew: u64, sns: u64, vns: u64, svr: u64, vr: u64) -> FrameData {
        let bits = sew << 42 | vew << 32 | sns << 31 | vns << 21 | svr << 19 | vr << 10;
        build(st, bits)
    }

    /// ST3/4 ME: heading status/raw, airspeed type/raw.
    fn air(st: u8, hs: u64, hdg: u64, tas: u64, asp: u64) -> FrameData {
        build(st, hs << 42 | hdg << 32 | tas << 31 | asp << 21)
    }

    /// `bits` holds ME bits 14..=56 in place (bit 14 = bit 42 of the u64).
    fn build(st: u8, bits: u64) -> FrameData {
        let me = (19u64 << 51 | (st as u64) << 48 | bits).to_be_bytes();
        frame(
            17 << 3 | 5,
            &[
                0xAB, 0xCD, 0xEF, me[1], me[2], me[3], me[4], me[5], me[6], me[7],
            ],
            0,
        )
    }

    fn gs(v: FrameData) -> (f32, f32) {
        match v.velocity().unwrap().horizontal {
            Some(Horizontal::Ground {
                speed_kt,
                track_deg,
            }) => (speed_kt, track_deg),
            h => panic!("{h:?}"),
        }
    }

    #[test]
    fn st1_vector() {
        let v = long("8D485020994409940838175B284F").velocity().unwrap();
        let Some(Horizontal::Ground {
            speed_kt,
            track_deg,
        }) = v.horizontal
        else {
            panic!()
        };
        assert!((speed_kt - 159.2).abs() < 0.1, "{speed_kt}");
        assert!((track_deg - 182.88).abs() < 0.05, "{track_deg}");
        assert_eq!(v.vertical_rate_fpm, Some(-832));
        assert!(!v.vr_baro && !v.supersonic); // ME bit 36 = 0 -> GNSS
    }

    #[test]
    fn st3_vector() {
        let v = long("8DA05F219B06B6AF189400CBC33F").velocity().unwrap();
        let Some(Horizontal::Air {
            airspeed_kt,
            is_tas,
            heading_deg,
        }) = v.horizontal
        else {
            panic!()
        };
        assert!((heading_deg.unwrap() - 243.98).abs() < 0.05);
        assert_eq!(airspeed_kt, Some(375.0));
        assert!(is_tas);
        assert_eq!(v.vertical_rate_fpm, Some(-2304));
    }

    #[test]
    fn track_quadrants() {
        for (sew, sns, want) in [(0, 0, 45.0), (0, 1, 135.0), (1, 1, 225.0), (1, 0, 315.0)] {
            let (s, t) = gs(ground(1, sew, 101, sns, 101, 0, 1));
            assert!(
                (s - 141.42).abs() < 0.01 && (t - want).abs() < 0.01,
                "{s} {t}"
            );
        }
        // due north / due west
        assert_eq!(gs(ground(1, 0, 1, 0, 101, 0, 1)).1, 0.0);
        assert_eq!(gs(ground(1, 1, 101, 0, 1, 0, 1)).1, 270.0);
    }

    #[test]
    fn ground_no_data_vs_zero_and_max() {
        let h = |f: FrameData| f.velocity().unwrap().horizontal;
        assert_eq!(h(ground(1, 0, 0, 0, 101, 0, 1)), None);
        assert_eq!(h(ground(1, 0, 101, 0, 0, 0, 1)), None);
        assert_eq!(gs(ground(1, 0, 1, 0, 1, 0, 1)).0, 0.0);
        let (s, _) = gs(ground(1, 0, 1023, 0, 1023, 0, 1));
        assert!((s - 1022.0 * 2f32.sqrt()).abs() < 0.01);
    }

    #[test]
    fn supersonic_scaling() {
        let (s, _) = gs(ground(2, 0, 101, 0, 1, 0, 1));
        assert_eq!(s, 400.0);
        let v = ground(2, 0, 101, 0, 1, 0, 3).velocity().unwrap();
        assert!(v.supersonic);
        assert_eq!(v.vertical_rate_fpm, Some(128)); // VR not scaled
        let a = air(4, 0, 0, 0, 101).velocity().unwrap();
        assert_eq!(
            a.horizontal,
            Some(Horizontal::Air {
                airspeed_kt: Some(400.0),
                is_tas: false,
                heading_deg: None
            })
        );
    }

    #[test]
    fn heading_availability_and_airspeed_no_data() {
        let v = air(3, 1, 512, 0, 0).velocity().unwrap();
        assert_eq!(
            v.horizontal,
            Some(Horizontal::Air {
                airspeed_kt: None,
                is_tas: false,
                heading_deg: Some(180.0)
            })
        );
        // heading bits set but status 0 -> unavailable
        let v = air(3, 0, 512, 1, 1).velocity().unwrap();
        assert_eq!(
            v.horizontal,
            Some(Horizontal::Air {
                airspeed_kt: Some(0.0),
                is_tas: true,
                heading_deg: None
            })
        );
    }

    #[test]
    fn vertical_rate_cases() {
        let vr = |svr, raw| {
            let v = ground(1, 0, 101, 0, 101, svr, raw);
            v.velocity().unwrap().vertical_rate_fpm
        };
        assert_eq!(vr(0, 0), None);
        assert_eq!(vr(1, 0), None);
        assert_eq!(vr(0, 1), Some(0));
        assert_eq!(vr(0, 511), Some(32640));
        assert_eq!(vr(1, 511), Some(-32640));
    }

    #[test]
    fn vr_source_bit() {
        let base = ground(1, 0, 101, 0, 101, 0, 1);
        assert!(!base.velocity().unwrap().vr_baro);
        let FrameData::Long(b) = base else { panic!() };
        let me = u64::from_be_bytes([0, b[4], b[5], b[6], b[7], b[8], b[9], b[10]]) | 1 << 20;
        let m = me.to_be_bytes();
        let f = frame(
            17 << 3 | 5,
            &[0xAB, 0xCD, 0xEF, m[1], m[2], m[3], m[4], m[5], m[6], m[7]],
            0,
        );
        assert!(f.velocity().unwrap().vr_baro);
    }

    #[test]
    fn other_type_codes_and_reserved_subtypes() {
        assert_eq!(long("8D40621D58C382D690C8AC2863A7").velocity(), None);
        assert_eq!(ground(0, 0, 101, 0, 101, 0, 1).velocity(), None);
        assert_eq!(ground(5, 0, 101, 0, 101, 0, 1).velocity(), None);
    }
}
