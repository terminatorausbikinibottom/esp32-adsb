//! Runs `adsb_core::tlsauth::open_verified` (the firmware's TLS open) against `srv.py`, a hand-made
//! TLS 1.3 server that can omit or forge the authentication messages. See README.md.

use adsb_core::tlsauth::{OpenError, open_verified};
use embedded_io_adapters::tokio_1::FromTokio;
use embedded_tls::{Aes128GcmSha256, TlsClock, TlsConnection, TlsError};
use rand_chacha::ChaCha8Rng;
use rand_core::SeedableRng;
use std::sync::{Once, atomic::{AtomicU16, Ordering}};

struct Now;
impl TlsClock for Now {
    fn now() -> Option<u64> {
        Some(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs())
    }
}
struct Unsynced;
impl TlsClock for Unsynced {
    fn now() -> Option<u64> {
        Some(0)
    }
}

fn dir() -> String {
    format!("{}/target/fixtures", env!("CARGO_MANIFEST_DIR"))
}

fn fixtures() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let ok = std::process::Command::new("sh")
            .args([&format!("{}/gen.sh", env!("CARGO_MANIFEST_DIR")), &dir()])
            .status()
            .expect("sh gen.sh (needs openssl)")
            .success();
        assert!(ok, "gen.sh failed");
    });
}

async fn handshake<C: TlsClock>(mode: &str) -> Result<(), OpenError> {
    fixtures();
    static PORT: AtomicU16 = AtomicU16::new(24500);
    let port = PORT.fetch_add(1, Ordering::SeqCst);
    let python = std::env::var("TLS_HOSTILE_PYTHON").unwrap_or_else(|_| "python3".into());
    let mut srv = std::process::Command::new(python)
        .args(["-I", &format!("{}/srv.py", env!("CARGO_MANIFEST_DIR")), mode, &port.to_string(), &dir()])
        .spawn()
        .expect("python3 with `cryptography` (see README.md)");
    let mut sock = None;
    for _ in 0..100 {
        if let Ok(s) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
            sock = Some(s);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let sock = sock.expect("srv.py did not start (is `cryptography` installed for TLS_HOSTILE_PYTHON?)");
    let root = std::fs::read(format!("{}/root.der", dir())).unwrap();
    let (mut r, mut w) = (vec![0u8; 16640], vec![0u8; 2048]);
    let mut conn = TlsConnection::<_, Aes128GcmSha256>::new(FromTokio::new(sock), &mut r, &mut w);
    let res = open_verified::<_, C>(&mut conn, "victim.test", &root, ChaCha8Rng::from_seed([7; 32])).await;
    let _ = srv.kill();
    let _ = srv.wait();
    res
}

#[tokio::test]
async fn good_flow_is_accepted() {
    handshake::<Now>("good").await.unwrap();
}

#[tokio::test]
async fn no_certificate_is_refused() {
    let r = handshake::<Now>("nocert").await;
    assert!(matches!(r, Err(OpenError::NoServerAuth)), "{r:?}");
}

#[tokio::test]
async fn real_chain_without_certificate_verify_is_refused() {
    let r = handshake::<Now>("nocv").await;
    assert!(matches!(r, Err(OpenError::NoServerAuth)), "{r:?}");
}

#[tokio::test]
async fn forged_chain_is_invalid_certificate_with_or_without_proof() {
    for mode in ["nocv_bad", "forged"] {
        let r = handshake::<Now>(mode).await;
        assert!(matches!(r, Err(OpenError::Tls(TlsError::InvalidCertificate))), "{mode}: {r:?}");
    }
}

#[tokio::test]
async fn leaf_signed_by_an_ordinary_leaf_is_refused() {
    let r = handshake::<Now>("leafsigned").await;
    assert!(matches!(r, Err(OpenError::Tls(TlsError::InvalidCertificate))), "{r:?}");
}

#[tokio::test]
async fn unsynced_clock_refuses_the_good_flow() {
    let r = handshake::<Unsynced>("good").await;
    assert!(matches!(r, Err(OpenError::Tls(TlsError::InvalidCertificate))), "{r:?}");
}
