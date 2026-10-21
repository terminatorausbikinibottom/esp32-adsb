//! Scenarios through `FeedPipeline` (design step S.1). Needs the `scenario` feature.
#![cfg(feature = "scenario")]

use adsb_core::pipeline::FeedPipeline;
use adsb_core::scenario::{Event, Kind, RADIUS_KM, Scenario, overflight as t};
use adsb_core::tracker::{Card, Snapshot};

/// Card state as the UI shows it. "Approaching" is the Quiet card with a non-empty
/// `approaching` list (design D8: nearest greyed + approaching hint); the tracker has no
/// separate card for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Quiet,
    Approaching,
    Overhead,
    JustPassed,
    NoData,
}

fn state(s: &Snapshot) -> State {
    match s.card {
        Card::NoData => State::NoData,
        Card::Overhead => State::Overhead,
        Card::JustPassed => State::JustPassed,
        Card::Quiet if !s.approaching.is_empty() => State::Approaching,
        Card::Quiet => State::Quiet,
    }
}

/// Run a scenario, snapshotting every `step_ms`; returns `(time, snapshot)` per step.
fn run(kind: Kind, seed: u64, step_ms: u64) -> Vec<(u64, Snapshot)> {
    let mut sc = Scenario::new(kind, seed);
    let mut p = FeedPipeline::new(sc.receiver());
    let mut out = Vec::new();
    let mut now = 0;
    while now <= sc.duration_ms() {
        sc.poll(now, &mut |at, ev| {
            if let Event::Line(l) = ev {
                p.feed(l.as_bytes(), at);
            }
        });
        out.push((now, p.snapshot(now, RADIUS_KM)));
        now += step_ms;
    }
    assert_eq!(p.counters().undecoded, 0);
    assert_eq!(p.counters().source.malformed, 0);
    assert_eq!(p.counters().tracker.filtered, 0);
    assert_eq!(p.counters().tracker.implausible, 0);
    assert_eq!(p.counters().clock_resets, 0);
    out
}

fn transitions(run: &[(u64, Snapshot)]) -> Vec<(u64, State)> {
    let mut v: Vec<(u64, State)> = Vec::new();
    for (now, s) in run {
        let st = state(s);
        if v.last().is_none_or(|l| l.1 != st) {
            v.push((*now, st));
        }
    }
    v
}

#[test]
fn overflight_walks_through_all_card_states_at_scripted_times() {
    let tr = transitions(&run(Kind::Overflight, 1, 1000));
    let want = [
        (0, State::Quiet),
        (t::APPROACHING_MS, State::Approaching),
        (t::OVERHEAD_MS, State::Overhead),
        (t::JUST_PASSED_MS, State::JustPassed),
        (t::QUIET_MS, State::Quiet),
        (t::NO_DATA_MS, State::NoData),
    ];
    assert_eq!(tr.len(), want.len(), "{tr:?}");
    for ((at, st), (want_at, want_st)) in tr.iter().zip(want) {
        assert_eq!(*st, want_st, "{tr:?}");
        assert!(
            at.abs_diff(want_at) <= 2000,
            "{st:?} at {at}, want {want_at}"
        );
    }
}

#[test]
fn overflight_ga_aircraft_is_listed_without_position() {
    let r = run(Kind::Overflight, 1, 1000);
    let (_, s) = &r[330];
    let ga: Vec<_> = s.list.iter().filter(|a| a.position.is_none()).collect();
    assert_eq!(ga.len(), 1, "{:?}", s.list.len());
    assert!((1400..=1600).contains(&ga[0].altitude_ft.unwrap()));
    assert!(ga[0].callsign.is_some()); // from its one DF17
}

#[test]
fn overhead_jet_is_at_fl350_and_primary() {
    let r = run(Kind::Overflight, 1, 1000);
    let (_, s) = &r[230];
    let a = &s.list[s.primary.unwrap()];
    assert_eq!(a.altitude_ft, Some(35_000));
    assert!(a.position.unwrap().ground_m < 2_000.0);
}

#[test]
fn same_seed_is_identical_regardless_of_poll_step_and_seeds_differ() {
    let collect = |seed, step| {
        let mut sc = Scenario::new(Kind::Busy, seed);
        let mut v = Vec::new();
        let mut now = 0;
        while now <= sc.duration_ms() + step {
            sc.poll(now, &mut |at, ev| v.push((at, format!("{ev:?}"))));
            now += step;
        }
        v
    };
    let a = collect(7, 1000);
    assert!(a.len() > 10_000);
    assert_eq!(a, collect(7, 1000));
    assert_eq!(a, collect(7, 333));
    assert_ne!(a, collect(8, 1000));
    assert!(a.windows(2).all(|w| w[0].0 <= w[1].0), "time order");
}

#[test]
fn feed_outage_events_bracket_the_silence() {
    let mut sc = Scenario::new(Kind::Overflight, 1);
    let mut ev = Vec::new();
    sc.poll(u64::MAX, &mut |at, e| match e {
        Event::Line(_) => ev.push((at, 'l')),
        Event::FeedLost => ev.push((at, 'L')),
        Event::FeedRestored => ev.push((at, 'R')),
    });
    let lost = ev.iter().position(|e| e.1 == 'L').unwrap();
    assert_eq!(ev[lost].0, t::FEED_LOST_MS);
    assert_eq!(ev[lost + 1], (t::FEED_RESTORED_MS, 'R'));
    assert_eq!(lost + 2, ev.len());
    assert!(ev[lost - 1].0 < t::FEED_LOST_MS);
}

#[test]
fn busy_tracks_30_aircraft() {
    let r = run(Kind::Busy, 3, 5000);
    let (_, s) = &r[20];
    assert_eq!(s.list.len(), 30);
    assert!(
        s.list
            .iter()
            .all(|a| a.position.is_some() && a.callsign.is_some())
    );
    assert_ne!(s.card, Card::NoData);
}

#[test]
fn edge_cases_are_present() {
    let r = run(Kind::Edge, 5, 1000);
    let (_, s) = &r[45];
    let find = |cs: &str| {
        s.list
            .iter()
            .find(|a| a.callsign.as_deref() == Some(cs))
            .unwrap()
    };
    assert!(find("ZQLONG99").position.is_some());
    assert!(find("ZQNOPOS1").position.is_none());
    assert!(find("ZQSTALE1").position.is_none(), "stale after 30 s");
    assert_eq!(find("ZQSOS777").squawk, Some(7700));
    assert!(find("ZQLONG99").callsign.as_ref().unwrap().len() == 8);
}
