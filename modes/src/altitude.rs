//! Barometric altitude: 12-bit ADS-B field and 13-bit Mode S AC code (Q=1 25 ft, Q=0 Gillham).
//!
//! Field bit order, MSB first: C1 A1 C2 A2 C4 A4 [M] B1 D1/Q B2 D2 B4 D4 (M only in the 13-bit AC).

use crate::FrameData;

/// Gillham 100 ft cyclic code (C1 C2 C4 as a 3-bit value) to 1..=5; 0, 5 and 7 are invalid.
const C_CODE: [u8; 8] = [0, 1, 3, 2, 5, 0, 4, 0];

/// Altitude in feet from the field with M removed (12 bits, MSB = C1). 0 means "no data".
fn decode_12(v: u16) -> Option<i32> {
    if v == 0 {
        return None;
    }
    if v & 0x10 != 0 {
        // Q=1: drop the Q bit, remaining 11 bits count 25 ft from -1000 ft
        let n = (v & 0xFE0) >> 1 | v & 0xF;
        return Some(n as i32 * 25 - 1000);
    }
    let bit = |m: u16| (v & m != 0) as u8;
    let (c1, a1, c2, a2, c4, a4) = (
        bit(0x800),
        bit(0x400),
        bit(0x200),
        bit(0x100),
        bit(0x80),
        bit(0x40),
    );
    let (b1, b2, d2, b4, d4) = (bit(0x20), bit(0x8), bit(0x4), bit(0x2), bit(0x1));
    // 500 ft part: Gray code D2 D4 A1 A2 A4 B1 B2 B4 (MSB first)
    let gray = [d2, d4, a1, a2, a4, b1, b2, b4]
        .iter()
        .fold(0u32, |a, &b| a << 1 | b as u32);
    let mut n500 = gray;
    n500 ^= n500 >> 1;
    n500 ^= n500 >> 2;
    n500 ^= n500 >> 4;
    let c = C_CODE[(c1 << 2 | c2 << 1 | c4) as usize] as i32;
    if c == 0 {
        return None;
    }
    let c = if n500 & 1 == 1 { 6 - c } else { c };
    Some(n500 as i32 * 500 + c * 100 - 1300)
}

impl FrameData {
    /// Barometric altitude in feet from an airborne-position ME (TC 9-18). `None` for other
    /// type codes, no-data (field 0) or invalid Gillham codes. Does not check CRC.
    pub fn adsb_altitude(&self) -> Option<i32> {
        let me = self.me()?;
        matches!(me[0] >> 3, 9..=18).then_some(())?;
        decode_12((me[1] as u16) << 4 | (me[2] >> 4) as u16)
    }

    /// Altitude in feet from the AC field of DF0/4/16/20. `None` when M=1 (metric, unsupported),
    /// no data, or an invalid Gillham code. Does not check CRC.
    pub fn ac_altitude(&self) -> Option<i32> {
        if !matches!(self.df(), 0 | 4 | 16 | 20) || !self.length_matches_df() {
            return None;
        }
        let b = self.bytes();
        let v = ((b[2] & 0x1F) as u16) << 8 | b[3] as u16;
        if v & 0x40 != 0 {
            return None;
        }
        decode_12((v & 0x1F80) >> 1 | v & 0x3F)
    }
}

#[cfg(test)]
mod tests {
    use crate::FrameData;
    use crate::basics::synth::frame;

    /// Independent encoder: reflected-binary tables rather than the decoder's arithmetic.
    /// Returns the 12-bit field (Q=0) for an altitude that is a multiple of 100 ft.
    fn gillham_12(alt: i32) -> u16 {
        let n = (alt + 1200) / 100; // 100 ft steps from -1200
        let (n500, r) = (n / 5, n % 5);
        // 100 ft code sequence C1 C2 C4 for r = 0..4 (n500 even), reversed when odd
        const SEQ: [[u8; 3]; 5] = [[0, 0, 1], [0, 1, 1], [0, 1, 0], [1, 1, 0], [1, 0, 0]];
        let c = SEQ[if n500 % 2 == 0 { r } else { 4 - r } as usize];
        let g = (n500 ^ (n500 >> 1)) as u16; // D2 D4 A1 A2 A4 B1 B2 B4
        let gb = |i: u32| (g >> (7 - i)) & 1; // i = 0 is D2
        let (d2, d4, a1, a2, a4, b1, b2, b4) =
            (gb(0), gb(1), gb(2), gb(3), gb(4), gb(5), gb(6), gb(7));
        // C1 A1 C2 A2 C4 A4 B1 D1 B2 D2 B4 D4
        let bits = [
            c[0] as u16,
            a1,
            c[1] as u16,
            a2,
            c[2] as u16,
            a4,
            b1,
            0,
            b2,
            d2,
            b4,
            d4,
        ];
        bits.iter().fold(0, |a, &b| a << 1 | b)
    }

    fn adsb(field: u16, tc: u8) -> FrameData {
        let mut body = [0u8; 10];
        body[3] = tc << 3;
        body[4] = (field >> 4) as u8;
        body[5] = (field << 4) as u8;
        frame(17 << 3, &body, 0)
    }

    fn ac(df: u8, field13: u16) -> FrameData {
        let h = [(field13 >> 8) as u8, field13 as u8];
        if df >= 16 {
            frame(df << 3, &[0, h[0], h[1], 0, 0, 0, 0, 0, 0, 0], 0x123456)
        } else {
            frame(df << 3, &[0, h[0], h[1]], 0x123456)
        }
    }

    /// Insert the M bit (0x40) into a 12-bit field to make the 13-bit AC.
    fn to13(f12: u16, m: bool) -> u16 {
        (f12 & 0xFC0) << 1 | (f12 & 0x3F) | (m as u16) << 6
    }

    #[test]
    fn vector_38000() {
        let f = FrameData::Long({
            let hex = "8D40621D58C382D690C8AC2863A7";
            let mut b = [0u8; 14];
            for (i, o) in b.iter_mut().enumerate() {
                *o = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
            }
            b
        });
        assert_eq!(f.adsb_altitude(), Some(38000));
    }

    #[test]
    fn q1_25ft_steps_and_nodata() {
        // Q=1 field for n: bits above Q = n>>4, Q, low 4 bits
        let q1 = |n: u16| (n & 0x7F0) << 1 | 0x10 | (n & 0xF);
        assert_eq!(adsb(q1(0), 11).adsb_altitude(), Some(-1000));
        assert_eq!(adsb(q1(1), 11).adsb_altitude(), Some(-975));
        assert_eq!(adsb(q1(0x7FF), 11).adsb_altitude(), Some(50175));
        assert_eq!(adsb(0, 11).adsb_altitude(), None);
        assert_eq!(adsb(q1(1560), 18).adsb_altitude(), Some(38000));
    }

    #[test]
    fn adsb_type_code_range() {
        for tc in 0..32 {
            let want = (9..=18).contains(&tc).then_some(38000);
            assert_eq!(adsb(0xC38, tc).adsb_altitude(), want, "tc {tc}");
        }
    }

    #[test]
    fn gillham_known_pairs() {
        // Hand-derived from the Annex 10 tables.
        // -1200 ft: C4 only. -1100: C2 C4. -1000: C2. -900: C1 C2. -800: C1.
        assert_eq!(adsb(0x080, 11).adsb_altitude(), Some(-1200));
        assert_eq!(adsb(0x280, 11).adsb_altitude(), Some(-1100));
        assert_eq!(adsb(0x200, 11).adsb_altitude(), Some(-1000));
        assert_eq!(adsb(0xA00, 11).adsb_altitude(), Some(-900));
        assert_eq!(adsb(0x800, 11).adsb_altitude(), Some(-800));
        // n500=1 (gray 00000001 = B4), odd so reflected: C1 (raw 5) -> 1: -1300+500+100
        assert_eq!(adsb(0x802, 11).adsb_altitude(), Some(-700));
    }

    #[test]
    fn gillham_roundtrip_12_and_13() {
        let mut alt = -1200;
        while alt <= 126_700 {
            let f = gillham_12(alt);
            assert_eq!(f & 0x10, 0);
            assert_eq!(adsb(f, 12).adsb_altitude(), Some(alt), "adsb {alt}");
            for df in [0, 4, 16, 20] {
                assert_eq!(
                    ac(df, to13(f, false)).ac_altitude(),
                    Some(alt),
                    "DF{df} {alt}"
                );
            }
            assert_eq!(ac(4, to13(f, true)).ac_altitude(), None, "M=1 {alt}");
            alt += 100;
        }
    }

    #[test]
    fn gillham_invalid_codes() {
        let valid = gillham_12(0);
        // C bits 000, 101 (C1 C4) and 111 are invalid 100 ft codes
        for c in [0u16, 0b101, 0b111] {
            let f = (valid & !(0x800 | 0x200 | 0x080))
                | (c >> 2) << 11
                | (c >> 1 & 1) << 9
                | (c & 1) << 7;
            assert_eq!(adsb(f, 12).adsb_altitude(), None, "c {c:03b}");
        }
        // every 12-bit Q=0 code is either valid within range or None; never panics
        for f in (0..0x1000u16).filter(|f| f & 0x10 == 0) {
            if let Some(a) = adsb(f, 12).adsb_altitude() {
                assert!(
                    (-1200..=126_700).contains(&a) && a % 100 == 0,
                    "{f:03x} {a}"
                );
                assert_eq!(gillham_12(a), f);
            }
        }
    }

    #[test]
    fn ac_q1_and_formats() {
        let q1 = |n: u16| (n & 0x7E0) << 2 | (n & 0x10) << 1 | 0x10 | (n & 0xF);
        assert_eq!(ac(4, q1(1560)).ac_altitude(), Some(38000));
        assert_eq!(ac(4, q1(1560) | 0x40).ac_altitude(), None); // M=1
        assert_eq!(ac(4, 0).ac_altitude(), None); // no data
        assert_eq!(ac(5, q1(1560)).ac_altitude(), None); // DF5 is identity
        assert_eq!(ac(21, q1(1560)).ac_altitude(), None);
        assert_eq!(adsb(0xC38, 11).ac_altitude(), None);
    }
}
