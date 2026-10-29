//! Every diacritic in the panel font must render as ink, not as a missing glyph. Own binary: the
//! Slint platform can be installed once.
use adsb_app::{Status, WIDTH};
use adsb_core::meta::{Airport, FocusMeta};
use adsb_core::scenario::Kind;
use adsb_core::tracker::{AircraftView, Card, Counters, Marker, PositionView, Snapshot};
use sim::Sim;

/// The city row of the info panel (x 456..800, y 32 + 383 ..+19).
fn city_row(sim: &Sim) -> Vec<u16> {
    (415..434)
        .flat_map(|y| (456..WIDTH).map(move |x| (x, y)))
        .map(|(x, y)| sim.framebuffer()[y * WIDTH + x].0)
        .collect()
}

#[test]
fn diacritics_render() {
    let mut sim = Sim::new(Kind::Overflight, 1);
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
    snap.list
        .push(AircraftView {
            icao: 0xA00001,
            callsign: None,
            category: None,
            squawk: None,
            altitude_ft: None,
            vertical_rate_fpm: None,
            ground_speed_kt: None,
            track_deg: None,
            last_seen_age_ms: 0,
            position: Some(PositionView {
                ground_m: 2000.0,
                bearing_deg: 0.0,
                height_m: None,
                elevation_deg: None,
                marker: Marker::Exact,
                provisional: false,
                age_ms: 0,
                in_radius: false,
                approach: None,
            }),
        })
        .unwrap();
    let st = Status {
        now_ms: 1000,
        wifi_ok: true,
        feed_ok: true,
        radius_km: 5.0,
    };
    let row = |sim: &mut Sim, c: char| {
        sim.reset(Kind::Overflight, 1);
        let m = FocusMeta {
            destination: Some(Airport {
                iata: None,
                municipality: Some(format!("X{c}X")),
            }),
            ..Default::default()
        };
        sim.app.update(&snap, &st, Some(&m));
        sim.render();
        city_row(sim)
    };
    let missing = row(&mut sim, '\u{4e2d}'); // CJK: not in the font
    assert_ne!(
        missing,
        row(&mut sim, ' '),
        "baseline: a missing glyph must differ from a space or the check is blind"
    );
    let chars = "°→↑↓·±×−–—©’áàâäãåæçéèêëíìîïñóòôöõøúùûüýþÿÁÀÂÄÃÅÆÇÉÈÊËÍÌÎÏÑÓÒÔÖÕØÚÙÛÜÝÞßðÐœŒĀāĂăĄąĆćČčĎďĐđĒēĖėĘęĚěĞğĪīĮįİıŁłŃńŇňŌōŐőŘřŚśŞşŠšŤťŪūŮůŰűŲųŹźŻżŽž";
    let mut seen = std::collections::HashSet::new();
    for c in chars.chars() {
        let r = row(&mut sim, c);
        assert_ne!(r, missing, "{c} renders like a missing glyph");
        seen.insert(r);
    }
    assert!(
        seen.len() > chars.chars().count() * 9 / 10,
        "glyphs look alike: {}",
        seen.len()
    );
}
