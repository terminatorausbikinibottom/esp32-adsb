//! Hand-written metadata for the scenario callsigns (all fictitious) and a synthetic photo, so
//! the sim shows a filled info panel without network access. The JSON follows the adsbdb shapes
//! checked in RD.7 and goes through the same parser as the device uses.
use adsb_core::meta::{FocusMeta, Photo, parse_adsbdb};
use std::rc::Rc;

pub const JET: &str = r#"{"response":{"aircraft":{"type":"Testliner 300","icao_type":"TL30","manufacturer":"Example Aerospace","registration":"ZQ-JET","registered_owner":"Zeta Quebec Airways"},"flightroute":{"airline":{"name":"Zeta Quebec Airways"},"origin":{"iata_code":"AAA","municipality":"Alphaville"},"destination":{"iata_code":"BBB","municipality":"Bravo Bay"}}}}"#;
/// Aircraft only (no scheduled route): no photo in the sim keyframes either.
pub const PRIVATE: &str = r#"{"response":{"aircraft":{"type":"Sparrow 2","icao_type":"SPW2","manufacturer":"Example Light Aircraft","registration":"ZQ-BRD","registered_owner":"Example Flying Club"}}}"#;
pub const LONG_NAME: &str = r#"{"response":{"aircraft":{"type":"Intercontinental Heavy Freighter Extended Range","icao_type":"IHFX","manufacturer":"International Example Aerospace Corporation","registration":"ZQ-LONGREG"},"flightroute":{"airline":{"name":"The Very Long Fictitious Transcontinental Cargo and Passenger Airways Company"},"origin":{"iata_code":"CCC","municipality":"Port Ludicrous-on-the-Extremely-Long-River"},"destination":{"iata_code":"DDD","municipality":"Delta"}}}}"#;
pub const DIACRITICS: &str = r#"{"response":{"aircraft":{"type":"Testliner 300","icao_type":"TL30","manufacturer":"Example Aerospace","registration":"ZQ-DIA"},"flightroute":{"airline":{"name":"Île de Zêta Línea Aérea"},"origin":{"iata_code":"LCJ","municipality":"Łódź"},"destination":{"iata_code":"ZRH","municipality":"Zürich · São Paulo · Kraków"}}}}"#;

pub fn from_json(adsbdb: &str, photo: Option<(&str, Rc<Photo>)>) -> FocusMeta {
    let (a, r) = parse_adsbdb(adsbdb.as_bytes()).expect("fixture parses");
    let mut m = FocusMeta::from_parts(a.as_ref(), r.as_ref());
    if let Some((by, p)) = photo {
        m.photographer = Some(by.into());
        m.photo = Some(p);
    }
    m
}

/// Metadata for the scenario callsigns; `None` (shown as dashes) for everything else.
pub fn for_callsign(cs: &str) -> Option<FocusMeta> {
    Some(match cs {
        c if c.starts_with("ZQJ") => from_json(JET, Some(("Jane Example", synthetic_photo()))),
        c if c.starts_with("ZQB") => from_json(PRIVATE, None),
        "ZQLONG99" => from_json(
            LONG_NAME,
            Some(("A. Photographer-With-A-Rather-Long-Name", synthetic_photo())),
        ),
        _ => return None,
    })
}

/// Sky gradient, a white plane silhouette and green ground: obviously not a real photograph.
pub fn synthetic_photo() -> Rc<Photo> {
    let (w, h) = (336usize, 224usize);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as i32 - 168, y as i32 - 100);
            let body = dy.abs() <= 6 && dx.abs() <= 80;
            let wing =
                dx.abs() + 2 * dy.abs() <= 60 && dy.abs() <= 40 && dx.abs() <= 30 + dy.abs() / 2;
            let px = if body || wing {
                [235, 238, 242]
            } else if y > 180 {
                [60, 120 + (y - 180) as u8, 70]
            } else {
                [40 + (y / 3) as u8, 100 + (y / 2) as u8, 200]
            };
            rgb.extend(px);
        }
    }
    Rc::new(Photo {
        width: w as u16,
        height: h as u16,
        rgb,
    })
}
