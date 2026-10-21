//! Frame basics: downlink format, ICAO address, CRC-24 and address/parity overlay.

use crate::FrameData;

const POLY: u32 = 0xFFF409;

impl FrameData {
    pub fn bytes(&self) -> &[u8] {
        match self {
            FrameData::Short(b) => b,
            FrameData::Long(b) => b,
        }
    }

    /// Downlink format (top 5 bits). Not checked against the frame length.
    pub fn df(&self) -> u8 {
        self.bytes()[0] >> 3
    }

    /// Whether the length matches what the DF requires (DF >= 16 is long).
    pub fn length_matches_df(&self) -> bool {
        matches!(self, FrameData::Long(_)) == (self.df() >= 16)
    }

    /// CRC-24 remainder over the whole frame, parity field included.
    ///
    /// 0 for a valid DF17/18. For AP-overlay formats it equals the transponder address.
    pub fn crc_remainder(&self) -> u32 {
        let mut reg: u32 = 0;
        for &byte in self.bytes() {
            for bit in (0..8).rev() {
                let top = (reg >> 23) & 1;
                reg = ((reg << 1) | ((byte >> bit) as u32 & 1)) & 0xFF_FFFF;
                if top == 1 {
                    reg ^= POLY;
                }
            }
        }
        reg
    }

    /// True for DF17/18 (long) and DF11 (short) frames whose remainder is 0.
    pub fn crc_ok(&self) -> bool {
        self.length_matches_df() && self.crc_remainder() == 0
    }

    /// Explicit AA field of DF11/17/18. Unvalidated: check `crc_ok` before trusting it.
    pub fn icao(&self) -> Option<u32> {
        match self.df() {
            11 | 17 | 18 if self.length_matches_df() => {
                let b = self.bytes();
                Some(u32::from_be_bytes([0, b[1], b[2], b[3]]))
            }
            _ => None,
        }
    }

    /// Address recovered from the parity overlay of DF0/4/5/16/20/21.
    ///
    /// Cannot be validated here: the caller must apply a known-ICAO filter (R15).
    pub fn recovered_address(&self) -> Option<u32> {
        match self.df() {
            0 | 4 | 5 | 16 | 20 | 21 if self.length_matches_df() => Some(self.crc_remainder()),
            _ => None,
        }
    }

    /// Interrogator ID of a DF11 reply: a non-zero remainder that fits in 7 bits.
    pub fn df11_iid(&self) -> Option<u8> {
        match self.crc_remainder() {
            r @ 1..=127 if self.df() == 11 && self.length_matches_df() => Some(r as u8),
            _ => None,
        }
    }
}

/// Test helper: synthetic frames with correct parity. Superseded by the `encode` feature (S.1).
#[cfg(test)]
pub(crate) mod synth {
    use crate::FrameData;

    /// CRC-24 of the data bits only (frame with zeroed parity field).
    fn parity_of(data: &[u8]) -> u32 {
        let mut padded = [0u8; 14];
        padded[..data.len()].copy_from_slice(data);
        let n = data.len() + 3;
        let f = if n == 7 {
            FrameData::Short(padded[..7].try_into().unwrap())
        } else {
            FrameData::Long(padded)
        };
        f.crc_remainder()
    }

    /// `head` is the first byte (DF + 3 low bits), `body` the rest of the data bits
    /// (4 bytes short, 11 bytes long). `overlay` is XORed into the parity (address for AP).
    pub fn frame(head: u8, body: &[u8], overlay: u32) -> FrameData {
        let mut data = [0u8; 11];
        data[0] = head;
        data[1..=body.len()].copy_from_slice(body);
        let data = &data[..=body.len()];
        let p = (parity_of(data) ^ overlay).to_be_bytes();
        let mut out = [0u8; 14];
        out[..data.len()].copy_from_slice(data);
        out[data.len()..data.len() + 3].copy_from_slice(&p[1..]);
        if data.len() == 4 {
            FrameData::Short(out[..7].try_into().unwrap())
        } else {
            FrameData::Long(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::synth::frame;
    use super::*;

    fn long(hex: &str) -> FrameData {
        let mut b = [0u8; 14];
        for (i, o) in b.iter_mut().enumerate() {
            *o = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
        }
        FrameData::Long(b)
    }

    const VECTORS: [(&str, u32); 5] = [
        ("8D4840D6202CC371C32CE0576098", 0x4840D6),
        ("8D40621D58C382D690C8AC2863A7", 0x40621D),
        ("8D40621D58C386435CC412692AD6", 0x40621D),
        ("8D485020994409940838175B284F", 0x485020),
        ("8DA05F219B06B6AF189400CBC33F", 0xA05F21),
    ];

    #[test]
    fn vectors_pass_crc_and_extract_df_icao() {
        for (hex, icao) in VECTORS {
            let f = long(hex);
            assert!(f.crc_ok(), "{hex}");
            assert_eq!(f.df(), 17);
            assert_eq!(f.icao(), Some(icao));
            assert_eq!(f.recovered_address(), None);
        }
    }

    #[test]
    fn any_single_bit_flip_fails_crc() {
        let FrameData::Long(good) = long(VECTORS[0].0) else {
            unreachable!()
        };
        for bit in 0..112 {
            let mut b = good;
            b[bit / 8] ^= 0x80 >> (bit % 8);
            assert_ne!(FrameData::Long(b).crc_remainder(), 0, "bit {bit}");
        }
    }

    #[test]
    fn synthetic_df4_recovers_address() {
        let f = frame(4 << 3, &[0x12, 0x34, 0x56], 0xABCDEF);
        assert!(matches!(f, FrameData::Short(_)));
        assert_eq!(f.recovered_address(), Some(0xABCDEF));
        assert_eq!(f.icao(), None);
    }

    #[test]
    fn synthetic_long_ap_formats_recover_address() {
        for df in [16u8, 20, 21] {
            let f = frame(df << 3, &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 0x123456);
            assert_eq!(f.recovered_address(), Some(0x123456), "DF{df}");
        }
        for df in [0u8, 5] {
            let f = frame(df << 3, &[9, 8, 7], 0x00FF01);
            assert_eq!(f.recovered_address(), Some(0x00FF01), "DF{df}");
        }
    }

    #[test]
    fn synthetic_df11_iid_and_acquisition() {
        let aa = [0xAB, 0xCD, 0xEF];
        let f = frame(11 << 3 | 5, &aa, 5);
        assert_eq!(f.crc_remainder(), 5);
        assert_eq!(f.df11_iid(), Some(5));
        assert!(!f.crc_ok());
        assert_eq!(f.icao(), Some(0xABCDEF));

        let f = frame(11 << 3 | 5, &aa, 0);
        assert!(f.crc_ok());
        assert_eq!(f.df11_iid(), None);

        // remainder above 7 bits is not an IID
        assert_eq!(frame(11 << 3, &aa, 0x80).df11_iid(), None);
    }

    #[test]
    fn length_mismatch_yields_nothing() {
        let f = FrameData::Short([0x8D, 0, 0, 0, 0, 0, 0]); // DF17 but short
        assert!(!f.crc_ok());
        assert_eq!(f.icao(), None);
        let f = FrameData::Long([4 << 3; 14]); // DF4 but long
        assert_eq!(f.recovered_address(), None);
    }
}
