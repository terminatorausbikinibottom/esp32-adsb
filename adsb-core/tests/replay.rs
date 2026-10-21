//! Local-only replay of a real 30002 capture (design step 2.11, R13). Never part of CI.
//!
//! `ADSB_REPLAY_CAPTURE=<path> ADSB_RX_LAT=.. ADSB_RX_LON=.. cargo test -- --ignored replay`
//!
//! Privacy: prints counts and distances only, never coordinates, ICAOs or callsigns.

use adsb_core::tracker::{AircraftTable, Config};
use modes::{AvrSource, Body, FrameSource, RawFrame, cpr_global, decode};
use std::collections::{HashMap, HashSet};

const TICKS_PER_MS: u64 = 12_000; // 12 MHz counter, R25
const TS_MASK: u64 = (1 << 48) - 1;
const SNAPSHOT_EVERY_MS: u64 = 10_000;
const RADIUS_KM: f64 = 20.0;

/// Frames with a monotonic millisecond clock derived from the `@` timestamps. The 48-bit counter
/// wrap is handled by modular deltas; a jump backwards (R25) is treated as no time elapsed.
fn load(path: &str) -> (Vec<(u64, RawFrame)>, AvrSource) {
    let bytes = std::fs::read(path).expect("read capture");
    let mut src = AvrSource::new();
    let mut frames = Vec::new();
    let (mut last_ticks, mut ticks_total, mut backwards) = (None::<u64>, 0u64, 0u32);
    for chunk in bytes.chunks(4096) {
        src.feed(chunk, &mut |f: RawFrame| {
            if let Some(t) = f.timestamp {
                if let Some(prev) = last_ticks {
                    let d = t.ticks.wrapping_sub(prev) & TS_MASK;
                    if d < TS_MASK / 2 {
                        ticks_total += d;
                    } else {
                        backwards += 1;
                    }
                }
                last_ticks = Some(t.ticks);
            }
            frames.push((ticks_total / TICKS_PER_MS, f));
        });
    }
    println!("timestamp jumps backwards: {backwards}");
    (frames, src)
}

/// Dev convenience only: without ADSB_RX_LAT/LON, use the median of globally decoded fixes as a
/// pseudo-receiver. Lives in memory, is never printed, and makes the range check approximate.
fn pseudo_receiver(frames: &[(u64, RawFrame)]) -> (f64, f64) {
    let mut last: HashMap<u32, [Option<(modes::CprPosition, u64)>; 2]> = HashMap::new();
    let (mut lats, mut lons) = (Vec::new(), Vec::new());
    for (ms, f) in frames {
        let Some(m) = decode(f) else { continue };
        let Body::AirbornePosition { cpr, .. } = m.body else {
            continue;
        };
        if !m.address.is_verified() {
            continue;
        }
        let e = last.entry(m.address.icao()).or_default();
        e[cpr.odd as usize] = Some((cpr, *ms));
        if let Some((o, t)) = e[!cpr.odd as usize]
            && ms - t <= 10_000
        {
            let (even, odd) = if cpr.odd { (o, cpr) } else { (cpr, o) };
            if let Some((la, lo)) = cpr_global(even, odd, cpr.odd) {
                lats.push(la);
                lons.push(lo);
            }
        }
    }
    assert!(
        !lats.is_empty(),
        "no global fixes to derive a pseudo-receiver"
    );
    let median = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v[v.len() / 2]
    };
    (median(&mut lats), median(&mut lons))
}

#[test]
#[ignore = "local only: needs ADSB_REPLAY_CAPTURE"]
fn replay() {
    let path = match std::env::var("ADSB_REPLAY_CAPTURE") {
        Ok(p) if std::path::Path::new(&p).is_file() => p,
        _ => {
            println!("SKIP: ADSB_REPLAY_CAPTURE unset or not a file");
            return;
        }
    };
    let (frames, src) = load(&path);
    let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f64>().ok());
    let (rx_lat_deg, rx_lon_deg) = match (env("ADSB_RX_LAT"), env("ADSB_RX_LON")) {
        (Some(a), Some(o)) => (a, o),
        _ => {
            println!("ADSB_RX_LAT/LON unset: using in-memory pseudo-receiver (dev convenience)");
            pseudo_receiver(&frames)
        }
    };
    let mut table = AircraftTable::new(Config {
        rx_lat_deg,
        rx_lon_deg,
        rx_elev_m: 0.0,
    });

    let (mut df_long, mut df_ok) = (0u32, 0u32);
    let mut max_ground_m = 0f64;
    let mut tracked: HashSet<u32> = HashSet::new();
    let mut next_snapshot = SNAPSHOT_EVERY_MS;
    let mut last_ms = 0;
    for (ms, f) in &frames {
        if matches!(f.data.df(), 17 | 18) && f.data.length_matches_df() {
            df_long += 1;
            df_ok += f.data.crc_ok() as u32;
        }
        if let Some(m) = decode(f) {
            table.update(&m, *ms);
        }
        if *ms >= next_snapshot {
            table.age(*ms);
            let snap = table.snapshot(*ms, RADIUS_KM);
            for a in &snap.list {
                if let (Some(p), Some(_)) = (&a.position, &a.callsign) {
                    max_ground_m = max_ground_m.max(p.ground_m);
                    tracked.insert(a.icao);
                }
            }
            next_snapshot = *ms + SNAPSHOT_EVERY_MS;
        }
        last_ms = *ms;
    }
    // Positions of aircraft without a callsign count for the range check too.
    let snap = table.snapshot(last_ms, RADIUS_KM);
    for a in &snap.list {
        if let Some(p) = &a.position {
            max_ground_m = max_ground_m.max(p.ground_m);
        }
    }

    let c = table.counters();
    let rate = df_ok as f64 / df_long.max(1) as f64;
    println!("frames: {} (source {:?})", frames.len(), src.counters());
    println!("duration: {} s", last_ms / 1000);
    println!(
        "DF17/18 CRC pass: {df_ok}/{df_long} ({:.1} %)",
        rate * 100.0
    );
    println!("aircraft with callsign and position: {}", tracked.len());
    println!("max resolved distance: {:.0} km", max_ground_m / 1000.0);
    println!(
        "known-ICAO filter drops: {}/{} ({:.1} %), implausible positions: {}",
        c.filtered,
        c.messages,
        c.filtered as f64 * 100.0 / c.messages.max(1) as f64,
        c.implausible
    );

    assert!(df_long > 0, "no DF17/18 frames in capture");
    assert!(rate > 0.95, "CRC pass rate too low");
    assert!(max_ground_m <= 450_000.0, "position beyond 450 km");
    assert!(
        tracked.len() >= 5,
        "fewer than 5 aircraft with callsign and position"
    );
}
