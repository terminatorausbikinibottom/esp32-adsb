//! Renders overflight keyframes (one per card state) to `target/ui-snapshots/*.png`.
use adsb_app::{CardKind, HEIGHT, PointerEvent, PointerPhase, Screen, WIDTH};
use adsb_core::scenario::{Kind, overflight as t};
use sim::Sim;
use std::path::{Path, PathBuf};

fn save(sim: &Sim, dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(format!("{name}.png"));
    let mut enc = png::Encoder::new(
        std::fs::File::create(&path).unwrap(),
        WIDTH as u32,
        HEIGHT as u32,
    );
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .unwrap()
        .write_image_data(&sim.rgba())
        .unwrap();
    path
}

fn distinct_colours(sim: &Sim) -> usize {
    let mut v: Vec<u16> = sim.framebuffer().iter().map(|p| p.0).collect();
    v.sort_unstable();
    v.dedup();
    v.len()
}

// One test only: the Slint platform can be installed once per process.
#[test]
fn overflight_keyframes_render_to_png() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/ui-snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    let mut sim = Sim::new(Kind::Overflight, 1);

    // (file, time, expected card kind); times are mid-state, well clear of the +-2 s edges.
    let keyframes = [
        ("1-quiet", 20_000, CardKind::Quiet),
        (
            "2-approaching",
            t::APPROACHING_MS + 15_000,
            CardKind::Approaching,
        ),
        ("3-overhead", t::OVERHEAD_MS + 10_000, CardKind::Overhead),
        (
            "4-just-passed",
            t::JUST_PASSED_MS + 10_000,
            CardKind::JustPassed,
        ),
        ("5-quiet-after", t::QUIET_MS + 10_000, CardKind::Quiet),
        ("6-no-data", t::NO_DATA_MS + 5_000, CardKind::NoData),
    ];
    let mut shots = Vec::new();
    let mut now = 0;
    for (name, at, kind) in keyframes {
        while now < at {
            now += 1000;
            sim.advance_to(now);
        }
        assert_eq!(sim.app.ui().kind, kind, "{name}");
        assert!(distinct_colours(&sim) >= 4, "{name} looks blank");
        shots.push(save(&sim, &dir, name));
    }

    for p in [PointerPhase::Down, PointerPhase::Up] {
        sim.pointer(PointerEvent {
            x: 724.0,
            y: 440.0,
            phase: p,
        });
    }
    assert_eq!(sim.app.screen(), Screen::List);
    shots.push(save(&sim, &dir, "7-list-placeholder"));

    for s in shots {
        let len = std::fs::metadata(&s).unwrap().len();
        assert!(len > 2000, "{s:?} is only {len} bytes");
    }
}
