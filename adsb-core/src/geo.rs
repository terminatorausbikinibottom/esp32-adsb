//! Spherical-earth geo maths (f64 + libm). Distances in metres, angles in degrees.

/// Mean earth radius; the spherical model is good to ~0.6 %, plenty for a 5 km vicinity.
pub const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// Great-circle (ground) distance via haversine.
pub fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let (sp, sl) = (libm::sin(dp / 2.0), libm::sin(dl / 2.0));
    let a = sp * sp + libm::cos(p1) * libm::cos(p2) * sl * sl;
    // clamp: rounding can push `a` marginally above 1 for antipodal points
    2.0 * EARTH_RADIUS_M * libm::asin(libm::sqrt(a.min(1.0)))
}

/// Initial bearing from point 1 to point 2, in [0, 360), clockwise from true north.
pub fn bearing_deg(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dl = (lon2 - lon1).to_radians();
    let y = libm::sin(dl) * libm::cos(p2);
    let x = libm::cos(p1) * libm::sin(p2) - libm::sin(p1) * libm::cos(p2) * libm::cos(dl);
    let b = libm::atan2(y, x).to_degrees();
    // `% 360` of a tiny negative value can round to exactly 360.0; fold that back to 0
    let b = if b < 0.0 { b + 360.0 } else { b };
    if b >= 360.0 { 0.0 } else { b }
}

/// 8-point compass label ("N", "NE", ...) for a bearing in degrees (any real value).
pub fn compass_label(bearing_deg: f64) -> &'static str {
    const L: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    let sector = libm::floor((bearing_deg + 22.5) / 45.0) as i64;
    L[sector.rem_euclid(8) as usize]
}

/// Elevation angle above the horizon: `atan2(height, ground distance)`; 90 straight up.
pub fn elevation_deg(height_m: f64, ground_m: f64) -> f64 {
    libm::atan2(height_m, ground_m).to_degrees()
}

/// Straight-line distance from height above receiver and ground distance.
pub fn slant_m(height_m: f64, ground_m: f64) -> f64 {
    libm::hypot(height_m, ground_m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) {
        assert!((a - b).abs() <= tol, "{a} vs {b}");
    }

    #[test]
    fn ams_lhr() {
        // AMS 52.3086 N 4.7639 E, LHR 51.4700 N 0.4543 W; ~370 km
        let d = distance_m(52.3086, 4.7639, 51.4700, -0.4543);
        close(d, 370_000.0, 370_000.0 * 0.005);
        let b = bearing_deg(52.3086, 4.7639, 51.4700, -0.4543);
        assert_eq!(compass_label(b), "W");
    }

    #[test]
    fn jfk_lhr() {
        // JFK 40.6413 N 73.7781 W -> LHR; ~5540 km
        let d = distance_m(40.6413, -73.7781, 51.4700, -0.4543);
        close(d, 5_540_000.0, 5_540_000.0 * 0.005);
    }

    #[test]
    fn cardinal_bearings() {
        close(bearing_deg(10.0, 20.0, 11.0, 20.0), 0.0, 1e-9);
        close(bearing_deg(0.0, 20.0, 0.0, 21.0), 90.0, 1e-9);
        close(bearing_deg(10.0, 20.0, 9.0, 20.0), 180.0, 1e-9);
        close(bearing_deg(0.0, 20.0, 0.0, 19.0), 270.0, 1e-9);
    }

    #[test]
    fn identical_points() {
        assert_eq!(distance_m(50.0, 8.0, 50.0, 8.0), 0.0);
        assert_eq!(bearing_deg(50.0, 8.0, 50.0, 8.0), 0.0);
    }

    #[test]
    fn antimeridian() {
        // 1 degree of longitude at the equator, across 180: ~111.2 km, heading east
        let d = distance_m(0.0, 179.5, 0.0, -179.5);
        close(d, 111_195.0, 200.0);
        close(bearing_deg(0.0, 179.5, 0.0, -179.5), 90.0, 1e-9);
        close(bearing_deg(0.0, -179.5, 0.0, 179.5), 270.0, 1e-9);
    }

    #[test]
    fn poles() {
        let quarter = core::f64::consts::PI * EARTH_RADIUS_M / 2.0;
        close(distance_m(0.0, 33.0, 90.0, 0.0), quarter, 1.0);
        // north bearing may land at 359.999...; compare on the circle
        let n = bearing_deg(89.0, 10.0, 90.0, 0.0);
        close(n.min(360.0 - n), 0.0, 1e-6);
        close(bearing_deg(-89.0, 10.0, -90.0, 0.0), 180.0, 1e-6);
        // antipodal: half circumference, no NaN
        let half = distance_m(0.0, 0.0, 0.0, 180.0);
        close(half, 2.0 * quarter, 1.0);
    }

    #[test]
    fn bearing_range() {
        let b = bearing_deg(0.0, 0.0, -1e-12, -1e-9);
        assert!((0.0..360.0).contains(&b));
    }

    #[test]
    fn compass() {
        let cases = [
            (0.0, "N"),
            (22.4, "N"),
            (22.5, "NE"),
            (45.0, "NE"),
            (90.0, "E"),
            (135.0, "SE"),
            (180.0, "S"),
            (225.0, "SW"),
            (270.0, "W"),
            (315.0, "NW"),
            (337.4, "NW"),
            (337.5, "N"),
            (359.9, "N"),
            (-45.0, "NW"),
            (360.0, "N"),
        ];
        for (b, l) in cases {
            assert_eq!(compass_label(b), l, "{b}");
        }
    }

    #[test]
    fn elevation() {
        close(elevation_deg(3000.0, 3000.0), 45.0, 1e-9);
        assert_eq!(elevation_deg(11_000.0, 0.0), 90.0);
        close(elevation_deg(0.0, 5000.0), 0.0, 1e-12);
        // 11 km up at 5 km ground distance
        close(elevation_deg(11_000.0, 5000.0), 65.55, 0.01);
        // below the receiver horizon
        assert!(elevation_deg(-100.0, 1000.0) < 0.0);
    }

    #[test]
    fn slant() {
        close(slant_m(3000.0, 4000.0), 5000.0, 1e-9);
        assert_eq!(slant_m(11_000.0, 0.0), 11_000.0);
    }
}
