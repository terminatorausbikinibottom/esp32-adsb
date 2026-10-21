//! Live-mode logic that doesn't touch the browser: the reconnect state machine and validation of
//! the receiver config the bridge serves. Kept native so it is unit-tested.
use adsb_core::tracker::Config;

pub const BACKOFF_MIN_MS: u64 = 1_000;
pub const BACKOFF_MAX_MS: u64 = 30_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Idle,
    Connecting,
    Open,
    Waiting { until_ms: u64 },
}

/// WebSocket lifecycle with exponential backoff. A failed handshake (bridge down, 503 from the
/// client cap) and a dropped connection are the same event to the browser: `on_closed`.
/// The delay resets only after data arrived, so a bridge that accepts and immediately closes
/// is still backed off.
#[derive(Debug)]
pub struct Reconnect {
    link: Link,
    delay_ms: u64,
}

impl Default for Reconnect {
    fn default() -> Self {
        Self {
            link: Link::Idle,
            delay_ms: BACKOFF_MIN_MS,
        }
    }
}

impl Reconnect {
    /// Begin connecting now; the caller opens the socket.
    pub fn start(&mut self) {
        *self = Self {
            link: Link::Connecting,
            ..Self::default()
        };
    }

    pub fn stop(&mut self) {
        self.link = Link::Idle;
    }

    pub fn on_open(&mut self) {
        if self.link == Link::Connecting {
            self.link = Link::Open;
        }
    }

    pub fn on_data(&mut self) {
        self.delay_ms = BACKOFF_MIN_MS;
    }

    pub fn on_closed(&mut self, now_ms: u64) {
        if matches!(self.link, Link::Connecting | Link::Open) {
            self.link = Link::Waiting {
                until_ms: now_ms + self.delay_ms,
            };
            self.delay_ms = (self.delay_ms * 2).min(BACKOFF_MAX_MS);
        }
    }

    /// True when the backoff elapsed and the caller must open a new socket.
    pub fn poll(&mut self, now_ms: u64) -> bool {
        match self.link {
            Link::Waiting { until_ms } if now_ms >= until_ms => {
                self.link = Link::Connecting;
                true
            }
            _ => false,
        }
    }

    pub fn feed_ok(&self) -> bool {
        self.link == Link::Open
    }

    pub fn link(&self) -> Link {
        self.link
    }
}

/// Parse-checked receiver and display radius from `/local/config.json`. Anything implausible
/// disables live mode rather than showing a wrong picture.
pub fn validate(lat: f64, lon: f64, elev_m: f64, radius_km: f64) -> Option<(Config, f64)> {
    let ok = lat.abs() <= 90.0
        && lon.abs() <= 180.0
        && (-500.0..=9000.0).contains(&elev_m)
        && radius_km > 0.0
        && radius_km <= 450.0; // tracker MAX_RANGE_M
    ok.then_some((
        Config {
            rx_lat_deg: lat,
            rx_lon_deg: lon,
            rx_elev_m: elev_m,
        },
        radius_km,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_cap_and_resets_after_data() {
        let mut r = Reconnect::default();
        r.start();
        let mut now = 0;
        let mut delays = vec![];
        for _ in 0..8 {
            r.on_closed(now); // handshake failed (down or 503)
            let Link::Waiting { until_ms } = r.link() else {
                panic!()
            };
            delays.push(until_ms - now);
            assert!(!r.poll(until_ms - 1));
            now = until_ms;
            assert!(r.poll(now));
            assert_eq!(r.link(), Link::Connecting);
        }
        assert_eq!(delays, [1000, 2000, 4000, 8000, 16000, 30000, 30000, 30000]);
        r.on_open();
        assert!(r.feed_ok());
        r.on_data();
        r.on_closed(now);
        assert!(!r.feed_ok());
        assert_eq!(
            r.link(),
            Link::Waiting {
                until_ms: now + 1000
            }
        );
    }

    #[test]
    fn open_without_data_keeps_backing_off_and_stop_is_final() {
        let mut r = Reconnect::default();
        r.start();
        r.on_open();
        r.on_closed(0);
        assert!(r.poll(1000));
        r.on_open();
        r.on_closed(1000);
        assert_eq!(r.link(), Link::Waiting { until_ms: 3000 });
        r.stop();
        r.on_closed(5000);
        assert!(!r.poll(u64::MAX) && !r.feed_ok());
        assert_eq!(r.link(), Link::Idle);
    }

    #[test]
    fn config_validation() {
        assert!(validate(47.0, 8.0, 400.0, 5.0).is_some());
        assert!(validate(91.0, 8.0, 0.0, 5.0).is_none());
        assert!(validate(47.0, 181.0, 0.0, 5.0).is_none());
        assert!(validate(f64::NAN, 8.0, 0.0, 5.0).is_none());
        assert!(validate(47.0, 8.0, 0.0, 0.0).is_none());
        assert!(validate(47.0, 8.0, 0.0, 1e6).is_none());
    }
}
