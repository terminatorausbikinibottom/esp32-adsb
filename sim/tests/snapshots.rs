//! Renders keyframes (overflight states, busy, edge, range steps, track angles) to `target/ui-snapshots/*.png`.
use adsb_app::{Badge, CardKind, HEIGHT, Status, WIDTH};
use adsb_core::meta::FocusMeta;
use adsb_core::scenario::{Kind, overflight as t};
use adsb_core::tracker::{AircraftView, Card, Counters, Marker, PositionView, Snapshot};
use sim::{Sim, fixtures};
use std::path::{Path, PathBuf};
use std::rc::Rc;

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

fn pixel(sim: &Sim, x: i32, y: i32) -> [u8; 3] {
    let i = (y as usize * WIDTH + x as usize) * 4;
    sim.rgba()[i..i + 3].try_into().unwrap()
}

/// Synthetic aircraft: (ground distance m, bearing deg, track deg).
fn ac(n: u32, ground_m: f64, bearing_deg: f64, track_deg: Option<f32>) -> AircraftView {
    AircraftView {
        icao: 0xA00000 + n,
        callsign: Some(format!("ZQ{n:02}").as_str().try_into().unwrap()),
        category: None,
        squawk: None,
        altitude_ft: Some(30_000),
        vertical_rate_fpm: None,
        ground_speed_kt: track_deg.map(|_| 400.0),
        track_deg,
        last_seen_age_ms: 0,
        position: Some(PositionView {
            ground_m,
            bearing_deg,
            height_m: None,
            elevation_deg: None,
            marker: Marker::Exact,
            provisional: false,
            age_ms: 0,
            in_radius: false,
            approach: None,
        }),
    }
}

fn synthetic(list: impl IntoIterator<Item = AircraftView>) -> Box<Snapshot> {
    let mut snap = Box::new(Snapshot {
        card: Card::Quiet,
        list: Default::default(),
        in_radius: 0,
        primary: None,
        focus: Some(0),
        just_passed: None,
        approaching: Default::default(),
        counters: Counters::default(),
    });
    for a in list {
        snap.list.push(a).unwrap();
    }
    snap
}

/// Shows a synthetic snapshot on a fresh app state.
fn show(sim: &mut Sim, snap: &Snapshot, meta: Option<&FocusMeta>) {
    sim.reset(Kind::Overflight, 1);
    let st = Status {
        now_ms: 1000,
        wifi_ok: true,
        feed_ok: true,
        radius_km: 5.0,
    };
    sim.app.update(snap, &st, meta);
    sim.render();
}

// One test only: the Slint platform can be installed once per process.
#[test]
fn keyframes_render_to_png() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/ui-snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    let mut sim = Sim::new(Kind::Overflight, 1);
    let mut shots = Vec::new();

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
    let mut now = 0;
    for (name, at, kind) in keyframes {
        while now < at {
            now += 1000;
            sim.advance_to(now);
        }
        let ui = sim.app.ui();
        assert_eq!(ui.kind, kind, "{name}");
        assert!(distinct_colours(&sim) >= 4, "{name} looks blank");
        match kind {
            CardKind::Overhead => {
                // The focus ring is drawn around the focus blip, and the panel shows its callsign.
                let f = ui.blips.iter().find(|b| b.focus).expect("focus blip");
                assert_eq!(ui.title, f.label.as_str());
                assert_eq!(ui.badge, Badge::Overhead);
                // Scenario fixtures: full metadata and the synthetic photo.
                assert_eq!(ui.airline, "Zeta Quebec Airways");
                assert_eq!(ui.model, "Example Aerospace Testliner 300 (TL30)");
                assert_eq!(ui.route, "AAA \u{2192} BBB");
                assert_eq!(ui.cities, "Alphaville \u{2192} Bravo Bay");
                assert_eq!(ui.credit, "Photo \u{a9} Jane Example / planespotters.net");
                assert!(ui.photo.is_some());
                // Photo at the top of the panel: its sky colour is at the panel's top-left corner.
                assert!(pixel(&sim, 448 + 8 + 4, 32 + 4 + 4)[2] > 150);
                let ring = pixel(&sim, f.x, 32 + f.y - 14);
                assert!(
                    ring[0] > 0xf0 && ring[1] > 0xc0 && ring[2] < 0x20,
                    "{ring:?}"
                );
                assert!(
                    (f.x - 224).abs() <= 22 && (f.y - 224).abs() <= 22,
                    "inside the 5 km ring"
                );
            }
            CardKind::NoData => assert!(ui.blips.is_empty()),
            _ => {}
        }
        shots.push(save(&sim, &dir, name));
    }

    // Every second of both scenarios renders: partial repaints with gaps between the dirty
    // rectangles hit an off-by-one in Slint 1.18.1 scene.rs (debug assertion) without a fix.
    sim.seek(Kind::Busy, 1, 300_000);
    sim.seek(Kind::Edge, 1, 120_000);
    sim.seek(Kind::Busy, 1, 100_000);
    assert!(
        sim.app.ui().blips.len() >= 15,
        "busy: {}",
        sim.app.ui().blips.len()
    );
    assert!(
        sim.app
            .ui()
            .blips
            .iter()
            .filter(|b| !b.label.is_empty())
            .count()
            <= 13
    );
    shots.push(save(&sim, &dir, "7-busy"));

    sim.seek(Kind::Edge, 1, 70_000);
    shots.push(save(&sim, &dir, "8-edge"));

    // Range steps: ten aircraft at the same distance pick the step (see radar tests).
    for (km, d_m) in [
        (10, 8_000.0),
        (25, 20_000.0),
        (50, 40_000.0),
        (100, 80_000.0),
    ] {
        let snap = synthetic((0..10).map(|n| ac(n, d_m, n as f64 * 36.0, Some(n as f32 * 36.0))));
        show(&mut sim, &snap, None);
        assert_eq!(sim.app.ui().range_label, format!("{km} km"));
        assert_eq!(sim.app.ui().blips.len(), 10);
        shots.push(save(&sim, &dir, &format!("9-range-{km}")));
    }

    // Tracks 0/45/90/180/270, no track (round), and a far one beyond the range.
    let spec = [
        (0.0, Some(0.0)),
        (72.0, Some(45.0)),
        (144.0, Some(90.0)),
        (216.0, Some(180.0)),
        (288.0, Some(270.0)),
        (340.0, None),
    ];
    let snap = synthetic(
        spec.iter()
            .enumerate()
            .map(|(n, &(b, t))| ac(n as u32, 6_000.0, b, t))
            .chain((0..4).map(|n| {
                ac(
                    10 + n,
                    8_500.0,
                    20.0 + n as f64 * 90.0,
                    Some(30.0 + n as f32 * 70.0),
                )
            }))
            .chain([ac(20, 30_000.0, 100.0, Some(10.0))]),
    );
    show(&mut sim, &snap, None);
    assert_eq!(sim.app.ui().beyond, "+1 beyond");
    let sprites: Vec<u8> = sim.app.ui().blips.iter().map(|b| b.sprite).collect();
    assert_eq!(sprites[..6], [0, 3, 6, 12, 18, 24]);
    shots.push(save(&sim, &dir, "10-tracks"));

    // Metadata states on a synthetic snapshot whose focus has a callsign.
    let one = || synthetic([ac(1, 1_500.0, 40.0, Some(90.0))]);
    let photo = fixtures::synthetic_photo();
    let with = |json, by: &str| fixtures::from_json(json, Some((by, Rc::clone(&photo))));

    let m = fixtures::from_json(fixtures::PRIVATE, None);
    show(&mut sim, &one(), Some(&m));
    let ui = sim.app.ui();
    assert_eq!(
        (ui.airline.as_str(), ui.route.as_str()),
        ("Example Flying Club", "\u{2014} \u{2192} \u{2014}")
    );
    assert!(ui.photo.is_none() && ui.credit.is_empty());
    shots.push(save(&sim, &dir, "11-meta-no-photo"));

    show(&mut sim, &one(), None);
    let ui = sim.app.ui();
    assert_eq!(
        (
            ui.airline.as_str(),
            ui.model.as_str(),
            ui.registration.as_str()
        ),
        ("\u{2014}", "\u{2014}", "\u{2014}")
    );
    assert_eq!(ui.title, "ZQ01");
    shots.push(save(&sim, &dir, "12-meta-not-found"));

    let m = with(fixtures::LONG_NAME, "A. Photographer-Wit...Long-Name");
    show(&mut sim, &one(), Some(&m));
    assert_eq!(
        sim.app.ui().credit,
        "Photo \u{a9} A. Photographer-Wit... / planespotters.net"
    );
    shots.push(save(&sim, &dir, "13-meta-long-names"));

    let m = with(fixtures::DIACRITICS, "Zoë Łukasiewicz");
    show(&mut sim, &one(), Some(&m));
    assert_eq!(
        sim.app.ui().cities,
        "Łódź \u{2192} Zürich · São Paulo · Kraków"
    );
    shots.push(save(&sim, &dir, "14-meta-diacritics"));

    // Meta without a focus is ignored.
    let mut none = one();
    none.focus = None;
    show(&mut sim, &none, Some(&m));
    assert_eq!(sim.app.ui().airline, "\u{2014}");

    for s in shots {
        let len = std::fs::metadata(&s).unwrap().len();
        assert!(len > 2000, "{s:?} is only {len} bytes");
    }
}
