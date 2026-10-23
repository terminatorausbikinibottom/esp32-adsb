//! Metadata of the focus aircraft (airline, type, route, photo): the model the panel shows,
//! tolerant parsers for the adsbdb and planespotters replies, an LRU cache with TTLs, the lookup
//! policy and the request rate limiter. Pure: no I/O, time is passed in as `now_ms`.
//!
//! API shapes were checked against the live services on 2026-10-23 (see the design doc, RD.7).

use crate::tracker::Snapshot;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use heapless::String as HString;
use serde::{Deserialize, Deserializer};

/// Replies larger than this are refused (real ones are 1-3 KB).
pub const MAX_BODY: usize = 16 * 1024;
/// The only host photo URLs may point to (planespotters image CDN).
pub const PHOTO_HOST_PREFIX: &str = "https://t.plnspttrs.net/";

/// Decoded photo, RGB8, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Photo {
    pub width: u16,
    pub height: u16,
    pub rgb: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct AircraftInfo {
    /// Model, e.g. "A319 112".
    #[serde(rename = "type")]
    pub model: Option<String>,
    pub icao_type: Option<String>,
    pub manufacturer: Option<String>,
    pub registration: Option<String>,
    pub registered_owner: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Airport {
    #[serde(rename = "iata_code")]
    pub iata: Option<String>,
    pub municipality: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
struct Airline {
    name: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct RouteInfo {
    #[serde(default, deserialize_with = "lenient")]
    airline: Option<Airline>,
    #[serde(default, deserialize_with = "lenient")]
    pub origin: Option<Airport>,
    #[serde(default, deserialize_with = "lenient")]
    pub destination: Option<Airport>,
}

impl RouteInfo {
    pub fn airline_name(&self) -> Option<&str> {
        self.airline.as_ref()?.name.as_deref()
    }
}

/// What the panel shows besides the tracker values. `None` fields show as "—".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FocusMeta {
    pub airline: Option<String>,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub icao_type: Option<String>,
    pub registration: Option<String>,
    pub origin: Option<Airport>,
    pub destination: Option<Airport>,
    pub photographer: Option<String>,
    /// Compared by pointer (`Rc::ptr_eq`) to detect a new photo.
    pub photo: Option<Rc<Photo>>,
}

impl FocusMeta {
    pub fn from_parts(a: Option<&AircraftInfo>, r: Option<&RouteInfo>) -> Self {
        let mut m = FocusMeta::default();
        if let Some(a) = a {
            m.manufacturer = a.manufacturer.clone();
            m.model = a.model.clone();
            m.icao_type = a.icao_type.clone();
            m.registration = a.registration.clone();
            // Fall back to the owner (private and cargo aircraft have no scheduled route).
            m.airline = a.registered_owner.clone();
        }
        if let Some(r) = r {
            m.airline = r.airline_name().map(String::from).or(m.airline);
            m.origin = r.origin.clone();
            m.destination = r.destination.clone();
        }
        m
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetaError {
    /// The service answered "unknown aircraft" / "unknown callsign" (or similar text).
    NotFound,
    Malformed,
    TooLarge,
}

/// A field that does not have the expected shape becomes `None` instead of failing the reply.
fn lenient<'de, D: Deserializer<'de>, T: serde::de::DeserializeOwned>(
    d: D,
) -> Result<Option<T>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).ok())
}

// Short-lived parse value: boxing would only add an allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Deserialize)]
#[serde(untagged)]
enum Reply {
    /// `{"response":"unknown aircraft"}`
    Text(#[allow(dead_code)] String),
    Data {
        #[serde(default, deserialize_with = "lenient")]
        aircraft: Option<AircraftInfo>,
        #[serde(default, deserialize_with = "lenient")]
        flightroute: Option<RouteInfo>,
    },
}

#[derive(Deserialize)]
struct AdsbDbBody {
    response: Reply,
}

/// Parses `/v0/aircraft/<icao>[?callsign=]` and `/v0/callsign/<cs>` replies. Note: with the
/// combined query an unknown callsign makes the whole reply "unknown callsign", even when the
/// aircraft is known, so callers retry without the callsign.
pub fn parse_adsbdb(body: &[u8]) -> Result<(Option<AircraftInfo>, Option<RouteInfo>), MetaError> {
    if body.len() > MAX_BODY {
        return Err(MetaError::TooLarge);
    }
    match serde_json::from_slice::<AdsbDbBody>(body) {
        Ok(AdsbDbBody {
            response:
                Reply::Data {
                    aircraft: None,
                    flightroute: None,
                },
        })
        | Ok(AdsbDbBody {
            response: Reply::Text(_),
        }) => Err(MetaError::NotFound),
        Ok(AdsbDbBody {
            response:
                Reply::Data {
                    aircraft,
                    flightroute,
                },
        }) => Ok((aircraft, flightroute)),
        Err(_) => Err(MetaError::Malformed),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhotoRef {
    /// `https://t.plnspttrs.net/...` image URL.
    pub src: String,
    pub photographer: String,
}

#[derive(Deserialize)]
struct Thumb {
    src: String,
}

#[derive(Deserialize)]
struct PsPhoto {
    thumbnail_large: Option<Thumb>,
    thumbnail: Option<Thumb>,
    #[serde(default)]
    photographer: String,
}

#[derive(Deserialize)]
struct PsBody {
    photos: Vec<PsPhoto>,
}

/// Parses `/pub/photos/hex/<icao>`: the first photo, large thumbnail preferred. `Ok(None)` = no
/// photo (empty array, or a URL outside [`PHOTO_HOST_PREFIX`]).
pub fn parse_planespotters(body: &[u8]) -> Result<Option<PhotoRef>, MetaError> {
    if body.len() > MAX_BODY {
        return Err(MetaError::TooLarge);
    }
    let b: PsBody = serde_json::from_slice(body).map_err(|_| MetaError::Malformed)?;
    Ok(b.photos.into_iter().next().and_then(|p| {
        let src = p.thumbnail_large.or(p.thumbnail)?.src;
        src.starts_with(PHOTO_HOST_PREFIX).then_some(PhotoRef {
            src,
            photographer: p.photographer,
        })
    }))
}

pub const TTL_ROUTE_MS: u64 = 6 * 3600 * 1000;
pub const TTL_AIRCRAFT_MS: u64 = 7 * 24 * 3600 * 1000;
pub const TTL_NEGATIVE_MS: u64 = 3600 * 1000;
pub const TEXT_CACHE_ENTRIES: usize = 64;
pub const PHOTO_CACHE_ENTRIES: usize = 4;

/// Least-recently-used cache with a per-entry expiry. Linear scans: at most 64 entries.
/// Store `Option<V>` to cache a negative answer (with `TTL_NEGATIVE_MS`).
pub struct Lru<K, V> {
    cap: usize,
    tick: u64,
    items: Vec<(K, V, u64, u64)>, // key, value, expires_ms, last use
}

impl<K: PartialEq, V> Lru<K, V> {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            tick: 0,
            items: Vec::with_capacity(cap),
        }
    }

    pub fn get(&mut self, key: &K, now_ms: u64) -> Option<&V> {
        let i = self.items.iter().position(|e| e.0 == *key)?;
        if self.items[i].2 <= now_ms {
            self.items.swap_remove(i);
            return None;
        }
        self.tick += 1;
        self.items[i].3 = self.tick;
        Some(&self.items[i].1)
    }

    pub fn insert(&mut self, key: K, value: V, ttl_ms: u64, now_ms: u64) {
        self.items.retain(|e| e.0 != key);
        if self.items.len() >= self.cap {
            // Expired entries go first, then the least recently used.
            let victim = (0..self.items.len())
                .min_by_key(|&i| (self.items[i].2 > now_ms, self.items[i].3))
                .unwrap();
            self.items.swap_remove(victim);
        }
        self.tick += 1;
        self.items
            .push((key, value, now_ms.saturating_add(ttl_ms), self.tick));
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// An aircraft to look up (text part).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Want {
    pub icao: u32,
    /// `None` when the callsign is missing, empty or contains `?` (unreliable decode).
    pub callsign: Option<HString<8>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Focus first, then up to 2 prefetch (nearest positioned).
    pub text: heapless::Vec<Want, 3>,
    /// The focus ICAO, once it has been focus for [`PHOTO_DWELL_MS`].
    pub photo: Option<u32>,
}

pub const PHOTO_DWELL_MS: u64 = 3000;
const PREFETCH: usize = 2;

/// Lookup policy: which aircraft to ask about now. Keeps the focus dwell time.
#[derive(Default)]
pub struct Policy {
    focus: Option<(u32, u64)>, // icao, since_ms
}

impl Policy {
    pub fn plan(&mut self, snap: &Snapshot, now_ms: u64) -> Plan {
        let mut plan = Plan::default();
        let focus = snap.focus.and_then(|i| snap.list.get(i).map(|a| (i, a)));
        let Some((fi, f)) = focus else {
            self.focus = None;
            return plan;
        };
        let since = match self.focus {
            Some((icao, since)) if icao == f.icao => since,
            _ => now_ms,
        };
        self.focus = Some((f.icao, since));
        if now_ms.saturating_sub(since) >= PHOTO_DWELL_MS {
            plan.photo = Some(f.icao);
        }
        let want = |a: &crate::tracker::AircraftView| Want {
            icao: a.icao,
            callsign: a
                .callsign
                .as_ref()
                .filter(|c| !c.trim().is_empty() && !c.contains('?'))
                .map(|c| HString::try_from(c.trim()).unwrap()),
        };
        let _ = plan.text.push(want(f));
        // `list` has the positioned aircraft first, nearest first.
        for (i, a) in snap.list.iter().enumerate() {
            if plan.text.is_full() || a.position.is_none() {
                break;
            }
            if i != fi {
                let _ = plan.text.push(want(a));
            }
        }
        debug_assert!(plan.text.len() <= 1 + PREFETCH);
        plan
    }
}

/// At most one request per second, exponential back-off on errors, long pause on HTTP 429.
#[derive(Default)]
pub struct RateLimiter {
    next_ms: u64,
    errors: u32,
}

impl RateLimiter {
    pub const INTERVAL_MS: u64 = 1000;
    pub const BACKOFF_MAX_MS: u64 = 60_000;
    pub const PAUSE_429_MS: u64 = 5 * 60_000;

    pub fn ready(&self, now_ms: u64) -> bool {
        now_ms >= self.next_ms
    }

    /// Call when a request starts.
    pub fn started(&mut self, now_ms: u64) {
        self.next_ms = now_ms + Self::INTERVAL_MS;
    }

    pub fn success(&mut self) {
        self.errors = 0;
    }

    pub fn failure(&mut self, now_ms: u64) {
        self.errors = (self.errors + 1).min(16);
        let back = (Self::INTERVAL_MS << self.errors).min(Self::BACKOFF_MAX_MS);
        self.next_ms = now_ms + back;
    }

    pub fn too_many_requests(&mut self, now_ms: u64) {
        self.next_ms = now_ms + Self::PAUSE_429_MS;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::{AircraftView, Card, Counters, Marker, PositionView};

    // Hand-written, fictitious data in the verified shapes.
    const COMBINED: &str = r#"{"response":{"aircraft":{"type":"Testliner 100","icao_type":"TL10","manufacturer":"Example Aero","mode_s":"ABCDEF","registration":"ZQ-TST","registered_owner_country_iso_name":"ZZ","registered_owner":"Example Owner","url_photo":null,"url_photo_thumbnail":null},"flightroute":{"callsign":"ZQJ123","callsign_icao":"ZQJ123","callsign_iata":"ZJ123","airline":{"name":"Zeta Quebec Airways","icao":"ZQJ","iata":"ZJ","country":"Nowhere","country_iso":"ZZ","callsign":"ZEQ"},"origin":{"iata_code":"AAA","icao_code":"ZZAA","municipality":"Alpha City","name":"Alpha Intl","latitude":1.0,"longitude":2.0},"destination":{"iata_code":"BBB","municipality":"Zürich-Beta"}}}}"#;
    const AIRCRAFT_ONLY: &str =
        r#"{"response":{"aircraft":{"type":"Testliner 100","registration":"ZQ-TST"}}}"#;
    const CALLSIGN_ONLY: &str = r#"{"response":{"flightroute":{"airline":{"name":"Zeta Quebec Airways"},"origin":{"iata_code":"AAA"},"destination":null}}}"#;
    const PHOTOS: &str = r#"{"photos":[{"id":"1","thumbnail":{"src":"https://t.plnspttrs.net/00/1_t.jpg","size":{"width":200,"height":133}},"thumbnail_large":{"src":"https://t.plnspttrs.net/00/1_280.jpg","size":{"width":422,"height":280}},"link":"https://www.planespotters.net/photo/1/x?utm_source=api","photographer":"Jane Müller"}]}"#;

    #[test]
    fn adsbdb_shapes() {
        let (a, r) = parse_adsbdb(COMBINED.as_bytes()).unwrap();
        let (a, r) = (a.unwrap(), r.unwrap());
        assert_eq!(a.model.as_deref(), Some("Testliner 100"));
        assert_eq!(a.registration.as_deref(), Some("ZQ-TST"));
        assert_eq!(r.airline_name(), Some("Zeta Quebec Airways"));
        assert_eq!(
            r.destination.as_ref().unwrap().municipality.as_deref(),
            Some("Zürich-Beta")
        );
        let m = FocusMeta::from_parts(Some(&a), Some(&r));
        assert_eq!(m.airline.as_deref(), Some("Zeta Quebec Airways"));
        assert_eq!(m.origin.unwrap().iata.as_deref(), Some("AAA"));

        let (a, r) = parse_adsbdb(AIRCRAFT_ONLY.as_bytes()).unwrap();
        assert!(r.is_none());
        let m = FocusMeta::from_parts(a.as_ref(), None);
        assert_eq!(
            (m.manufacturer, m.registration.as_deref()),
            (None, Some("ZQ-TST"))
        );

        let (a, r) = parse_adsbdb(CALLSIGN_ONLY.as_bytes()).unwrap();
        assert!(a.is_none());
        let r = r.unwrap();
        assert!(r.destination.is_none() && r.origin.is_some());

        // The owner stands in for the airline when there is no route.
        let (a, _) = parse_adsbdb(COMBINED.as_bytes()).unwrap();
        assert_eq!(
            FocusMeta::from_parts(a.as_ref(), None).airline.as_deref(),
            Some("Example Owner")
        );
    }

    #[test]
    fn adsbdb_failures() {
        for t in ["unknown aircraft", "unknown callsign", "anything else"] {
            let body = alloc::format!(r#"{{"response":"{t}"}}"#);
            assert_eq!(parse_adsbdb(body.as_bytes()), Err(MetaError::NotFound));
        }
        assert_eq!(
            parse_adsbdb(br#"{"response":{}}"#),
            Err(MetaError::NotFound)
        );
        for bad in [
            "",
            "{",
            "[]",
            r#"{"response":5}"#,
            r#"{"nope":1}"#,
            "<html>",
        ] {
            assert_eq!(
                parse_adsbdb(bad.as_bytes()),
                Err(MetaError::Malformed),
                "{bad}"
            );
        }
        // A wrongly shaped part is dropped, the rest survives.
        let (a, r) =
            parse_adsbdb(br#"{"response":{"aircraft":{"type":"X"},"flightroute":"oops"}}"#)
                .unwrap();
        assert!(a.is_some() && r.is_none());
        let big = alloc::format!(r#"{{"response":"{}"}}"#, "x".repeat(MAX_BODY));
        assert_eq!(parse_adsbdb(big.as_bytes()), Err(MetaError::TooLarge));
    }

    #[test]
    fn planespotters_shapes() {
        let p = parse_planespotters(PHOTOS.as_bytes()).unwrap().unwrap();
        assert_eq!(p.src, "https://t.plnspttrs.net/00/1_280.jpg");
        assert_eq!(p.photographer, "Jane Müller");
        // Falls back to the small thumbnail.
        let small = r#"{"photos":[{"thumbnail":{"src":"https://t.plnspttrs.net/a_t.jpg"},"photographer":"A"}]}"#;
        assert!(
            parse_planespotters(small.as_bytes())
                .unwrap()
                .unwrap()
                .src
                .ends_with("a_t.jpg")
        );
        assert_eq!(parse_planespotters(br#"{"photos":[]}"#), Ok(None));
        // Other hosts and plain http are refused.
        for src in ["https://evil.example/x.jpg", "http://t.plnspttrs.net/x.jpg"] {
            let b = alloc::format!(
                r#"{{"photos":[{{"thumbnail_large":{{"src":"{src}"}},"photographer":"A"}}]}}"#
            );
            assert_eq!(parse_planespotters(b.as_bytes()), Ok(None));
        }
        assert_eq!(
            parse_planespotters(br#"{"error":"User-Agent"}"#),
            Err(MetaError::Malformed)
        );
        assert_eq!(parse_planespotters(b"{"), Err(MetaError::Malformed));
        let big = alloc::format!(r#"{{"photos":[],"x":"{}"}}"#, "x".repeat(MAX_BODY));
        assert_eq!(
            parse_planespotters(big.as_bytes()),
            Err(MetaError::TooLarge)
        );
    }

    #[test]
    fn lru_hit_miss_expiry_eviction() {
        let mut c: Lru<u32, &str> = Lru::new(2);
        assert!(c.get(&1, 0).is_none());
        c.insert(1, "a", 100, 0);
        c.insert(2, "b", 1000, 0);
        assert_eq!(c.get(&1, 50), Some(&"a")); // touches 1: 2 is now the oldest use
        c.insert(3, "c", 1000, 60);
        assert!(c.get(&2, 60).is_none(), "LRU entry evicted");
        assert_eq!(c.get(&1, 99), Some(&"a"));
        assert!(c.get(&1, 100).is_none(), "expired");
        assert_eq!(c.len(), 1);
        // An expired entry is evicted before a live, less recently used one.
        c.insert(4, "d", 10, 100);
        assert_eq!(c.get(&3, 100), Some(&"c"));
        c.insert(5, "e", 1000, 200); // 4 expired at 110
        assert!(c.get(&3, 200).is_some() && c.get(&5, 200).is_some());
        // Re-insert replaces and renews.
        c.insert(5, "e2", 1000, 300);
        assert_eq!((c.len(), c.get(&5, 1299)), (2, Some(&"e2")));
        // Negative caching stores None.
        let mut n: Lru<u32, Option<u8>> = Lru::new(4);
        n.insert(7, None, TTL_NEGATIVE_MS, 0);
        assert_eq!(n.get(&7, 1), Some(&None));
        assert!(n.get(&7, TTL_NEGATIVE_MS).is_none());
    }

    fn ac(n: u32, ground_m: f64, cs: Option<&str>, positioned: bool) -> AircraftView {
        AircraftView {
            icao: 0xA00000 + n,
            callsign: cs.map(|c| HString::try_from(c).unwrap()),
            category: None,
            squawk: None,
            altitude_ft: None,
            vertical_rate_fpm: None,
            ground_speed_kt: None,
            track_deg: None,
            last_seen_age_ms: 0,
            position: positioned.then_some(PositionView {
                ground_m,
                bearing_deg: 0.0,
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

    fn snap(list: impl IntoIterator<Item = AircraftView>, focus: Option<usize>) -> Snapshot {
        let mut s = Snapshot {
            card: Card::Quiet,
            list: Default::default(),
            in_radius: 0,
            primary: None,
            focus,
            just_passed: None,
            approaching: Default::default(),
            counters: Counters::default(),
        };
        for a in list {
            s.list.push(a).unwrap();
        }
        s
    }

    #[test]
    fn policy_focus_prefetch_dwell_and_callsign_skip() {
        // 50 aircraft, nearest first; the last 10 have no position.
        let list: Vec<_> = (0..50)
            .map(|n| ac(n, 1000.0 + n as f64 * 500.0, Some("ZQ1"), n < 40))
            .collect();
        let mut s = snap(list, Some(0));
        let mut p = Policy::default();
        let plan = p.plan(&s, 10_000);
        let icaos: Vec<u32> = plan.text.iter().map(|w| w.icao - 0xA00000).collect();
        assert_eq!(icaos, [0, 1, 2]);
        assert_eq!(plan.photo, None, "no photo before the dwell time");
        assert_eq!(p.plan(&s, 12_999).photo, None);
        assert_eq!(p.plan(&s, 13_000).photo, Some(0xA00000));

        // Focus on the 5th aircraft: prefetch the two nearest others, not a neighbour of the focus.
        s.focus = Some(4);
        let plan = p.plan(&s, 14_000);
        let icaos: Vec<u32> = plan.text.iter().map(|w| w.icao - 0xA00000).collect();
        assert_eq!(icaos, [4, 0, 1]);
        assert_eq!(plan.photo, None, "new focus restarts the dwell");
        assert_eq!(p.plan(&s, 17_000).photo, Some(0xA00004));

        // No focus: nothing, and the dwell is forgotten.
        s.focus = None;
        assert_eq!(p.plan(&s, 18_000), Plan::default());
        s.focus = Some(4);
        assert_eq!(p.plan(&s, 18_001).photo, None);

        // Unpositioned aircraft are never prefetched.
        let s = snap(
            [
                ac(0, 1.0, Some("ZQ1"), true),
                ac(1, 0.0, Some("ZQ2"), false),
            ],
            Some(0),
        );
        assert_eq!(Policy::default().plan(&s, 0).text.len(), 1);

        // Callsigns: missing, empty and with '?' are skipped, others trimmed.
        let s = snap(
            [
                ac(0, 1.0, Some("ZQ9 "), true),
                ac(1, 2.0, Some("ZQ?1"), true),
                ac(2, 3.0, Some("  "), true),
            ],
            Some(0),
        );
        let t = Policy::default().plan(&s, 0).text;
        let cs: Vec<Option<&str>> = t.iter().map(|w| w.callsign.as_deref()).collect();
        assert_eq!(cs, [Some("ZQ9"), None, None]);
    }

    #[test]
    fn rate_limiter() {
        let mut r = RateLimiter::default();
        assert!(r.ready(0));
        r.started(0);
        assert!(!r.ready(999) && r.ready(1000));
        r.success();
        r.failure(1000); // 2 s
        assert!(!r.ready(2999) && r.ready(3000));
        r.failure(3000); // 4 s
        assert!(!r.ready(6999) && r.ready(7000));
        for _ in 0..30 {
            r.failure(0);
        }
        assert!(!r.ready(59_999) && r.ready(60_000), "capped at 60 s");
        r.success();
        r.started(100_000);
        r.failure(100_000);
        assert!(r.ready(102_000), "success reset the back-off");
        r.too_many_requests(200_000);
        assert!(!r.ready(499_999) && r.ready(500_000));
    }
}
