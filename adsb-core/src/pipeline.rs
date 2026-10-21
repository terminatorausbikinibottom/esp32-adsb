//! The single ingest path for firmware and simulator: AVR bytes -> `AvrSource` -> `decode` ->
//! `AircraftTable::update`, plus counters.
//!
//! Time: the tracker needs one monotonic millisecond clock for both CPR pair windows and
//! snapshots, so frames are stamped on the caller's clock (`now_ms` of the `feed` call):
//! - Frames with a 12 MHz `@` timestamp keep their *relative* spacing: the first one (and the
//!   first after a backwards jump, R25) is anchored to `now_ms`, later ones are `anchor + tick
//!   delta`, with the 48-bit wrap handled. This keeps a TCP batch of frames from collapsing to
//!   one instant, which would break the 10 s even/odd window and dead reckoning.
//! - The result is clamped to `now_ms` (a frame cannot have arrived in the future) and never
//!   goes backwards.
//! - Frames without a timestamp (`*` lines) and other clock kinds are stamped `now_ms`.
//!
//! Callers must pass a non-decreasing `now_ms` to `feed` and `snapshot`.

use crate::tracker::{self, AircraftTable, Config, Snapshot};
use modes::{AvrSource, ClockKind, Counters as SourceCounters, FrameSource, RawFrame, decode};

const TICKS_PER_MS: u64 = 12_000;
const TICK_MASK: u64 = (1 << 48) - 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub source: SourceCounters,
    pub tracker: tracker::Counters,
    /// Frames `decode` rejected (failed CRC, unsupported DF).
    pub undecoded: u32,
    /// Timestamp jumps backwards that re-anchored the clock.
    pub clock_resets: u32,
}

pub struct FeedPipeline {
    source: AvrSource,
    table: AircraftTable,
    clock: Clock,
    undecoded: u32,
}

#[derive(Default)]
struct Clock {
    /// Last counter value, and caller-clock ms plus counter ticks since the anchor.
    last_ticks: Option<u64>,
    anchor_ms: u64,
    ticks_since_anchor: u64,
    last_ms: u64,
    resets: u32,
}

impl Clock {
    fn stamp(&mut self, f: &RawFrame, now_ms: u64) -> u64 {
        let ticks = f
            .timestamp
            .filter(|t| t.clock == ClockKind::Counter12MHz)
            .map(|t| t.ticks);
        let ms = match ticks {
            None => now_ms,
            Some(t) => {
                let delta = self.last_ticks.map(|p| t.wrapping_sub(p) & TICK_MASK);
                match delta {
                    Some(d) if d < TICK_MASK / 2 => self.ticks_since_anchor += d,
                    first_or_backwards => {
                        self.resets += first_or_backwards.is_some() as u32;
                        self.anchor_ms = now_ms.max(self.last_ms);
                        self.ticks_since_anchor = 0;
                    }
                }
                self.last_ticks = Some(t);
                self.anchor_ms + self.ticks_since_anchor / TICKS_PER_MS
            }
        };
        self.last_ms = ms.min(now_ms).max(self.last_ms);
        self.last_ms
    }
}

impl FeedPipeline {
    pub fn new(cfg: Config) -> Self {
        Self {
            source: AvrSource::new(),
            table: AircraftTable::new(cfg),
            clock: Clock::default(),
            undecoded: 0,
        }
    }

    /// Feed bytes read from the transport (any chunking). `now_ms` is the arrival time.
    pub fn feed(&mut self, bytes: &[u8], now_ms: u64) {
        let Self {
            source,
            table,
            clock,
            undecoded,
        } = self;
        source.feed(bytes, &mut |f: RawFrame| match decode(&f) {
            Some(m) => table.update(&m, clock.stamp(&f, now_ms)),
            None => *undecoded = undecoded.saturating_add(1),
        });
    }

    /// Call on every transport (re)connect: the source's counter may have restarted or jumped,
    /// so the next stamped frame is re-anchored to its `now_ms` instead of continuing the old
    /// timeline (which would clamp every later frame to `now_ms` and collapse their spacing).
    /// Residual: frames of one `feed` call that are >10 s apart in source time are still
    /// stamped identically when the call's `now_ms` is earlier than their spacing implies
    /// (the clamp to `now_ms`); a live socket delivers at most a few ms of backlog.
    pub fn reset_clock(&mut self) {
        self.clock.last_ticks = None;
    }

    /// Age out stale entries, then build the vicinity snapshot.
    pub fn snapshot(&mut self, now_ms: u64, radius_km: f64) -> Snapshot {
        self.table.age(now_ms);
        self.table.snapshot(now_ms, radius_km)
    }

    pub fn counters(&self) -> Counters {
        Counters {
            source: self.source.counters(),
            tracker: self.table.counters(),
            undecoded: self.undecoded,
            clock_resets: self.clock.resets,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use modes::{Timestamp, encode};

    fn line(f: modes::FrameData, ticks: u64) -> std::string::String {
        let hex: std::string::String = f.bytes().iter().map(|b| format!("{b:02X}")).collect();
        format!("@{ticks:012X}{hex};\n")
    }

    const CFG: Config = Config {
        rx_lat_deg: 47.0,
        rx_lon_deg: -30.0,
        rx_elev_m: 0.0,
    };

    fn raw(ticks: u64) -> RawFrame {
        RawFrame {
            data: encode::all_call(1, 5),
            timestamp: Some(Timestamp {
                ticks,
                clock: ClockKind::Counter12MHz,
            }),
            signal: None,
        }
    }

    #[test]
    fn clock_keeps_spacing_clamps_and_handles_wrap_and_backwards_jump() {
        let mut c = Clock::default();
        let start = TICK_MASK - 6_000; // wraps after 0.5 ms
        assert_eq!(c.stamp(&raw(start), 1000), 1000);
        // 2 s of ticks later, but the caller only advanced 1.5 s: clamped to now
        let t2 = (start + 24_000_000) & TICK_MASK;
        assert_eq!(c.stamp(&raw(t2), 2500), 2500);
        // ticks are the clock when the caller clock is ahead
        assert_eq!(c.stamp(&raw((t2 + 12_000 * 40) & TICK_MASK), 9000), 3040);
        let mut c = Clock::default();
        c.stamp(&raw(start), 1000);
        assert_eq!(
            c.stamp(&raw((start + 12_000 * 700) & TICK_MASK), 9000),
            1700
        );
        // backwards jump: re-anchor at the caller clock, still monotonic
        assert_eq!(c.stamp(&raw(5), 9500), 9500);
        assert_eq!(c.resets, 1);
        // unstamped frames use the caller clock
        let mut f = raw(0);
        f.timestamp = None;
        assert_eq!(c.stamp(&f, 9600), 9600);
    }

    #[test]
    fn reset_clock_reanchors_after_a_counter_jump() {
        let ms = |c: &mut Clock, t: u64, now| c.stamp(&raw(t), now);
        let mut c = Clock::default();
        ms(&mut c, 1_000, 10_000);
        // Reconnect to a source whose counter is an hour ahead, no reset: the new timeline is
        // far in the future, so frames 30 s apart both clamp to `now`.
        let far = 12_000 * 3_600_000;
        assert_eq!(ms(&mut c, far, 20_000), 20_000);
        assert_eq!(ms(&mut c, far + 12_000 * 30_000, 50_000), 50_000);
        assert_eq!(ms(&mut c, far + 12_000 * 60_000, 50_001), 50_001);
        // With a reset the first frame anchors at `now` and the 30 s spacing survives.
        let mut c = Clock::default();
        ms(&mut c, 1_000, 10_000);
        c.last_ticks = None; // what FeedPipeline::reset_clock does
        assert_eq!(ms(&mut c, far, 20_000), 20_000);
        assert_eq!(ms(&mut c, far + 12_000 * 30_000, 50_000), 50_000);
        assert_eq!(ms(&mut c, far + 12_000 * 40_000, 60_000), 60_000);
        assert_eq!(c.resets, 0);
    }

    #[test]
    fn reset_clock_on_pipeline_does_not_count_as_backwards_jump() {
        let mut p = FeedPipeline::new(CFG);
        p.feed(
            line(encode::identification(0xABCDEF, "A", None), 900).as_bytes(),
            5,
        );
        p.reset_clock();
        p.feed(
            line(encode::identification(0xABCDEF, "A", None), 5).as_bytes(),
            10,
        );
        assert_eq!(p.counters().clock_resets, 0);
        assert_eq!(p.counters().tracker.messages, 2);
    }

    #[test]
    fn feed_decodes_in_chunks_and_counts() {
        let mut p = FeedPipeline::new(CFG);
        let mut bytes = line(encode::identification(0xABCDEF, "TEST1", None), 100).into_bytes();
        let mut bad = line(encode::identification(0xABCDEF, "TEST2", None), 200).into_bytes();
        bad[20] = if bad[20] == b'0' { b'1' } else { b'0' }; // CRC fails
        bytes.extend(bad);
        bytes.extend(b"*5d4840d6202cc3;\n"); // short frame, DF11 with a bad remainder
        for chunk in bytes.chunks(7) {
            p.feed(chunk, 50);
        }
        let c = p.counters();
        assert_eq!(c.source.accepted, 3);
        assert_eq!(c.tracker.messages, 1);
        assert!(c.undecoded >= 1);
        let s = p.snapshot(50, 5.0);
        assert_eq!(s.list.len(), 1);
        assert_eq!(s.list[0].callsign.as_deref(), Some("TEST1"));
    }
}
