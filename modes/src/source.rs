use crate::frame::{ClockKind, FrameData, RawFrame, Timestamp};

/// Frame/reject counters kept by every source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub accepted: u32,
    pub malformed: u32,
    pub mode_ac_skipped: u32,
}

/// Sans-IO adapter from transport input to raw Mode S frames. The caller owns the transport
/// and passes whatever it read, in chunks of any size.
///
/// Sketches of deferred adapters:
/// - `BeastSource` (`Input = u8`): unescapes 0x1A framing, maps the 6-byte MLAT counter to
///   `Timestamp { clock: Counter12MHz }` and the signal byte to `SignalLevel`.
/// - `Demodulator` (`Input = IQ sample`): finds preambles and slices bits, stamping frames
///   with `Timestamp { clock: SampleIndex { rate_hz } }`; `signal` is derived from magnitude.
pub trait FrameSource {
    type Input;

    fn feed(&mut self, input: &[Self::Input], emit: &mut impl FnMut(RawFrame));
    fn counters(&self) -> Counters;
}

const TS_HEX: usize = 12;
const MAX_BODY_HEX: usize = 28;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// Between frames: CR/LF ignored, a marker starts a frame, anything else is garbage.
    Idle,
    /// Discarding garbage until the next marker or LF.
    Skip,
    /// Reading the `@` timestamp.
    Ts,
    Body,
}

/// Parser for the dump1090/fr24feed AVR text form (port 30002): `@<12 hex ts><14|28 hex>;`
/// (the only form the target feed sends) and `*<14|28 hex>;`. Four hex digits (Mode A/C) are
/// skipped and counted. Memory use is fixed.
pub struct AvrSource {
    state: State,
    ts: u64,
    ts_len: usize,
    has_ts: bool,
    body: [u8; MAX_BODY_HEX],
    body_len: usize,
    counters: Counters,
}

impl Default for AvrSource {
    fn default() -> Self {
        Self::new()
    }
}

fn hex(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

impl AvrSource {
    pub const fn new() -> Self {
        Self {
            state: State::Idle,
            ts: 0,
            ts_len: 0,
            has_ts: false,
            body: [0; MAX_BODY_HEX],
            body_len: 0,
            counters: Counters {
                accepted: 0,
                malformed: 0,
                mode_ac_skipped: 0,
            },
        }
    }

    fn malformed(&mut self) {
        self.counters.malformed = self.counters.malformed.saturating_add(1);
        self.state = State::Skip;
    }

    fn start(&mut self, has_ts: bool) {
        self.has_ts = has_ts;
        self.ts = 0;
        self.ts_len = 0;
        self.body_len = 0;
        self.state = if has_ts { State::Ts } else { State::Body };
    }

    fn finish(&mut self, emit: &mut impl FnMut(RawFrame)) {
        let n = self.body_len;
        self.state = State::Idle;
        if n == 4 {
            self.counters.mode_ac_skipped = self.counters.mode_ac_skipped.saturating_add(1);
            return;
        }
        if n != 14 && n != 28 {
            return self.malformed();
        }
        let mut bytes = [0u8; 14];
        for (dst, pair) in bytes.iter_mut().zip(self.body[..n].as_chunks::<2>().0) {
            *dst = pair[0] << 4 | pair[1];
        }
        let data = if n == 14 {
            FrameData::Short(bytes[..7].try_into().unwrap())
        } else {
            FrameData::Long(bytes)
        };
        let timestamp = self.has_ts.then_some(Timestamp {
            ticks: self.ts,
            clock: ClockKind::Counter12MHz,
        });
        self.counters.accepted = self.counters.accepted.saturating_add(1);
        emit(RawFrame {
            data,
            timestamp,
            signal: None,
        });
    }

    fn push(&mut self, b: u8, emit: &mut impl FnMut(RawFrame)) {
        // A marker always resyncs, even mid-frame.
        if b == b'*' || b == b'@' {
            if matches!(self.state, State::Ts | State::Body) {
                self.counters.malformed = self.counters.malformed.saturating_add(1);
            }
            return self.start(b == b'@');
        }
        match self.state {
            State::Idle => match b {
                b'\r' | b'\n' => {}
                _ => self.malformed(),
            },
            State::Skip => {
                if b == b'\n' {
                    self.state = State::Idle;
                }
            }
            State::Ts => match hex(b) {
                Some(d) => {
                    self.ts = self.ts << 4 | d as u64;
                    self.ts_len += 1;
                    if self.ts_len == TS_HEX {
                        self.state = State::Body;
                    }
                }
                None => self.malformed(),
            },
            State::Body => match (b, hex(b)) {
                (b';', _) => self.finish(emit),
                (_, Some(d)) if self.body_len < MAX_BODY_HEX => {
                    self.body[self.body_len] = d;
                    self.body_len += 1;
                }
                _ => self.malformed(), // non-hex, CR/LF before `;`, or overlong
            },
        }
    }
}

impl FrameSource for AvrSource {
    type Input = u8;

    fn feed(&mut self, input: &[u8], emit: &mut impl FnMut(RawFrame)) {
        for &b in input {
            self.push(b, emit);
        }
    }

    fn counters(&self) -> Counters {
        self.counters
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LONG: &str = "8D4840D6202CC371C32CE0576098";
    const SHORT: &str = "5D4840D6202CC3";
    const TS: &str = "0123456789AB";

    fn run(chunks: &[&[u8]]) -> (std::vec::Vec<RawFrame>, Counters) {
        let mut s = AvrSource::new();
        let mut out = std::vec::Vec::new();
        for c in chunks {
            s.feed(c, &mut |f| out.push(f));
        }
        (out, s.counters())
    }

    fn unhex<const N: usize>(s: &str) -> [u8; N] {
        core::array::from_fn(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap())
    }

    #[test]
    fn star_long_and_short() {
        let l = std::format!("*{LONG};\n*{SHORT};\r\n");
        let (f, c) = run(&[l.as_bytes()]);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].data, FrameData::Long(unhex(LONG)));
        assert_eq!(f[0].timestamp, None);
        assert_eq!(f[1].data, FrameData::Short(unhex(SHORT)));
        assert_eq!(
            c,
            Counters {
                accepted: 2,
                ..Default::default()
            }
        );
    }

    #[test]
    fn at_form_with_timestamp() {
        let l = std::format!("@{TS}{LONG};\n@{TS}{SHORT};\n");
        let (f, c) = run(&[l.as_bytes()]);
        assert_eq!(f.len(), 2);
        let ts = Some(Timestamp {
            ticks: 0x0123_4567_89AB,
            clock: ClockKind::Counter12MHz,
        });
        assert_eq!(f[0].timestamp, ts);
        assert_eq!(f[0].data, FrameData::Long(unhex(LONG)));
        assert_eq!(f[1].data, FrameData::Short(unhex(SHORT)));
        assert_eq!(c.accepted, 2);
    }

    #[test]
    fn lowercase_hex_and_crlf() {
        let l = std::format!("@{TS}{};\r\n", LONG.to_lowercase());
        let (f, _) = run(&[l.as_bytes()]);
        assert_eq!(f[0].data, FrameData::Long(unhex(LONG)));
    }

    #[test]
    fn byte_at_a_time_and_every_split_match_whole() {
        let l = std::format!("@{TS}{LONG};\n*{SHORT};\r\n@{TS}{SHORT};\n");
        let (whole, wc) = run(&[l.as_bytes()]);
        assert_eq!(whole.len(), 3);
        let bytes: std::vec::Vec<&[u8]> = l.as_bytes().chunks(1).collect();
        assert_eq!(run(&bytes), (whole.clone(), wc));
        for i in 0..l.len() {
            let (a, b) = l.as_bytes().split_at(i);
            assert_eq!(run(&[a, b]), (whole.clone(), wc));
        }
    }

    #[test]
    fn mode_ac_skipped_and_counted() {
        let l = std::format!("@{TS}1A2B;\n*0123;\n@{TS}{LONG};\n");
        let (f, c) = run(&[l.as_bytes()]);
        assert_eq!(f.len(), 1);
        assert_eq!(
            c,
            Counters {
                accepted: 1,
                malformed: 0,
                mode_ac_skipped: 2
            }
        );
    }

    #[test]
    fn rejects_bad_lengths_and_non_hex() {
        for bad in [
            "*8D4840;",
            "*8D4840D6202CC371;",
            "*8D4840D6202CC371C32CE057609;",
            "*8D48G0D6202CC3;",
        ] {
            let l = std::format!("{bad}\n*{SHORT};\n");
            let (f, c) = run(&[l.as_bytes()]);
            assert_eq!((f.len(), c.malformed), (1, 1), "{bad}");
        }
        // bad timestamp
        let l = std::format!("@01X3456789AB{LONG};\n*{SHORT};\n");
        let (f, c) = run(&[l.as_bytes()]);
        assert_eq!((f.len(), c.malformed), (1, 1));
    }

    #[test]
    fn overlong_garbage_resyncs_at_next_marker() {
        let junk = "A".repeat(500);
        let l = std::format!("*{junk};\n@{TS}{LONG};\n{junk}@{TS}{SHORT};\n");
        let (f, c) = run(&[l.as_bytes()]);
        assert_eq!(f.len(), 2);
        assert_eq!(c.malformed, 2);
    }

    #[test]
    fn truncated_frame_then_marker_resyncs() {
        let l = std::format!("@{TS}8D48@{TS}{LONG};\n");
        let (f, c) = run(&[l.as_bytes()]);
        assert_eq!((f.len(), c.malformed), (1, 1));
    }

    #[test]
    fn newline_before_semicolon_is_malformed() {
        let l = std::format!("*{LONG}\n*{SHORT};\n");
        let (f, c) = run(&[l.as_bytes()]);
        assert_eq!((f.len(), c.malformed), (1, 1));
    }

    #[test]
    fn random_bytes_never_panic() {
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        let mut s = AvrSource::new();
        let alphabet = b"*@;\r\n0123456789abcdefABCDEFxyz \0\xff";
        for _ in 0..200_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            // mix of fully random bytes and protocol-flavoured ones
            let b = if x & 1 == 0 {
                (x >> 8) as u8
            } else {
                alphabet[(x >> 8) as usize % alphabet.len()]
            };
            s.feed(&[b], &mut |_| {});
        }
        // still usable afterwards
        let mut n = 0;
        s.feed(std::format!("\n*{SHORT};\n").as_bytes(), &mut |_| n += 1);
        assert_eq!(n, 1);
    }
}
