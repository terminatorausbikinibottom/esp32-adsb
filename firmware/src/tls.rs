//! Verified HTTPS GET over embassy-net for the metadata lookups (docs/design/radar-ui.md RD.8).
//! TLS 1.3 by embedded-tls with the chain checked against a pinned root; there is no unverified path.
//! Record buffers and the HTTP body live in PSRAM (plain allocations land there, see `main.rs`).
//! Logs name the public API hosts only, never the User-Agent contact.

extern crate alloc;

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering::Relaxed};

use adsb_core::wire;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::{self, TcpSocket};
use embassy_net::{IpAddress, Stack};
use embassy_time::{Duration, Instant, with_timeout};
use embedded_io_async::{ErrorType, Read, Write};
use embedded_tls::pki::CertVerifier;
use embedded_tls::{
    Aes128GcmSha256, Certificate, CryptoProvider, TlsClock, TlsConfig, TlsConnection, TlsContext,
    TlsError, TlsVerifier,
};
use esp_alloc::MemoryCapability;
use rand_chacha::ChaCha8Rng;
use rand_core::{CryptoRngCore, SeedableRng};

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

pub fn set_unix_time(unix_s: u64) {
    UNIX_AT_BOOT.store(unix_s.saturating_sub(Instant::now().as_secs()) as u32, Relaxed);
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

/// Room for the server's whole certificate chain (the verifier keeps a copy; the RSA chain of the image
/// host is about 5.5 KB). A longer chain fails the handshake with `OutOfMemory`.
const CHAIN_BYTES: usize = 8192;

struct Provider<'a> {
    rng: ChaCha8Rng,
    verifier: CertVerifier<'a, Aes128GcmSha256, WallClock, CHAIN_BYTES>,
}

impl CryptoProvider for Provider<'_> {
    type CipherSuite = Aes128GcmSha256;
    type Signature = [u8; 0]; // no client certificate

    fn rng(&mut self) -> impl CryptoRngCore {
        &mut self.rng
    }

    fn verifier(&mut self) -> Result<&mut impl TlsVerifier<Self::CipherSuite>, TlsError> {
        Ok(&mut self.verifier)
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
    /// The reply had no complete HTTP head, or the transfer failed before one arrived.
    Http,
}

pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    pub handshake_ms: u64,
    /// The TLS record buffers sit in PSRAM (K1).
    pub bufs_in_psram: bool,
}

/// Data-bus window the S3 maps external RAM into.
const PSRAM_VADDR: core::ops::Range<usize> = 0x3C00_0000..0x3E00_0000;
const READ_BUF: usize = 16 * 1024 + 640; // largest TLS record the server may send
const WRITE_BUF: usize = 2048;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REPLY: usize = 32 * 1024;

pub async fn resolve(stack: Stack<'_>, host: &str) -> Option<IpAddress> {
    let found = with_timeout(Duration::from_secs(5), stack.dns_query(host, DnsQueryType::A)).await;
    found.ok()?.ok()?.first().copied()
}

/// `GET https://<host><path>`. Reads until the server closes or `stop_after` body bytes arrived
/// (then the connection is closed early; pair it with a `Range` header). A chunked body is decoded.
pub async fn get(
    stack: Stack<'_>,
    host: &str,
    path: &str,
    user_agent: &str,
    range_to: Option<usize>,
    stop_after: usize,
) -> Result<Reply, Error> {
    let root = pinned_root(host).ok_or(Error::Connect)?;
    let ip = resolve(stack, host).await.ok_or(Error::Dns)?;

    let mut tcp_rx = alloc::vec![0u8; 8192];
    let mut tcp_tx = alloc::vec![0u8; 1024];
    let mut socket = TcpSocket::new(stack, &mut tcp_rx, &mut tcp_tx);
    match with_timeout(CONNECT_TIMEOUT, socket.connect((ip, 443))).await {
        Ok(Ok(())) => {}
        _ => return Err(Error::Connect),
    }

    let mut read_buf = alloc::vec![0u8; READ_BUF];
    let mut write_buf = alloc::vec![0u8; WRITE_BUF];
    let config = TlsConfig::new().with_server_name(host);
    let mut conn = TlsConnection::<_, Aes128GcmSha256>::new(Sampled(socket), &mut read_buf, &mut write_buf);
    let provider = Provider {
        rng: seeded_rng(),
        verifier: CertVerifier::new(Certificate::X509(root)),
    };
    let t0 = Instant::now();
    conn.open(TlsContext::new(&config, provider)).await.map_err(Error::Tls)?;
    let handshake_ms = t0.elapsed().as_millis();
    sample_heap();

    let mut request = alloc::format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: {user_agent}\r\nAccept: */*\r\nConnection: close\r\n"
    );
    if let Some(last) = range_to {
        request.push_str(&alloc::format!("Range: bytes=0-{last}\r\n"));
    }
    request.push_str("\r\n");
    conn.write_all(request.as_bytes()).await.map_err(|_| Error::Http)?;
    conn.flush().await.map_err(|_| Error::Http)?;

    let mut data: Vec<u8> = Vec::new();
    let mut chunk = alloc::vec![0u8; 1024];
    let mut head = None;
    loop {
        let n = match with_timeout(IO_TIMEOUT, conn.read(&mut chunk)).await {
            Ok(Ok(0)) | Ok(Err(TlsError::ConnectionClosed)) => break,
            Ok(Ok(n)) => n,
            // A reset after the head is a short body, not a failure.
            _ if head.is_some() => break,
            _ => return Err(Error::Http),
        };
        data.extend_from_slice(&chunk[..n]);
        head = head.or_else(|| wire::parse_head(&data));
        if head.as_ref().is_some_and(|h| data.len() - h.body_start >= stop_after) || data.len() > MAX_REPLY {
            break;
        }
    }
    let _ = conn.close().await;

    let head = head.ok_or(Error::Http)?;
    let body = &data[head.body_start..];
    let body = if head.chunked { wire::dechunk(body).0 } else { body.to_vec() };
    let bufs_in_psram = PSRAM_VADDR.contains(&(read_buf.as_ptr() as usize));
    Ok(Reply { status: head.status, body, handshake_ms, bufs_in_psram })
}
