//! Parse and validate the build-time configuration (docs/design/radar-ui.md, "Configuration").
//! Included by `build.rs` with `#[path]`; the tests run on the host with `rustc --test`.
//! Error messages name the variable and the rule, never the value.

/// Variables the build reads, for `cargo:rerun-if-env-changed`.
pub const VARS: &[&str] = &[
    "WIFI_SSID",
    "WIFI_KEY",
    "ADSB_WIFI_SSID",
    "ADSB_WIFI_PASSWORD",
    "ADSB_FEED_HOST",
    "ADSB_FEED_PORT",
    "ADSB_RX_LAT",
    "ADSB_RX_LON",
    "ADSB_RX_ELEV_M",
    "ADSB_RADIUS_KM",
    "ADSB_DEMO",
    "ADSB_META_PROXY",
    "ADSB_HTTP_CONTACT",
];

/// Returns the source of `config.rs`. `get` reads one variable (unset or not unicode: `None`).
pub fn generate(get: impl Fn(&str) -> Option<String>) -> Result<String, String> {
    let required = |name: &str, rule: &str| {
        get(name).ok_or_else(|| {
            let hint = if name == "WIFI_SSID" && get("ADSB_WIFI_SSID").is_some() {
                " (ADSB_WIFI_SSID is the old name, rename it to WIFI_SSID)"
            } else if name == "WIFI_KEY" && get("ADSB_WIFI_PASSWORD").is_some() {
                " (ADSB_WIFI_PASSWORD is the old name, rename it to WIFI_KEY)"
            } else {
                ""
            };
            format!("{name} is required: {rule}{hint}")
        })
    };
    let number = |name: &str, default: &str, lo: f64, hi: f64| -> Result<f64, String> {
        let v: f64 = get(name)
            .unwrap_or_else(|| default.to_string())
            .parse()
            .map_err(|_| format!("{name} must be a decimal number"))?;
        if (lo..=hi).contains(&v) {
            Ok(v)
        } else {
            Err(format!("{name} must be in {lo}..={hi}"))
        }
    };

    let ssid = required("WIFI_SSID", "1 to 32 bytes")?;
    if !(1..=32).contains(&ssid.len()) {
        return Err("WIFI_SSID must be 1 to 32 bytes".into());
    }
    let key = required("WIFI_KEY", "8 to 63 bytes (WPA2-PSK)")?;
    if !(8..=63).contains(&key.len()) {
        return Err("WIFI_KEY must be 8 to 63 bytes (WPA2-PSK)".into());
    }

    let demo = match get("ADSB_DEMO").as_deref() {
        None => "None",
        Some("overflight") => "Some(Demo::Overflight)",
        Some("busy") => "Some(Demo::Busy)",
        Some(_) => return Err("ADSB_DEMO must be `overflight` or `busy`".into()),
    };
    let host = if demo == "None" {
        Some(required("ADSB_FEED_HOST", "IPv4 literal or host name")?)
    } else {
        get("ADSB_FEED_HOST")
    };
    if let Some(h) = &host
        && (h.is_empty() || h.len() > 253 || h.chars().any(|c| c.is_whitespace() || c.is_control()))
    {
        return Err("ADSB_FEED_HOST must be 1 to 253 characters without white space".into());
    }

    let port = number("ADSB_FEED_PORT", "30002", 1.0, 65535.0)?;
    if port.fract() != 0.0 {
        return Err("ADSB_FEED_PORT must be an integer".into());
    }
    required("ADSB_RX_LAT", "decimal degrees")?;
    required("ADSB_RX_LON", "decimal degrees")?;
    let lat = number("ADSB_RX_LAT", "", -90.0, 90.0)?;
    let lon = number("ADSB_RX_LON", "", -180.0, 180.0)?;
    let elev = number("ADSB_RX_ELEV_M", "0", -500.0, 10_000.0)?;
    let radius = number("ADSB_RADIUS_KM", "5", 0.5, 50.0)?;

    let proxy = match get("ADSB_META_PROXY") {
        None => "None".to_string(),
        Some(p) if p.starts_with("http://") && !p.contains(char::is_whitespace) => {
            format!("Some({p:?})")
        }
        Some(_) => return Err("ADSB_META_PROXY must be `http://<host>:<port>`".into()),
    };
    // Goes into the User-Agent header: no white space or control characters (header injection).
    let contact = match get("ADSB_HTTP_CONTACT") {
        None => "None".to_string(),
        Some(c) if (1..=120).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_graphic()) => {
            format!("Some({c:?})")
        }
        Some(_) => {
            return Err("ADSB_HTTP_CONTACT must be 1 to 120 printable ASCII characters without white space".into());
        }
    };
    let host = host.map_or("None".to_string(), |h| format!("Some({h:?})"));

    Ok(format!(
        "pub const WIFI_SSID: &str = {ssid:?};\n\
         pub const WIFI_KEY: &str = {key:?};\n\
         pub const FEED_HOST: Option<&str> = {host};\n\
         pub const FEED_PORT: u16 = {port};\n\
         pub const RX_LAT: f64 = {lat:?};\n\
         pub const RX_LON: f64 = {lon:?};\n\
         pub const RX_ELEV_M: f64 = {elev:?};\n\
         pub const RADIUS_KM: f64 = {radius:?};\n\
         pub const DEMO: Option<Demo> = {demo};\n\
         pub const META_PROXY: Option<&str> = {proxy};\n\
         pub const HTTP_CONTACT: Option<&str> = {contact};\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARK: &str = "MARKER-VALUE-9f3";

    fn base() -> Vec<(&'static str, String)> {
        [
            ("WIFI_SSID", "example-ssid"),
            ("WIFI_KEY", "example-key-123"),
            ("ADSB_FEED_HOST", "192.0.2.10"),
            ("ADSB_RX_LAT", "47.0"),
            ("ADSB_RX_LON", "-30.0"),
        ]
        .map(|(k, v)| (k, v.to_string()))
        .to_vec()
    }

    fn run(vars: &[(&str, String)]) -> Result<String, String> {
        generate(|n| vars.iter().find(|(k, _)| *k == n).map(|(_, v)| v.clone()))
    }

    fn with(name: &'static str, value: &str) -> Vec<(&'static str, String)> {
        let mut v: Vec<_> = base().into_iter().filter(|(k, _)| *k != name).collect();
        v.push((name, value.to_string()));
        v
    }

    fn without(name: &str) -> Vec<(&'static str, String)> {
        base().into_iter().filter(|(k, _)| *k != name).collect()
    }

    #[test]
    fn valid_defaults() {
        let out = run(&base()).unwrap();
        assert!(out.contains("FEED_PORT: u16 = 30002;"));
        assert!(out.contains("RX_LON: f64 = -30.0;"));
        assert!(out.contains("DEMO: Option<Demo> = None;"));
    }

    #[test]
    fn missing_var_is_named() {
        for n in ["WIFI_SSID", "WIFI_KEY", "ADSB_FEED_HOST", "ADSB_RX_LAT", "ADSB_RX_LON"] {
            assert!(run(&without(n)).unwrap_err().starts_with(n), "{n}");
        }
        assert!(generate(|_| None).unwrap_err().starts_with("WIFI_SSID"));
    }

    #[test]
    fn bad_values_fail_without_echo() {
        let bad = [
            ("ADSB_RX_LAT", "91"),
            ("ADSB_RX_LAT", "nan"),
            ("ADSB_RX_LON", "-181"),
            ("ADSB_RX_LON", MARK),
            ("ADSB_RX_ELEV_M", "10001"),
            ("ADSB_RX_ELEV_M", "-501"),
            ("ADSB_RADIUS_KM", "0.4"),
            ("ADSB_RADIUS_KM", "51"),
            ("ADSB_FEED_PORT", "0"),
            ("ADSB_FEED_PORT", "70000"),
            ("ADSB_FEED_PORT", "30002.5"),
            ("ADSB_FEED_HOST", "a b"),
            ("ADSB_DEMO", MARK),
            ("ADSB_META_PROXY", MARK),
            ("ADSB_HTTP_CONTACT", "a b"),
            ("ADSB_HTTP_CONTACT", "a\r\nX-Evil: 1"),
            ("ADSB_HTTP_CONTACT", ""),
            ("WIFI_SSID", ""),
            ("WIFI_KEY", "short"),
        ];
        for (n, v) in bad {
            let e = run(&with(n, v)).unwrap_err();
            assert!(e.contains(n), "{n}={v}: {e}");
            assert!(!e.contains(v) || v.len() < 3, "{n} echoes value: {e}");
        }
        // a marker in every secret-like var must not appear in any error
        for n in ["WIFI_SSID", "WIFI_KEY"] {
            let long = MARK.repeat(5);
            let e = run(&with(n, &long)).unwrap_err();
            assert!(!e.contains(MARK));
        }
    }

    #[test]
    fn old_wifi_names_hint() {
        let mut v = without("WIFI_SSID");
        v.push(("ADSB_WIFI_SSID", MARK.into()));
        let e = run(&v).unwrap_err();
        assert!(e.starts_with("WIFI_SSID") && e.contains("ADSB_WIFI_SSID"));
        assert!(!e.contains(MARK));
    }

    #[test]
    fn demo_makes_host_optional() {
        let mut v = without("ADSB_FEED_HOST");
        v.push(("ADSB_DEMO", "overflight".into()));
        let out = run(&v).unwrap();
        assert!(out.contains("FEED_HOST: Option<&str> = None;"));
        assert!(out.contains("Some(Demo::Overflight)"));
    }

    #[test]
    fn proxy_and_strings_are_escaped() {
        let mut v = with("WIFI_SSID", "a\"b\\c");
        v.push(("ADSB_META_PROXY", "http://192.0.2.5:8080".into()));
        let out = run(&v).unwrap();
        assert!(out.contains(r#"WIFI_SSID: &str = "a\"b\\c";"#));
        assert!(out.contains(r#"Some("http://192.0.2.5:8080")"#));
    }

    #[test]
    fn http_contact_is_optional_and_escaped() {
        assert!(run(&base()).unwrap().contains("HTTP_CONTACT: Option<&str> = None;"));
        let out = run(&with("ADSB_HTTP_CONTACT", "mailto:me@example.invalid")).unwrap();
        assert!(out.contains(r#"HTTP_CONTACT: Option<&str> = Some("mailto:me@example.invalid");"#));
        let out = run(&with("ADSB_HTTP_CONTACT", "https://x.invalid/\"q")).unwrap();
        assert!(out.contains(r#"Some("https://x.invalid/\"q")"#));
        assert!(run(&with("ADSB_HTTP_CONTACT", &"a".repeat(121))).is_err());
    }
}
