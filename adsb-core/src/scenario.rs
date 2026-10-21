//! Deterministic, seeded scenarios (feature `scenario`) around a **fictitious** receiver.
//!
//! A scenario is a generator of `@`-timestamped AVR lines (so `AvrSource` is exercised too) plus
//! feed-outage markers. It allocates nothing and is pulled with [`Scenario::poll`], so it runs
//! on the device (`ADSB_DEMO`) as well as in the simulator. The output is a pure function of
//! `(kind, seed)`: polling in different step sizes gives the same events.
//!
//! The receiver is [`RX_LAT_DEG`], [`RX_LON_DEG`]: open Atlantic, not a real site. All aircraft,
//! ICAO addresses and callsigns are made up (callsigns start with `ZQ`).
//!
//! Timestamps are the 12 MHz counter, starting 100 s before its 48-bit wrap so the pipeline's
//! wrap handling is exercised.

use crate::tracker::Config;
use core::fmt::Write;
use heapless::{String, Vec};
use libm::{cos, round, sin};
use modes::{CategorySet, EmitterCategory, FrameData, encode};

/// Fictitious receiver: 47 N 30 W (mid-Atlantic).
pub const RX_LAT_DEG: f64 = 47.0;
pub const RX_LON_DEG: f64 = -30.0;
/// Radius the scenario milestones are defined for.
pub const RADIUS_KM: f64 = 5.0;

/// `overflight` script times (ms since scenario start); card states are expected to begin here.
/// Geometry: a 450 kt jet at FL350 on a 90 degree track passing over the receiver from 40 km
/// west, entering the 5 km radius 35 km in and leaving it 45 km in.
pub mod overflight {
    /// Quiet before: a distant aircraft flying away. The jet is first heard and approaching here.
    pub const APPROACHING_MS: u64 = 60_000;
    pub const OVERHEAD_MS: u64 = 211_200;
    pub const JUST_PASSED_MS: u64 = 254_400;
    /// A light aircraft sends short frames only from 300 s to 360 s; it never gets a position.
    pub const GA_START_MS: u64 = 300_000;
    pub const QUIET_MS: u64 = 314_400;
    pub const FEED_LOST_MS: u64 = 420_000;
    /// The tracker shows no data 30 s after the last frame.
    pub const NO_DATA_MS: u64 = 450_000;
    pub const FEED_RESTORED_MS: u64 = 460_000;
}

const TICK_MS: u64 = 500;
/// Frames of consecutive aircraft within one tick are spaced by this, to keep stamps monotonic.
const STAGGER_MS: u64 = 4;
const TICK_BASE: u64 = (1 << 48) - 100_000 * 12_000;
const M_PER_DEG: f64 = 111_195.0;
const KT_TO_MS: f64 = 1852.0 / 3600.0;
const MAX_TRACKS: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Overflight,
    /// 30 aircraft around the receiver for 5 minutes.
    Busy,
    /// Long callsigns, no position, stale position, squawk 7700.
    Edge,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event<'a> {
    /// One AVR line including the trailing `\n`.
    Line(&'a str),
    FeedLost,
    FeedRestored,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Full,
    /// Identification and velocity, never a position.
    NoPosition,
    /// One DF17 identification, then only short frames (DF4/DF11).
    ShortOnly,
}

struct Track {
    icao: u32,
    callsign: String<8>,
    mode: Mode,
    start_ms: u64,
    end_ms: u64,
    /// Position reports stop at this time (the aircraft then goes stale).
    pos_until_ms: u64,
    lat0: f64,
    lon0: f64,
    speed_kt: f32,
    track_deg: f32,
    alt_ft: i32,
    squawk: Option<u16>,
}

impl Track {
    fn position(&self, t_ms: u64) -> (f64, f64) {
        let d =
            t_ms.saturating_sub(self.start_ms) as f64 / 1000.0 * self.speed_kt as f64 * KT_TO_MS;
        let a = (self.track_deg as f64).to_radians();
        (
            self.lat0 + d * cos(a) / M_PER_DEG,
            self.lon0 + d * sin(a) / (M_PER_DEG * cos(self.lat0.to_radians())),
        )
    }
}

/// xorshift64*: small, deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        let mut r = Rng(seed ^ 0x9E37_79B9_7F4A_7C15 | 1);
        for _ in 0..4 {
            r.next();
        }
        r
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

pub struct Scenario {
    tracks: Vec<Track, MAX_TRACKS>,
    /// Feed outage `[start, end)`; no frames are produced inside it.
    outage: Option<(u64, u64)>,
    duration_ms: u64,
    /// Cursor: tick index and track index within the tick.
    k: u64,
    i: usize,
    /// 0 = `FeedLost` pending, 1 = `FeedRestored` pending, 2 = done.
    outage_stage: u8,
}

fn at_offset(north_m: f64, east_m: f64) -> (f64, f64) {
    (
        RX_LAT_DEG + north_m / M_PER_DEG,
        RX_LON_DEG + east_m / (M_PER_DEG * cos(RX_LAT_DEG.to_radians())),
    )
}

impl Scenario {
    pub fn new(kind: Kind, seed: u64) -> Self {
        let mut s = Scenario {
            tracks: Vec::new(),
            outage: None,
            duration_ms: 0,
            k: 0,
            i: 0,
            outage_stage: 2,
        };
        let mut rng = Rng::new(seed);
        match kind {
            Kind::Overflight => {
                use overflight::*;
                s.duration_ms = FEED_RESTORED_MS;
                s.outage = Some((FEED_LOST_MS, FEED_RESTORED_MS));
                s.outage_stage = 0;
                let (lat, lon) = at_offset(20_000.0, 0.0);
                s.add(&mut rng, "ZQB", Mode::Full, 0, lat, lon, 300.0, 0.0, 30_000);
                let (lat, lon) = at_offset(0.0, -40_000.0);
                s.add(
                    &mut rng,
                    "ZQJ",
                    Mode::Full,
                    APPROACHING_MS,
                    lat,
                    lon,
                    450.0,
                    90.0,
                    35_000,
                );
                // abeam by 2 km at 1500 ft; heard only through short frames
                let (lat, lon) = at_offset(2_000.0, -15_000.0);
                s.add(
                    &mut rng,
                    "ZQG",
                    Mode::ShortOnly,
                    GA_START_MS,
                    lat,
                    lon,
                    110.0,
                    90.0,
                    1_500,
                );
                s.tracks[2].end_ms = GA_START_MS + 60_000;
            }
            Kind::Busy => {
                s.duration_ms = 300_000;
                for _ in 0..30 {
                    let (r, a) = (rng.uniform(2_000.0, 80_000.0), rng.uniform(0.0, 360.0));
                    let a = a.to_radians();
                    let (lat, lon) = at_offset(r * cos(a), r * sin(a));
                    let (speed, track) = (rng.uniform(120.0, 500.0), rng.uniform(0.0, 360.0));
                    let alt = round(rng.uniform(2_000.0, 40_000.0) / 25.0) as i32 * 25;
                    s.add(
                        &mut rng,
                        "ZQ",
                        Mode::Full,
                        0,
                        lat,
                        lon,
                        speed as f32,
                        track as f32,
                        alt,
                    );
                }
            }
            Kind::Edge => {
                s.duration_ms = 120_000;
                let (lat, lon) = at_offset(8_000.0, -3_000.0);
                s.add(
                    &mut rng,
                    "ZQLONG",
                    Mode::Full,
                    0,
                    lat,
                    lon,
                    200.0,
                    45.0,
                    12_000,
                );
                s.tracks[0].callsign = fixed("ZQLONG99");
                s.add(
                    &mut rng,
                    "ZQNOPOS",
                    Mode::NoPosition,
                    0,
                    lat,
                    lon,
                    250.0,
                    180.0,
                    20_000,
                );
                s.tracks[1].callsign = fixed("ZQNOPOS1");
                // positions stop at 10 s: dead-reckoned, then stale after 30 s, removed at 60 s
                // of silence, but identification keeps it listed until the end
                let (lat, lon) = at_offset(-12_000.0, 6_000.0);
                s.add(
                    &mut rng,
                    "ZQSTALE",
                    Mode::Full,
                    0,
                    lat,
                    lon,
                    300.0,
                    270.0,
                    25_000,
                );
                s.tracks[2].pos_until_ms = 10_000;
                s.tracks[2].callsign = fixed("ZQSTALE1");
                let (lat, lon) = at_offset(4_000.0, 4_000.0);
                s.add(
                    &mut rng,
                    "ZQSOS",
                    Mode::Full,
                    0,
                    lat,
                    lon,
                    160.0,
                    315.0,
                    6_000,
                );
                s.tracks[3].squawk = Some(7700);
                s.tracks[3].callsign = fixed("ZQSOS777");
            }
        }
        s
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        rng: &mut Rng,
        prefix: &str,
        mode: Mode,
        start_ms: u64,
        lat0: f64,
        lon0: f64,
        speed_kt: f32,
        track_deg: f32,
        alt_ft: i32,
    ) {
        let icao = loop {
            let c = (rng.next() >> 24) as u32 & 0xFF_FFFF;
            if c != 0 && !self.tracks.iter().any(|t| t.icao == c) {
                break c;
            }
        };
        let mut callsign: String<8> = String::new();
        let _ = callsign.push_str(&prefix[..prefix.len().min(4)]);
        let _ = write!(callsign, "{}", rng.next() % 10_000);
        let _ = self.tracks.push(Track {
            icao,
            callsign,
            mode,
            start_ms,
            end_ms: self.duration_ms,
            pos_until_ms: u64::MAX,
            lat0,
            lon0,
            speed_kt,
            track_deg,
            alt_ft,
            squawk: None,
        });
    }

    /// The fictitious receiver, ready for `FeedPipeline::new`.
    pub fn receiver(&self) -> Config {
        Config {
            rx_lat_deg: RX_LAT_DEG,
            rx_lon_deg: RX_LON_DEG,
            rx_elev_m: 0.0,
        }
    }

    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    /// Emit, in time order, every event stamped at or before `until_ms` that has not been
    /// emitted yet. The `u64` is the frame's arrival time in ms since the scenario start: pass
    /// it as `now_ms` to `FeedPipeline::feed`. A line may be stamped up to
    /// `STAGGER_MS * MAX_TRACKS` after its tick, so call `poll` with the time you will snapshot at.
    pub fn poll(&mut self, until_ms: u64, out: &mut impl FnMut(u64, Event<'_>)) {
        loop {
            let tick_ms = self.k * TICK_MS;
            let block_ms =
                (tick_ms < self.duration_ms).then(|| tick_ms + self.i as u64 * STAGGER_MS);
            if let Some((lost, restored)) = self.outage.filter(|_| self.outage_stage < 2) {
                let (at, ev) = match self.outage_stage {
                    0 => (lost, Event::FeedLost),
                    _ => (restored, Event::FeedRestored),
                };
                if at <= until_ms && block_ms.is_none_or(|b| at <= b) {
                    out(at, ev);
                    self.outage_stage += 1;
                    continue;
                }
            }
            let Some(block_ms) = block_ms.filter(|&b| b <= until_ms) else {
                return;
            };
            if !self.outage.is_some_and(|(a, b)| (a..b).contains(&tick_ms)) {
                self.emit_track(self.i, tick_ms, block_ms, out);
            }
            self.i += 1;
            if self.i >= self.tracks.len() {
                self.i = 0;
                self.k += 1;
            }
        }
    }

    fn emit_track(
        &self,
        i: usize,
        tick_ms: u64,
        block_ms: u64,
        out: &mut impl FnMut(u64, Event<'_>),
    ) {
        let t = &self.tracks[i];
        if tick_ms < t.start_ms || tick_ms >= t.end_ms {
            return;
        }
        let k = tick_ms / TICK_MS;
        let first = tick_ms == t.start_ms;
        let mut frames: Vec<FrameData, 4> = Vec::new();
        let mut push = |f| {
            let _ = frames.push(f);
        };
        if first || (k.is_multiple_of(10) && t.mode != Mode::ShortOnly) {
            let category = EmitterCategory {
                set: CategorySet::A,
                ca: 3,
            };
            push(encode::identification(t.icao, &t.callsign, Some(category)));
        }
        if t.mode == Mode::ShortOnly {
            if !first && k.is_multiple_of(2) {
                push(encode::altitude_reply(t.icao, t.alt_ft));
            }
            if !first && k % 4 == 1 {
                push(encode::all_call(t.icao, 5));
            }
        } else {
            if k.is_multiple_of(2) {
                push(encode::velocity(t.icao, t.speed_kt, t.track_deg, 0));
            }
            if t.mode == Mode::Full && tick_ms < t.pos_until_ms {
                let (lat, lon) = t.position(tick_ms);
                push(encode::airborne_position(
                    t.icao,
                    t.alt_ft,
                    lat,
                    lon,
                    k % 2 == 1,
                ));
            }
            if let (Some(sq), true) = (t.squawk, k % 2 == 1) {
                push(encode::identity_reply(t.icao, sq));
            }
        }
        for (j, f) in frames.iter().enumerate() {
            let at = block_ms + j as u64;
            let ticks = (TICK_BASE + at * 12_000) & ((1 << 48) - 1);
            let mut line: String<48> = String::new();
            let _ = write!(line, "@{ticks:012X}");
            for b in f.bytes() {
                let _ = write!(line, "{b:02X}");
            }
            let _ = line.push_str(";\n");
            out(at, Event::Line(&line));
        }
    }
}

fn fixed(s: &str) -> String<8> {
    String::try_from(s).unwrap()
}
