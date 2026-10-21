//! Stateless frame decoder: one `RawFrame` in, one `Message` out, with the address trust level
//! explicit in the type. Tracking, CPR pairing and the known-ICAO filter (R15) live above.

use crate::{CprPosition, FrameData, Identification, RawFrame, SignalLevel, Timestamp, Velocity};

/// Transponder address and how far it can be trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Address {
    /// Explicit AA of a frame whose CRC proved it: DF17/18 with remainder 0, DF11 with remainder 0.
    Verified(u32),
    /// Not proven by the CRC: the address recovered from the AP overlay (DF0/4/5/16/20/21), or
    /// the AA of a DF11 whose remainder is an interrogator ID. A corrupted frame yields a random
    /// value here, so the tracker must accept it only if already known from a `Verified` frame (R15).
    Recovered(u32),
}

impl Address {
    pub fn icao(self) -> u32 {
        match self {
            Address::Verified(a) | Address::Recovered(a) => a,
        }
    }

    pub fn is_verified(self) -> bool {
        matches!(self, Address::Verified(_))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Body {
    /// DF17/18: TC 1-4.
    Identification(Identification),
    /// DF17/18: TC 9-18 (barometric) and 20-22 (GNSS height, `altitude_ft` is `None` there).
    AirbornePosition {
        cpr: CprPosition,
        altitude_ft: Option<i32>,
        gnss: bool,
    },
    /// DF17/18: TC 19.
    Velocity(Velocity),
    /// DF11 all-call reply. `iid` is the interrogator ID when the remainder is 1..=127.
    AllCall { capability: u8, iid: Option<u8> },
    /// DF0/4/16/20 altitude reply. `mb` is the raw Comm-B payload (DF20 only; BDS not inferred).
    Altitude {
        df: u8,
        altitude_ft: Option<i32>,
        mb: Option<[u8; 7]>,
    },
    /// DF5/21 identity reply. `code` is the Mode A code as four octal digits written in decimal
    /// (7700 means octal 7700). `mb` is the raw Comm-B payload (DF21 only).
    Squawk { code: u16, mb: Option<[u8; 7]> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub address: Address,
    pub body: Body,
    pub timestamp: Option<Timestamp>,
    pub signal: Option<SignalLevel>,
}

impl FrameData {
    /// Mode A code from the 13-bit ID field of DF5/21, as four octal digits written in decimal.
    /// Field bits MSB first: C1 A1 C2 A2 C4 A4 X B1 D1 B2 D2 B4 D4.
    pub fn squawk(&self) -> Option<u16> {
        if !matches!(self.df(), 5 | 21) || !self.length_matches_df() {
            return None;
        }
        let b = self.bytes();
        let v = ((b[2] & 0x1F) as u16) << 8 | b[3] as u16;
        let bit = |n: u32| v >> n & 1;
        let digit = |four: u32, two: u32, one: u32| bit(four) << 2 | bit(two) << 1 | bit(one);
        let (a, b, c, d) = (
            digit(7, 9, 11),
            digit(1, 3, 5),
            digit(8, 10, 12),
            digit(0, 2, 4),
        );
        Some(a * 1000 + b * 100 + c * 10 + d)
    }

    fn comm_b(&self) -> Option<[u8; 7]> {
        match self {
            FrameData::Long(b) => b[4..11].try_into().ok(),
            FrameData::Short(_) => None,
        }
    }
}

/// Decode one frame. `None` for: long frames failing the CRC, DF11 with a remainder above 127,
/// DF mismatching the frame length, and formats/type codes not decoded (surface position, DF18
/// CF != 0, DF19, ...).
pub fn decode(frame: &RawFrame) -> Option<Message> {
    let f = &frame.data;
    let (address, body) = match f.df() {
        17 | 18 => {
            if !f.crc_ok() {
                return None;
            }
            let body = if let Some(id) = f.identification() {
                Body::Identification(id)
            } else if let Some(v) = f.velocity() {
                Body::Velocity(v)
            } else {
                let cpr = f.cpr_position()?;
                Body::AirbornePosition {
                    cpr,
                    altitude_ft: f.adsb_altitude(),
                    gnss: f.type_code()? >= 20,
                }
            };
            (Address::Verified(f.icao()?), body)
        }
        11 => {
            let aa = f.icao()?;
            let address = match f.crc_remainder() {
                0 => Address::Verified(aa),
                1..=127 => Address::Recovered(aa),
                _ => return None,
            };
            let body = Body::AllCall {
                capability: f.bytes()[0] & 7,
                iid: f.df11_iid(),
            };
            (address, body)
        }
        df @ (0 | 4 | 16 | 20) => (
            Address::Recovered(f.recovered_address()?),
            Body::Altitude {
                df,
                altitude_ft: f.ac_altitude(),
                mb: if df == 20 { f.comm_b() } else { None },
            },
        ),
        df @ (5 | 21) => (
            Address::Recovered(f.recovered_address()?),
            Body::Squawk {
                code: f.squawk()?,
                mb: if df == 21 { f.comm_b() } else { None },
            },
        ),
        _ => return None,
    };
    Some(Message {
        address,
        body,
        timestamp: frame.timestamp,
        signal: frame.signal,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::basics::synth::frame;
    use crate::{ClockKind, Horizontal};

    fn raw(data: FrameData) -> RawFrame {
        RawFrame {
            data,
            timestamp: Some(Timestamp {
                ticks: 42,
                clock: ClockKind::Counter12MHz,
            }),
            signal: Some(SignalLevel(7)),
        }
    }

    fn long(hex: &str) -> FrameData {
        let mut b = [0u8; 14];
        for (i, o) in b.iter_mut().enumerate() {
            *o = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
        }
        FrameData::Long(b)
    }

    /// Short frame whose 13-bit AC/ID field is `field`.
    fn short(df: u8, field: u16, overlay: u32) -> FrameData {
        frame(df << 3, &[0, (field >> 8) as u8, field as u8], overlay)
    }

    #[test]
    fn squawk_known_codes() {
        // Field values worked out by hand from C1 A1 C2 A2 C4 A4 X B1 D1 B2 D2 B4 D4.
        for (field, code) in [
            (0x0AAA, 7700),
            (0x0AA2, 7500),
            (0x0A8A, 7600),
            (0x0808, 1200),
            (0x0A80, 7000),
            (0x0000, 0),
            (0x1FBF, 7777),
            (0x0040, 0),  // X bit ignored
            (0x0001, 4),  // D4
            (0x0010, 1),  // D1
            (0x1000, 10), // C1
        ] {
            assert_eq!(short(5, field, 0).squawk(), Some(code), "{field:#x}");
        }
    }

    #[test]
    fn squawk_roundtrip_all_codes() {
        // Encode with the spec bit positions, decode, compare.
        for code in 0..=0o7777u16 {
            let d = |s: u32| code >> s & 7;
            let (a, b, c, dd) = (d(9), d(6), d(3), d(0));
            let bit = |digit: u16, m: u16, n: u32| (digit & m != 0) as u16 * (1 << n);
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
            let want = a * 1000 + b * 100 + c * 10 + dd;
            assert_eq!(short(5, field, 0).squawk(), Some(want), "{code:o}");
        }
    }

    #[test]
    fn df5_and_df21_decode_squawk_recovered() {
        let m = decode(&raw(short(5, 0x0AAA, 0xABCDEF))).unwrap();
        assert_eq!(m.address, Address::Recovered(0xABCDEF));
        assert_eq!(
            m.body,
            Body::Squawk {
                code: 7700,
                mb: None
            }
        );
        assert_eq!(m.timestamp.unwrap().ticks, 42);
        assert_eq!(m.signal, Some(SignalLevel(7)));

        let mb = [1, 2, 3, 4, 5, 6, 7];
        let mut body = [0u8; 10];
        body[1] = 0x0A;
        body[2] = 0xA2;
        body[3..].copy_from_slice(&mb);
        let m = decode(&raw(frame(21 << 3, &body, 0x123456))).unwrap();
        assert_eq!(m.address, Address::Recovered(0x123456));
        assert_eq!(
            m.body,
            Body::Squawk {
                code: 7500,
                mb: Some(mb)
            }
        );
    }

    #[test]
    fn altitude_replies_df0_4_16_20() {
        // Q=1: 38000 ft -> n = 1560 -> 11 bits, Q at 0x10 of the 12-bit form -> AC field 0x0A38? computed below
        let n: u16 = (38000 + 1000) / 25;
        let twelve = (n & 0x7F0) << 1 | 0x10 | n & 0xF;
        let field = (twelve & 0xFC0) << 1 | twelve & 0x3F;
        for df in [0u8, 4] {
            let m = decode(&raw(short(df, field, 0xA1B2C3))).unwrap();
            assert_eq!(m.address, Address::Recovered(0xA1B2C3));
            assert_eq!(
                m.body,
                Body::Altitude {
                    df,
                    altitude_ft: Some(38000),
                    mb: None
                }
            );
        }
        let mut body = [0u8; 10];
        body[1] = (field >> 8) as u8;
        body[2] = field as u8;
        body[3..].copy_from_slice(&[9; 7]);
        for (df, mb) in [(16u8, None), (20, Some([9u8; 7]))] {
            let m = decode(&raw(frame(df << 3, &body, 0x111111))).unwrap();
            assert!(!m.address.is_verified());
            assert_eq!(
                m.body,
                Body::Altitude {
                    df,
                    altitude_ft: Some(38000),
                    mb
                }
            );
        }
    }

    #[test]
    fn df11_verified_with_zero_remainder() {
        let m = decode(&raw(frame(11 << 3 | 5, &[0xAB, 0xCD, 0xEF], 0))).unwrap();
        assert_eq!(m.address, Address::Verified(0xABCDEF));
        assert_eq!(
            m.body,
            Body::AllCall {
                capability: 5,
                iid: None
            }
        );
    }

    #[test]
    fn df11_iid_is_recovered_and_large_remainder_dropped() {
        let m = decode(&raw(frame(11 << 3, &[0xAB, 0xCD, 0xEF], 5))).unwrap();
        assert_eq!(m.address, Address::Recovered(0xABCDEF));
        assert_eq!(
            m.body,
            Body::AllCall {
                capability: 0,
                iid: Some(5)
            }
        );
        assert_eq!(decode(&raw(frame(11 << 3, &[0xAB, 0xCD, 0xEF], 128))), None);
    }

    #[test]
    fn long_adsb_frames_are_verified() {
        let m = decode(&raw(long("8D4840D6202CC371C32CE0576098"))).unwrap();
        assert_eq!(m.address, Address::Verified(0x4840D6));
        let Body::Identification(id) = m.body else {
            panic!()
        };
        assert_eq!(id.callsign.as_str(), "KLM1023");

        let m = decode(&raw(long("8D40621D58C382D690C8AC2863A7"))).unwrap();
        assert_eq!(m.address, Address::Verified(0x40621D));
        let Body::AirbornePosition {
            cpr,
            altitude_ft,
            gnss,
        } = m.body
        else {
            panic!()
        };
        assert!(!cpr.odd && !gnss);
        assert_eq!(altitude_ft, Some(38000));

        let m = decode(&raw(long("8D485020994409940838175B284F"))).unwrap();
        let Body::Velocity(v) = m.body else { panic!() };
        assert!(matches!(v.horizontal, Some(Horizontal::Ground { .. })));
        assert_eq!(m.timestamp.unwrap().ticks, 42);
    }

    #[test]
    fn corrupted_long_frames_are_dropped() {
        let FrameData::Long(good) = long("8D4840D6202CC371C32CE0576098") else {
            unreachable!()
        };
        for bit in 0..112 {
            let mut b = good;
            b[bit / 8] ^= 0x80 >> (bit % 8);
            // a flip in the DF field may yield an AP format, which is Recovered, never Verified
            if let Some(m) = decode(&raw(FrameData::Long(b))) {
                assert!(!m.address.is_verified(), "bit {bit}");
            }
        }
    }

    #[test]
    fn undecoded_formats_give_none() {
        assert_eq!(decode(&raw(frame(19 << 3, &[0; 10], 0))), None);
        assert_eq!(decode(&raw(frame(18 << 3 | 1, &[0; 10], 0))), None); // DF18 CF=1
        assert_eq!(
            decode(&raw(frame(17 << 3, &[0x28, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0))),
            None
        ); // TC5 surface
        assert_eq!(
            decode(&raw(FrameData::Short([0x8D, 0, 0, 0, 0, 0, 0]))),
            None
        ); // length mismatch
    }
}
