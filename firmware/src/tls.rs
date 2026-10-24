//! Verified HTTPS requests over embassy-net for the metadata lookups (docs/design/radar-ui.md RD.8, RD.9).
//! TLS 1.3 by embedded-tls with the chain checked against a pinned root; there is no unverified path.
//! Record buffers and the HTTP body live in PSRAM (plain allocations land there, see `main.rs`).
//! Logs name the public API hosts only, never the User-Agent contact.

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed};

use adsb_core::tlsauth::{self, OpenError};
use adsb_core::wire;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::{self, TcpSocket};
use embassy_net::{IpAddress, Stack};
use embassy_time::{Duration, Instant, with_timeout};
use embedded_io_async::{ErrorType, Read, Write};
use embedded_tls::{Aes128GcmSha256, TlsClock, TlsConnection, TlsError};
use esp_alloc::MemoryCapability;
use rand_chacha::ChaCha8Rng;
use rand_core::SeedableRng;

/// Pinned public roots (firmware/certs/README.md). Each host's server sends a chain whose last
/// certificate is signed by exactly one of these, so the pin is chosen per host.
const ISRG_ROOT_X1: &[u8] = include_bytes!("../certs/isrg-root-x1.der");
const GLOBALSIGN_ROOT_CA: &[u8] = include_bytes!("../certs/globalsign-root-ca.der");

/// Root for `host`; with `tls-wrong-ca` the other one, so that every handshake must fail.
fn pinned_root(host: &str) -> Option<&'static [u8]> {
    let (right, wrong) = match host {
        "api.adsbdb.com" | "t.plnspttrs.net" => (ISRG_ROOT_X1, GLOBALSIGN_ROOT_CA),
        "api.planespotters.net" => (GLOBALSIGN_ROOT_CA, ISRG_ROOT_X1),
        _ => return None,
    };
    Some(if cfg!(feature = "tls-wrong-ca") { wrong } else { right })
}

// Wall clock for certificate validity: unix seconds minus uptime seconds at the last SNTP sync.
// u32: no 64-bit atomics on Xtensa (good until 2106).
static UNIX_AT_BOOT: AtomicU32 = AtomicU32::new(0);

/// Build time (build.rs). SNTP is unauthenticated, so a time before it is refused: the clock stays
/// unsynced and no certificate is accepted.
const BUILD_UNIX: u64 = {
    match u64::from_str_radix(env!("ADSB_BUILD_UNIX"), 10) {
        Ok(t) => t,
        Err(_) => panic!("ADSB_BUILD_UNIX"),
    }
};

/// Returns false (clock unchanged) for a time before the firmware was built.
pub fn set_unix_time(unix_s: u64) -> bool {
    if unix_s < BUILD_UNIX {
        return false;
    }
    UNIX_AT_BOOT.store(unix_s.saturating_sub(Instant::now().as_secs()) as u32, Relaxed);
    true
}

/// Unix seconds, or `None` before the first SNTP sync.
pub fn unix_time() -> Option<u64> {
    match UNIX_AT_BOOT.load(Relaxed) {
        0 => None,
        at_boot => Some(u64::from(at_boot) + Instant::now().as_secs()),
    }
}

struct WallClock;

impl TlsClock for WallClock {
    // No time: the validity check then fails, so an unsynced clock never accepts a certificate.
    fn now() -> Option<u64> {
        Some(unix_time().unwrap_or(0))
    }
}

/// Seeds ChaCha8 from the hardware RNG (true random while the radio is on, which it is here).
fn seeded_rng() -> ChaCha8Rng {
    let rng = esp_hal::rng::Rng::new();
    let mut seed = [0u8; 32];
    seed.chunks_mut(4).for_each(|c| c.copy_from_slice(&rng.random().to_le_bytes()));
    ChaCha8Rng::from_seed(seed)
}

/// Lowest internal free heap seen at socket calls during the handshake and transfer.
pub static MIN_INTERNAL_FREE: AtomicUsize = AtomicUsize::new(usize::MAX);

pub fn internal_free() -> usize {
    esp_alloc::HEAP.free_caps(MemoryCapability::Internal.into())
}

fn sample_heap() {
    MIN_INTERNAL_FREE.fetch_min(internal_free(), Relaxed);
}

/// Socket that samples the heap at every call (the handshake crypto runs between those calls).
struct Sampled<'a>(TcpSocket<'a>);

impl ErrorType for Sampled<'_> {
    type Error = tcp::Error;
}

impl Read for Sampled<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        sample_heap();
        self.0.read(buf).await
    }
}

impl Write for Sampled<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        sample_heap();
        self.0.write(buf).await
    }
    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.flush().await
    }
}

#[derive(Debug)]
pub enum Error {
    Dns,
    Connect,
    Tls(TlsError),
    /// The handshake ended without a verified server Certificate and CertificateVerify.
    NoServerAuth,
    /// The reply had no complete HTTP head, or the transfer failed before one arrived.
    Http,
    /// A reused connection failed before the first reply byte: the server had closed it while idle.
    Stale,
}

pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    /// The body ended as the framing says (a body without any length ends by the close).
    pub complete: bool,
    /// The server allows another request on this connection.
    pub reusable: bool,
}

/// Data-bus window the S3 maps external RAM into.
const PSRAM_VADDR: core::ops::Range<usize> = 0x3C00_0000..0x3E00_0000;
const READ_BUF: usize = 16 * 1024 + 640; // largest TLS record the server may send
const WRITE_BUF: usize = 2048;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REPLY: usize = 32 * 1024;
const MAX_HEAD: usize = 4096;

/// TCP and TLS record buffers, allocated once (PSRAM) and lent to one [`Conn`] at a time.
pub struct Bufs {
    tcp_rx: Vec<u8>,
    tcp_tx: Vec<u8>,
    read: Vec<u8>,
    write: Vec<u8>,
}

impl Default for Bufs {
    fn default() -> Self {
        Self {
            tcp_rx: alloc::vec![0; 8192],
            tcp_tx: alloc::vec![0; 1024],
            read: alloc::vec![0; READ_BUF],
            write: alloc::vec![0; WRITE_BUF],
        }
    }
}

impl Bufs {
    /// The TLS record buffers sit in PSRAM (K1).
    pub fn in_psram(&self) -> bool {
        PSRAM_VADDR.contains(&(self.read.as_ptr() as usize))
    }
}

pub async fn resolve(stack: Stack<'_>, host: &str) -> Option<IpAddress> {
    let found = with_timeout(Duration::from_secs(5), stack.dns_query(host, DnsQueryType::A)).await;
    found.ok()?.ok()?.first().copied()
}

/// An authenticated TLS connection to one host; several requests may follow each other on it
/// (HTTP keep-alive), so that the slow handshake (seconds of CPU, it blocks the executor) is rare.
pub struct Conn<'a> {
    conn: TlsConnection<'a, Sampled<'a>, Aes128GcmSha256>,
    host: &'static str,
    pub handshake_ms: u64,
    pub requests: u32,
    pub last_used: Instant,
}

impl<'a> Conn<'a> {
    pub async fn open(stack: Stack<'a>, host: &'static str, bufs: &'a mut Bufs) -> Result<Conn<'a>, Error> {
        let root = pinned_root(host).ok_or(Error::Connect)?;
        let ip = resolve(stack, host).await.ok_or(Error::Dns)?;
        let Bufs { tcp_rx, tcp_tx, read, write } = bufs;
        let mut socket = TcpSocket::new(stack, tcp_rx, tcp_tx);
        match with_timeout(CONNECT_TIMEOUT, socket.connect((ip, 443))).await {
            Ok(Ok(())) => {}
            _ => return Err(Error::Connect),
        }
        let mut conn = TlsConnection::<_, Aes128GcmSha256>::new(Sampled(socket), read, write);
        let t0 = Instant::now();
        // Nothing is written before this returns Ok.
        tlsauth::open_verified::<_, WallClock>(&mut conn, host, root, seeded_rng())
            .await
            .map_err(|e| match e {
                OpenError::Tls(e) => Error::Tls(e),
                OpenError::NoServerAuth => Error::NoServerAuth,
            })?;
        sample_heap();
        Ok(Conn { conn, host, handshake_ms: t0.elapsed().as_millis(), requests: 0, last_used: Instant::now() })
    }

    /// `GET <path>` on this connection. The reply ends by Content-Length, by the last chunk, or
    /// (neither given) by the server closing, which makes the connection not reusable.
    pub async fn get(&mut self, path: &str, user_agent: &str) -> Result<Reply, Error> {
        let reused = self.requests > 0;
        self.requests += 1;
        let host = self.host;
        let request = alloc::format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {user_agent}\r\nAccept: */*\r\n\r\n"
        );
        // After a failure on a reused connection nothing tells a stale connection from a dead network
        // until a whole reply is missing: `Stale` lets the caller retry on a fresh one.
        let fail = |e| if reused { Error::Stale } else { e };
        self.conn.write_all(request.as_bytes()).await.map_err(|_| fail(Error::Http))?;
        self.conn.flush().await.map_err(|_| fail(Error::Http))?;

        let mut data: Vec<u8> = Vec::new();
        let mut chunk = alloc::vec![0u8; 1024];
        let mut head: Option<wire::Head> = None;
        let mut closed = false;
        loop {
            if head.is_none() {
                head = wire::parse_head(&data);
                // A server that never ends its head must not grow the buffer unchecked.
                if head.is_none() && data.len() > MAX_HEAD {
                    return Err(Error::Http);
                }
            }
            if let Some(h) = &head {
                if h.bad_framing {
                    return Err(Error::Http);
                }
                let rest = &data[h.body_start..];
                if matches!(wire::body(h, rest), wire::Body::Done { .. }) || rest.len() > MAX_REPLY {
                    break;
                }
            }
            match with_timeout(IO_TIMEOUT, self.conn.read(&mut chunk)).await {
                Ok(Ok(0)) | Ok(Err(TlsError::ConnectionClosed)) => {
                    closed = true;
                    break;
                }
                Ok(Ok(n)) => data.extend_from_slice(&chunk[..n]),
                // A reset after the head ends the body early: the reply is marked incomplete.
                _ if head.is_some() => {
                    closed = true;
                    break;
                }
                _ if data.is_empty() => return Err(fail(Error::Http)),
                _ => return Err(Error::Http),
            }
        }
        self.last_used = Instant::now();
        let head = head.ok_or_else(|| fail(Error::Http))?;
        let rest = &data[head.body_start..];
        Ok(match wire::body(&head, rest) {
            wire::Body::Done { body, exact } => {
                Reply { status: head.status, body, complete: true, reusable: exact && !closed && !head.close }
            }
            // Ended by close (no length) or cut short: the caller must not trust the body.
            wire::Body::Incomplete => {
                Reply { status: head.status, body: rest.to_vec(), complete: !head.chunked && head.content_length.is_none() && closed, reusable: false }
            }
        })
    }

    pub async fn close(self) {
        let _ = self.conn.close().await;
    }
}
