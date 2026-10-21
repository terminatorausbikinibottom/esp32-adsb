//! ADS-B (DF17 / DF18 CF=0) ME-field access and message parsing.

use crate::FrameData;
use heapless::String;

/// Callsign charset indexed by 6-bit code; `#` marks codes the spec leaves unused.
const CHARSET: &[u8; 64] = b"#ABCDEFGHIJKLMNOPQRSTUVWXYZ##### ###############0123456789######";

/// Emitter category set, selected by the identification type code (TC 4 = A ... TC 1 = D).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CategorySet {
    A,
    B,
    C,
    D,
}

/// Emitter category: set plus the 3-bit CA value (1..=7; CA 0 means "no information").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmitterCategory {
    pub set: CategorySet,
    pub ca: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identification {
    /// Up to 8 chars of `A-Z 0-9 space`, trailing spaces trimmed (may be empty).
    pub callsign: String<8>,
    /// `None` when CA = 0.
    pub category: Option<EmitterCategory>,
}

impl FrameData {
    /// The 56-bit ME field of an ADS-B frame: DF17, or DF18 with CF=0 (same ME format).
    pub fn me(&self) -> Option<&[u8; 7]> {
        match self {
            FrameData::Long(b) if b[0] >> 3 == 17 || b[0] == 18 << 3 => b[4..11].try_into().ok(),
            _ => None,
        }
    }

    /// ME type code (top 5 bits of ME). Does not check CRC.
    pub fn type_code(&self) -> Option<u8> {
        self.me().map(|me| me[0] >> 3)
    }

    /// Identification (TC 1-4). `None` for other type codes, or if any of the 8 characters
    /// is an unassigned 6-bit code: a CRC-valid frame with such a code is a non-compliant
    /// transmitter, so the whole callsign is rejected rather than shown with placeholders.
    pub fn identification(&self) -> Option<Identification> {
        let me = self.me()?;
        let set = match me[0] >> 3 {
            4 => CategorySet::A,
            3 => CategorySet::B,
            2 => CategorySet::C,
            1 => CategorySet::D,
            _ => return None,
        };
        let ca = me[0] & 7;
        let bits = me[1..].iter().fold(0u64, |a, &b| a << 8 | b as u64);
        let mut callsign = String::new();
        for i in 0..8 {
            match CHARSET[(bits >> (42 - 6 * i) & 0x3F) as usize] {
                b'#' => return None,
                c => callsign.push(c as char).ok()?,
            }
        }
        callsign.truncate(callsign.trim_end_matches(' ').len());
        Some(Identification {
            callsign,
            category: (ca != 0).then_some(EmitterCategory { set, ca }),
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

    /// DF17 frame with the given TC/CA and 8 six-bit codes.
    fn ident_frame(df_head: u8, tc: u8, ca: u8, codes: [u8; 8]) -> FrameData {
        let bits = codes.iter().fold(0u64, |a, &c| a << 6 | c as u64);
        let mut body = [0u8; 10];
        body[..3].copy_from_slice(&[0x12, 0x34, 0x56]); // AA
        body[3] = tc << 3 | ca;
        body[4..].copy_from_slice(&bits.to_be_bytes()[2..]);
        frame(df_head, &body, 0)
    }

    fn codes(s: &str) -> [u8; 8] {
        let mut c = [32u8; 8];
        for (o, ch) in c.iter_mut().zip(s.bytes()) {
            *o = CHARSET.iter().position(|&x| x == ch).unwrap() as u8;
        }
        c
    }

    #[test]
    fn klm1023_vector() {
        let f = long("8D4840D6202CC371C32CE0576098");
        assert_eq!(f.type_code(), Some(4));
        let id = f.identification().unwrap();
        assert_eq!(id.callsign.as_str(), "KLM1023");
        assert_eq!(
            id.category,
            Some(EmitterCategory {
                set: CategorySet::A,
                ca: 0
            })
            .filter(|c| c.ca != 0)
        );
    }

    #[test]
    fn all_type_codes_and_categories() {
        for (tc, set) in [
            (4, CategorySet::A),
            (3, CategorySet::B),
            (2, CategorySet::C),
            (1, CategorySet::D),
        ] {
            for ca in 0..8 {
                let f = ident_frame(17 << 3 | 5, tc, ca, codes("AB12"));
                let id = f.identification().unwrap();
                assert_eq!(id.callsign.as_str(), "AB12");
                let want = (ca != 0).then_some(EmitterCategory { set, ca });
                assert_eq!(id.category, want, "tc {tc} ca {ca}");
            }
        }
        for tc in (0..=31).filter(|t| !(1..=4).contains(t)) {
            assert_eq!(
                ident_frame(17 << 3, tc, 1, codes("X")).identification(),
                None
            );
        }
    }

    #[test]
    fn every_charset_code() {
        for code in 0..64u8 {
            let f = ident_frame(17 << 3, 4, 1, [code; 8]);
            let valid = matches!(code, 1..=26 | 32 | 48..=57);
            assert_eq!(f.identification().is_some(), valid, "code {code}");
            if valid {
                let want = match code {
                    1..=26 => (b'A' + code - 1) as char,
                    32 => ' ',
                    _ => (b'0' + code - 48) as char,
                };
                let cs = f.identification().unwrap().callsign;
                let exp: std::string::String = if want == ' ' {
                    "".into()
                } else {
                    std::iter::repeat_n(want, 8).collect()
                };
                assert_eq!(cs.as_str(), exp);
            }
        }
    }

    #[test]
    fn invalid_char_in_any_position_rejects() {
        for pos in 0..8 {
            let mut c = codes("ABCDEFGH");
            c[pos] = 0;
            assert_eq!(ident_frame(17 << 3, 4, 1, c).identification(), None);
        }
    }

    #[test]
    fn spaces() {
        let id = ident_frame(17 << 3, 4, 1, codes(""))
            .identification()
            .unwrap();
        assert_eq!(id.callsign.as_str(), "");
        let id = ident_frame(17 << 3, 4, 1, codes("AB CD"))
            .identification()
            .unwrap();
        assert_eq!(id.callsign.as_str(), "AB CD");
        let id = ident_frame(17 << 3, 4, 1, codes("ABCDEFGH"))
            .identification()
            .unwrap();
        assert_eq!(id.callsign.as_str(), "ABCDEFGH");
    }

    #[test]
    fn me_only_for_df17_and_df18_cf0() {
        assert!(ident_frame(17 << 3, 4, 1, codes("A")).me().is_some());
        assert!(
            ident_frame(18 << 3, 4, 1, codes("A"))
                .identification()
                .is_some()
        );
        // DF18 with CF=1 (TIS-B) is out of scope
        assert_eq!(ident_frame(18 << 3 | 1, 4, 1, codes("A")).me(), None);
        assert_eq!(ident_frame(20 << 3, 4, 1, codes("A")).me(), None);
        assert_eq!(FrameData::Short([0x88, 0, 0, 0, 0, 0, 0]).me(), None);
    }
}
