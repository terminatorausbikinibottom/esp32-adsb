//! Prints the frame hash for each overflight keyframe (same ones as `snapshots.rs`).
//! Compare with the browser: pick overflight, "Go to t (s)" = the keyframe second, "Dump frame".
//! Run `cargo test --test hash -- --nocapture` to see them; also written to
//! `target/ui-snapshots/hashes.txt`. Own binary: the Slint platform can be installed once.
use adsb_core::scenario::{Kind, overflight as t};
use sim::Sim;

#[test]
fn keyframe_hashes_are_stable_and_distinct() {
    let frames = [
        ("1-quiet", 20_000),
        ("2-approaching", t::APPROACHING_MS + 15_000),
        ("3-overhead", t::OVERHEAD_MS + 10_000),
        ("4-just-passed", t::JUST_PASSED_MS + 10_000),
        ("5-quiet-after", t::QUIET_MS + 10_000),
        ("6-no-data", t::NO_DATA_MS + 5_000),
    ];
    let mut sim = Sim::new(Kind::Overflight, 1);
    let mut report = String::new();
    let mut hashes = Vec::new();
    for (name, at) in frames {
        sim.seek(Kind::Overflight, 1, at);
        let h = sim.frame_hash();
        // A second seek to the same time must reproduce the frame exactly.
        sim.seek(Kind::Overflight, 1, at);
        assert_eq!(h, sim.frame_hash(), "{name} not reproducible");
        report += &format!("{name} t={} s {h:016x}\n", at / 1000);
        hashes.push(h);
    }
    print!("{report}");
    const REF: [u64; 6] = [
        0xe17dac042ff64347,
        0x3add3e907e624f6f,
        0x50a6e2223a79619b,
        0xbf8dc80dbcf77ab9,
        0x7618ffea8d872a38,
        0x028cdeacffff413d,
    ];
    assert_eq!(hashes, REF, "reference hashes changed");

    // Whatever happened before (long run, other scenario, navigation), a seek is a fresh start.
    for ((name, at), want) in frames.iter().zip(REF) {
        sim.seek(Kind::Overflight, 1, 455_000);
        sim.seek(Kind::Overflight, 1, *at);
        assert_eq!(sim.frame_hash(), want, "{name} after long run");
        sim.seek(Kind::Busy, 1, 100_000);
        sim.seek(Kind::Overflight, 1, *at);
        assert_eq!(sim.frame_hash(), want, "{name} after scenario switch");
    }
    // Seeking to 0 repaints at once: no frame of the previous run (or live view) survives.
    sim.seek(Kind::Overflight, 1, 0);
    let h0 = sim.frame_hash();
    sim.seek(Kind::Overflight, 1, 455_000);
    assert_ne!(sim.frame_hash(), h0);
    sim.seek(Kind::Overflight, 1, 0);
    assert_eq!(sim.frame_hash(), h0, "stale frame after seek to 0");
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ui-snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("hashes.txt"), report).unwrap();
    hashes.sort_unstable();
    hashes.dedup();
    assert_eq!(hashes.len(), 6, "keyframes should render differently");
}
