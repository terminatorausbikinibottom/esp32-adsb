//! WiFi station, network stack and live feed tasks (core 0). Logs never contain the SSID, the key,
//! the feed host or coordinates (see docs/design/radar-ui.md, Configuration).

extern crate alloc;

use core::net::Ipv4Addr;
use core::str::FromStr;
use core::sync::atomic::{AtomicBool, Ordering::Relaxed};

use embassy_net::tcp::TcpSocket;
use embassy_net::udp::{PacketMetadata, UdpSocket};
use embassy_net::{Config, Runner, Stack, StackResources};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use esp_radio::wifi::sta::StationConfig;
use esp_radio::wifi::{AuthenticationMethodConfig, Config as WifiConfig, Interface, WifiController};
use log::{info, warn};

use adsb_core::meta;
use adsb_core::wire;

use crate::config;
use crate::tls;
use crate::ui::{self, Pipeline};

/// True while the station has an IP address.
pub static WIFI_OK: AtomicBool = AtomicBool::new(false);

const RX_BYTES: usize = 8192; // about 0.5 s of stall at feed rates (JPEG decode later)
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const SOCKETS: usize = 5; // DHCP, DNS, feed, one for SNTP or HTTPS at a time, spare

pub type NetRunner = Runner<'static, Interface>;

/// Creates the stack and spawns the tasks. Call once, after the scanout runs.
pub fn start(
    spawner: embassy_executor::Spawner,
    controller: WifiController<'static>,
    pipeline: &'static Pipeline,
) {
    static RESOURCES: static_cell::StaticCell<StackResources<SOCKETS>> =
        static_cell::StaticCell::new();
    let seed = u64::from(esp_hal::rng::Rng::new().random()) << 32
        | u64::from(esp_hal::rng::Rng::new().random());
    let (stack, runner) = embassy_net::new(
        Interface::station(),
        Config::dhcpv4(Default::default()),
        RESOURCES.init(StackResources::new()),
        seed,
    );
    spawner.spawn(wifi_task(controller).expect("wifi_task"));
    spawner.spawn(net_task(runner).expect("net_task"));
    spawner.spawn(link_task(stack).expect("link_task"));
    spawner.spawn(feed_task(stack, pipeline).expect("feed_task"));
    spawner.spawn(sntp_task(stack).expect("sntp_task"));
    spawner.spawn(tls_spike_task(stack).expect("tls_spike_task"));
}

#[embassy_executor::task]
async fn wifi_task(mut controller: WifiController<'static>) {
    let password = config::WIFI_KEY.try_into().expect("WIFI_KEY");
    let station = StationConfig::default()
        .with_ssid(config::WIFI_SSID.try_into().expect("WIFI_SSID"))
        .with_authentication(AuthenticationMethodConfig::Wpa2Personal(password));
    controller
        .set_config(&WifiConfig::Station(station))
        .expect("wifi: set_config");
    let mut backoff = BACKOFF_MIN;
    loop {
        match controller.connect_async().await {
            Ok(_) => {
                info!("wifi: associated");
                backoff = BACKOFF_MIN;
                let _ = controller.wait_for_disconnect_async().await;
                warn!("wifi: disconnected");
            }
            Err(e) => {
                warn!("wifi: connect failed ({e:?}), retry in {} s", backoff.as_secs());
                Timer::after(backoff).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
        // A fresh association attempt right after a drop would spin; keep a minimum pause.
        Timer::after(BACKOFF_MIN).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: NetRunner) {
    runner.run().await
}

#[embassy_executor::task]
async fn link_task(stack: Stack<'static>) {
    loop {
        stack.wait_config_up().await;
        if let Some(cfg) = stack.config_v4() {
            info!("wifi: connected ip={}", cfg.address.address());
        }
        WIFI_OK.store(true, Relaxed);
        stack.wait_config_down().await;
        WIFI_OK.store(false, Relaxed);
        warn!("wifi: no IP address");
    }
}

const NTP_HOST: &str = "pool.ntp.org";
const SNTP_REFRESH: Duration = Duration::from_secs(6 * 3600);

/// Sets the wall clock (certificate validity, status bar) at boot and every 6 h.
#[embassy_executor::task]
async fn sntp_task(stack: Stack<'static>) {
    let mut backoff = BACKOFF_MIN;
    loop {
        stack.wait_config_up().await;
        let t0 = Instant::now();
        match sntp_once(stack).await {
            Some(unix) if !tls::set_unix_time(unix) => {
                warn!("sntp: time before the firmware build, ignored, retry in {} s", backoff.as_secs());
                Timer::after(backoff).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
            Some(unix) => {
                let (y, mo, d, h, mi, s) = wire::utc(unix);
                info!(
                    "sntp: time={y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z rtt_ms={}",
                    t0.elapsed().as_millis()
                );
                backoff = BACKOFF_MIN;
                Timer::after(SNTP_REFRESH).await;
            }
            None => {
                warn!("sntp: no valid reply, retry in {} s", backoff.as_secs());
                Timer::after(backoff).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

async fn sntp_once(stack: Stack<'static>) -> Option<u64> {
    let server = tls::resolve(stack, NTP_HOST).await?;
    let (mut rx_meta, mut tx_meta) = ([PacketMetadata::EMPTY; 1], [PacketMetadata::EMPTY; 1]);
    let (mut rx, mut tx) = ([0u8; 64], [0u8; 64]);
    let mut socket = UdpSocket::new(stack, &mut rx_meta, &mut rx, &mut tx_meta, &mut tx);
    socket.bind(0).ok()?;
    socket.send_to(&wire::SNTP_REQUEST, (server, 123)).await.ok()?;
    let mut reply = [0u8; 64];
    let (n, from) = with_timeout(Duration::from_secs(3), socket.recv_from(&mut reply)).await.ok()?.ok()?;
    // Only the server we asked may answer.
    (from.endpoint.addr == server).then(|| wire::parse_sntp(&reply[..n])).flatten()
}

/// One-shot at boot (RD.8 spike; RD.9 replaces it by `meta_task`): a verified HTTPS GET to each of
/// the three metadata hosts, logging handshake time, status and heap.
#[embassy_executor::task]
async fn tls_spike_task(stack: Stack<'static>) {
    while tls::unix_time().is_none() {
        Timer::after(Duration::from_secs(1)).await;
    }
    Timer::after(Duration::from_secs(2)).await; // let the feed settle first
    let ua = match config::HTTP_CONTACT {
        Some(contact) => alloc::format!("esp32-adsb/{} (+{contact})", env!("CARGO_PKG_VERSION")),
        None => alloc::format!("esp32-adsb/{}", env!("CARGO_PKG_VERSION")),
    };
    info!("tls: spike start contact={} wrong_ca={}", config::HTTP_CONTACT.is_some(), cfg!(feature = "tls-wrong-ca"));

    let adsbdb = probe(stack, &ua, "api.adsbdb.com", "/v0/callsign/KLM1023", None, usize::MAX).await;
    if let Some(r) = adsbdb.filter(|r| r.status == 200) {
        match meta::parse_adsbdb(&r.body) {
            Ok((_, route)) => {
                let iata = |a: Option<&meta::Airport>| a.and_then(|a| a.iata.clone()).unwrap_or_default();
                let route = route.map(|r| alloc::format!("{}-{}", iata(r.origin.as_ref()), iata(r.destination.as_ref())));
                info!("meta: parse ok kind=adsbdb route={}", route.as_deref().unwrap_or("-"));
            }
            Err(e) => warn!("meta: parse failed kind=adsbdb err={e:?}"),
        }
    }
    Timer::after(Duration::from_secs(1)).await; // at most 1 request/s

    // A fixed public example aircraft (design: "a fixed public example").
    let ps = probe(stack, &ua, "api.planespotters.net", "/pub/photos/hex/3C6444", None, usize::MAX).await;
    let photo = ps.and_then(|r| match meta::parse_planespotters(&r.body) {
        Ok(photo) => {
            info!("meta: parse ok kind=planespotters photo={}", photo.is_some());
            photo
        }
        Err(e) => {
            // Without a contact the API answers with an error object; the TLS part is still proven.
            warn!("meta: parse failed kind=planespotters status={} err={e:?}", r.status);
            None
        }
    });
    Timer::after(Duration::from_secs(1)).await;

    // The image URL comes from the answer; if there is none, any path proves the handshake.
    let path = photo.as_ref().and_then(|p| p.src.strip_prefix(meta::PHOTO_HOST_PREFIX)).unwrap_or("");
    let path = alloc::format!("/{path}");
    if let Some(r) = probe(stack, &ua, "t.plnspttrs.net", &path, Some(1023), 1024).await {
        info!("img: first_bytes_jpeg={}", r.body.starts_with(&[0xFF, 0xD8]));
    }
    info!("tls: spike done");
}

/// One HTTPS GET with log lines: `tls: ok|failed host=..`, `http: ..`, `tls: heap ..`.
async fn probe(
    stack: Stack<'static>,
    ua: &str,
    host: &str,
    path: &str,
    range_to: Option<usize>,
    stop_after: usize,
) -> Option<tls::Reply> {
    tls::MIN_INTERNAL_FREE.store(usize::MAX, Relaxed);
    let t0 = Instant::now();
    let result = tls::get(stack, host, path, ua, range_to, stop_after).await;
    let total_ms = t0.elapsed().as_millis();
    let stats = esp_alloc::HEAP.stats();
    let reply = match result {
        Ok(r) => {
            info!("tls: ok host={host} stack=embedded-tls verify=on hs_ms={} bufs_psram={}", r.handshake_ms, r.bufs_in_psram);
            info!("http: host={host} status={} body_bytes={} total_ms={total_ms}", r.status, r.body.len());
            Some(r)
        }
        Err(e) => {
            warn!("tls: failed host={host} err={e:?} total_ms={total_ms}");
            None
        }
    };
    info!(
        "tls: heap host={host} internal_min={} internal_free={} peak_used={}",
        tls::MIN_INTERNAL_FREE.load(Relaxed),
        tls::internal_free(),
        stats.max_usage
    );
    reply
}

#[embassy_executor::task]
async fn feed_task(stack: Stack<'static>, pipeline: &'static Pipeline) {
    let Some(host) = config::FEED_HOST.and_then(|h| Ipv4Addr::from_str(h).ok()) else {
        warn!("feed: ADSB_FEED_HOST is not an IPv4 literal (host names need DNS, RD.8)");
        return;
    };
    // PSRAM (plain allocations land there), leaked: the task lives for the whole run.
    let rx: &'static mut [u8] = alloc::vec![0u8; RX_BYTES].leak();
    let tx: &'static mut [u8] = alloc::vec![0u8; 256].leak();
    let mut chunk = alloc::vec![0u8; 1024];
    let mut backoff = BACKOFF_MIN;
    let mut stats = Stats::new();
    loop {
        stack.wait_config_up().await;
        let mut socket = TcpSocket::new(stack, &mut *rx, &mut *tx);
        match with_timeout(CONNECT_TIMEOUT, socket.connect((host, config::FEED_PORT))).await {
            Ok(Ok(())) => {
                info!("feed: connected");
                backoff = BACKOFF_MIN;
                pipeline.lock().await.reset_clock();
                ui::FEED_OK.store(true, Relaxed);
                stats.reconnects += 1;
                let why = pump(&mut socket, &mut chunk, pipeline, &mut stats).await;
                ui::FEED_OK.store(false, Relaxed);
                warn!("feed: lost ({why})");
            }
            Ok(Err(e)) => warn!("feed: connect failed ({e:?})"),
            Err(_) => warn!("feed: connect timed out"),
        }
        socket.abort();
        Timer::after(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// Reads until the socket fails or stays silent for `IDLE_TIMEOUT`; logs the 10 s statistics.
async fn pump(
    socket: &mut TcpSocket<'_>,
    chunk: &mut [u8],
    pipeline: &Pipeline,
    stats: &mut Stats,
) -> &'static str {
    loop {
        match with_timeout(IDLE_TIMEOUT, socket.read(chunk)).await {
            Err(_) => return "idle timeout",
            Ok(Ok(0)) => return "closed by peer",
            Ok(Err(_)) => return "read error",
            Ok(Ok(n)) => {
                pipeline.lock().await.feed(&chunk[..n], Instant::now().as_millis());
                stats.log_if_due(pipeline).await;
            }
        }
    }
}

struct Stats {
    last: Instant,
    accepted: u32,
    undecoded: u32,
    reconnects: u32,
}

impl Stats {
    fn new() -> Self {
        Self { last: Instant::now(), accepted: 0, undecoded: 0, reconnects: 0 }
    }

    /// Every 10 s: frames/s (accepted AVR frames), CRC/DF rejects in the window, aircraft count.
    async fn log_if_due(&mut self, pipeline: &Pipeline) {
        let secs = self.last.elapsed().as_secs();
        if secs < 10 {
            return;
        }
        let c = pipeline.lock().await.counters();
        info!(
            "feed: frames/s={} crc_fail={} aircraft={} reconnects={}",
            c.source.accepted.wrapping_sub(self.accepted) / secs as u32,
            c.undecoded.wrapping_sub(self.undecoded),
            ui::AIRCRAFT.load(Relaxed),
            self.reconnects.saturating_sub(1),
        );
        (self.accepted, self.undecoded, self.last) = (c.source.accepted, c.undecoded, Instant::now());
    }
}
