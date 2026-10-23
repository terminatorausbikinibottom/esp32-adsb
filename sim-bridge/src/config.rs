//! Bridge configuration: `.env` file (shell `KEY=value` lines) overlaid by the process environment.
//! Nothing here is ever logged; the values are the receiver location.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

pub const DEFAULT_ADDR: &str = "127.0.0.1:8081";
const DEFAULT_ORIGINS: &str = "http://localhost:8080,http://127.0.0.1:8080";

#[derive(Debug)]
pub struct Config {
    pub listen: SocketAddr,
    /// `host:port` of the fixed upstream feed; `None` disables `/ws/live`.
    pub feed: Option<String>,
    /// Pre-rendered `/local/config.json`; `None` (lat/lon unset) makes it 404.
    pub config_json: Option<String>,
    pub capture: Option<PathBuf>,
    pub origins: HashSet<String>,
    pub hosts: HashSet<String>,
    /// One permit per live WebSocket client (`ADSB_SIM_MAX_CLIENTS`).
    pub slots: Arc<Semaphore>,
    /// How long the upstream feed may stay down before the client WebSocket is closed
    /// (`ADSB_SIM_UPSTREAM_GRACE_S`).
    pub upstream_grace: Duration,
}

/// `KEY=value` lines as `set -a; . ./.env` would see them: optional `export`, optional quotes.
fn parse_env_file(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            let l = l.strip_prefix("export ").unwrap_or(l).trim_start();
            let (k, v) = l.split_once('=')?;
            if k.starts_with('#') {
                return None;
            }
            let v = v.trim();
            let v = [('"', '"'), ('\'', '\'')]
                .iter()
                .find_map(|&(a, b)| v.strip_prefix(a)?.strip_suffix(b))
                .unwrap_or(v);
            Some((k.trim().to_owned(), v.to_owned()))
        })
        .collect()
}

fn number(v: Option<&str>, name: &str, lo: f64, hi: f64) -> Result<Option<f64>, String> {
    let Some(v) = v else { return Ok(None) };
    match v.parse::<f64>() {
        Ok(x) if (lo..=hi).contains(&x) => Ok(Some(x)),
        _ => Err(format!("{name} is malformed or out of range")), // never echo the value
    }
}

impl Config {
    /// `file`: contents of the local `.env` (if any). `env`: process environment, wins over `file`.
    pub fn from_sources(
        file: Option<&str>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let file = file.map(parse_env_file).unwrap_or_default();
        let get = |k: &str| {
            env(k)
                .or_else(|| {
                    file.iter()
                        .rev()
                        .find(|(n, _)| n == k)
                        .map(|(_, v)| v.clone())
                })
                .filter(|v| !v.is_empty())
        };

        let listen: SocketAddr = get("ADSB_SIM_BRIDGE_ADDR")
            .as_deref()
            .unwrap_or(DEFAULT_ADDR)
            .parse()
            .map_err(|_| "ADSB_SIM_BRIDGE_ADDR is not a socket address".to_owned())?;
        if !listen.ip().is_loopback() {
            return Err(format!(
                "refusing to listen on non-loopback address {}",
                listen.ip()
            ));
        }

        let port = match get("ADSB_FEED_PORT") {
            Some(p) => p
                .parse::<u16>()
                .map_err(|_| "ADSB_FEED_PORT is malformed")?,
            None => 30002,
        };
        let feed = get("ADSB_FEED_HOST").map(|h| format!("{h}:{port}"));

        let lat = number(get("ADSB_RX_LAT").as_deref(), "ADSB_RX_LAT", -90.0, 90.0)?;
        let lon = number(get("ADSB_RX_LON").as_deref(), "ADSB_RX_LON", -180.0, 180.0)?;
        let elev = number(
            get("ADSB_RX_ELEV_M").as_deref(),
            "ADSB_RX_ELEV_M",
            -500.0,
            10_000.0,
        )?;
        let radius = number(
            get("ADSB_RADIUS_KM").as_deref(),
            "ADSB_RADIUS_KM",
            0.5,
            50.0,
        )?;
        // Parsed numbers only, so the hand-built JSON cannot be malformed or injected.
        let config_json = lat.zip(lon).map(|(lat, lon)| {
            format!(
                r#"{{"lat":{lat},"lon":{lon},"elev_m":{},"radius_km":{}}}"#,
                elev.unwrap_or(0.0),
                radius.unwrap_or(5.0)
            )
        });

        let origins: HashSet<String> = get("ADSB_SIM_ALLOWED_ORIGINS")
            .unwrap_or_else(|| DEFAULT_ORIGINS.to_owned())
            .split(',')
            .map(|o| o.trim().to_owned())
            .filter(|o| !o.is_empty())
            .collect();
        // Host values: the authorities of the allowed origins (trunk proxy, which forwards the
        // browser's Host) plus the bridge's own port (direct access).
        let mut hosts: HashSet<String> = origins
            .iter()
            .filter_map(|o| Some(o.split_once("://")?.1.to_ascii_lowercase()))
            .collect();
        for name in ["localhost", "127.0.0.1"] {
            hosts.insert(format!("{name}:{}", listen.port()));
        }

        let max_clients = match get("ADSB_SIM_MAX_CLIENTS") {
            Some(n) => n
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=1024).contains(n))
                .ok_or("ADSB_SIM_MAX_CLIENTS must be 1..=1024")?,
            None => 4,
        };

        let grace_s = match get("ADSB_SIM_UPSTREAM_GRACE_S") {
            Some(n) => n
                .parse::<u64>()
                .ok()
                .filter(|n| (1..=3600).contains(n))
                .ok_or("ADSB_SIM_UPSTREAM_GRACE_S must be 1..=3600")?,
            None => 10,
        };

        Ok(Config {
            upstream_grace: Duration::from_secs(grace_s),
            slots: Arc::new(Semaphore::new(max_clients)),
            listen,
            feed,
            config_json,
            capture: get("ADSB_REPLAY_CAPTURE").map(PathBuf::from),
            origins,
            hosts,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_clients_default_and_validation() {
        assert_eq!(cfg(None, &[]).unwrap().slots.available_permits(), 4);
        let c = cfg(Some("ADSB_SIM_MAX_CLIENTS=2"), &[]).unwrap();
        assert_eq!(c.slots.available_permits(), 2);
        for bad in ["0", "x", "-1", "99999"] {
            assert!(
                cfg(None, &[("ADSB_SIM_MAX_CLIENTS", bad)]).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn upstream_grace_default_and_validation() {
        assert_eq!(cfg(None, &[]).unwrap().upstream_grace.as_secs(), 10);
        let c = cfg(Some("ADSB_SIM_UPSTREAM_GRACE_S=3"), &[]).unwrap();
        assert_eq!(c.upstream_grace.as_secs(), 3);
        for bad in ["0", "x", "-1", "99999"] {
            assert!(
                cfg(None, &[("ADSB_SIM_UPSTREAM_GRACE_S", bad)]).is_err(),
                "{bad}"
            );
        }
    }

    fn cfg(file: Option<&str>, env: &[(&str, &str)]) -> Result<Config, String> {
        Config::from_sources(file, |k| {
            env.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        })
    }

    #[test]
    fn defaults() {
        let c = cfg(None, &[]).unwrap();
        assert_eq!(c.listen.to_string(), "127.0.0.1:8081");
        assert!(c.feed.is_none() && c.config_json.is_none() && c.capture.is_none());
        for h in [
            "localhost:8080",
            "127.0.0.1:8080",
            "localhost:8081",
            "127.0.0.1:8081",
        ] {
            assert!(c.hosts.contains(h), "{h}");
        }
        assert_eq!(c.hosts.len(), 4);
        assert!(c.origins.contains("http://localhost:8080"));
    }

    #[test]
    fn loopback_only() {
        for ok in ["127.0.0.1:9", "[::1]:9", "127.5.5.5:9"] {
            assert!(cfg(None, &[("ADSB_SIM_BRIDGE_ADDR", ok)]).is_ok(), "{ok}");
        }
        for bad in [
            "0.0.0.0:8081",
            "192.0.2.1:8081",
            "[::]:8081",
            "localhost:8081",
            "8081",
        ] {
            assert!(
                cfg(None, &[("ADSB_SIM_BRIDGE_ADDR", bad)]).is_err(),
                "{bad}"
            );
        }
        // Also when it comes from the .env file.
        assert!(cfg(Some("ADSB_SIM_BRIDGE_ADDR=0.0.0.0:8081"), &[]).is_err());
    }

    #[test]
    fn env_file_parsing_and_precedence() {
        let file = "# c\n\nexport ADSB_FEED_HOST=\"192.0.2.1\"\nADSB_FEED_PORT='30005'\n\
                    ADSB_RX_LAT=1.5\nADSB_RX_LON=-2.5\nADSB_RX_ELEV_M=12\nADSB_RADIUS_KM=7\n";
        let c = cfg(Some(file), &[]).unwrap();
        assert_eq!(c.feed.as_deref(), Some("192.0.2.1:30005"));
        assert_eq!(
            c.config_json.as_deref(),
            Some(r#"{"lat":1.5,"lon":-2.5,"elev_m":12,"radius_km":7}"#)
        );
        let c = cfg(Some(file), &[("ADSB_FEED_HOST", "192.0.2.9")]).unwrap();
        assert_eq!(c.feed.as_deref(), Some("192.0.2.9:30005"));
    }

    #[test]
    fn config_defaults_and_validation() {
        let c = cfg(Some("ADSB_RX_LAT=1\nADSB_RX_LON=2"), &[]).unwrap();
        assert_eq!(
            c.config_json.unwrap(),
            r#"{"lat":1,"lon":2,"elev_m":0,"radius_km":5}"#
        );
        // lat without lon: no config served.
        assert!(
            cfg(Some("ADSB_RX_LAT=1"), &[])
                .unwrap()
                .config_json
                .is_none()
        );
        for bad in [
            "ADSB_RX_LAT=91",
            "ADSB_RX_LAT=abc",
            "ADSB_RADIUS_KM=0.1",
            "ADSB_FEED_PORT=x",
        ] {
            assert!(cfg(Some(bad), &[]).is_err(), "{bad}");
        }
    }
}
