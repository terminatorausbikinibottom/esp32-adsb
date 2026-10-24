//! The only way the firmware opens a TLS connection: pinned-root chain verification plus the proof that
//! the server really authenticated (radar-ui RD.8 fix 2).
//!
//! embedded-tls 0.19 calls the verifier only for a Certificate or CertificateVerify message that
//! arrives; it does not require either before the server Finished. A man in the middle could send no
//! certificate at all, or replay the public chain without CertificateVerify, and `open()` succeeds.
//! [`open_verified`] records what the verifier saw and fails unless both messages were verified.
//! Host-tested against a hostile server in `tools/tls-hostile`.

use core::cell::Cell;

use embedded_io_async::{Read, Write};
use embedded_tls::pki::CertVerifier;
use embedded_tls::{
    Aes128GcmSha256, Certificate, CertificateEntryRef, CertificateRef, CertificateVerifyRef,
    CryptoProvider, CryptoRngCore, TlsCipherSuite, TlsClock, TlsConfig, TlsConnection, TlsContext,
    TlsError, TlsVerifier,
};

use crate::x509;

/// Room for the server's whole certificate chain (the verifier keeps a copy; the RSA chain of the image
/// host is about 5.5 KB). A longer chain fails the handshake with `OutOfMemory`.
const CHAIN_BYTES: usize = 8192;

#[derive(Debug)]
pub enum OpenError {
    Tls(TlsError),
    /// The handshake finished without a verified Certificate and CertificateVerify.
    NoServerAuth,
}

/// What the verifier has accepted so far.
#[derive(Default)]
struct Seen {
    cert: Cell<bool>,
    signature: Cell<bool>,
}

/// Structural chain rules (`x509`) in front of embedded-tls' signature, validity and host name checks.
struct Checked<'a, C: TlsClock> {
    inner: CertVerifier<'a, Aes128GcmSha256, C, CHAIN_BYTES>,
    root: &'a [u8],
    seen: &'a Seen,
}

impl<C: TlsClock> TlsVerifier<Aes128GcmSha256> for Checked<'_, C> {
    fn set_hostname_verification(&mut self, hostname: &str) -> Result<(), TlsError> {
        self.inner.set_hostname_verification(hostname)
    }

    fn verify_certificate(
        &mut self,
        transcript: &<Aes128GcmSha256 as TlsCipherSuite>::Hash,
        cert: CertificateRef,
    ) -> Result<(), TlsError> {
        // A new certificate needs its own CertificateVerify.
        self.seen.cert.set(false);
        self.seen.signature.set(false);
        let mut chain: alloc::vec::Vec<&[u8]> = alloc::vec::Vec::new();
        for e in &cert.entries {
            match e {
                CertificateEntryRef::X509(der) => chain.push(der),
                CertificateEntryRef::RawPublicKey(_) => return Err(TlsError::InvalidCertificate),
            }
        }
        x509::check_chain(&chain, self.root).map_err(|_| TlsError::InvalidCertificate)?;
        self.inner.verify_certificate(transcript, cert)?;
        self.seen.cert.set(true);
        Ok(())
    }

    fn verify_signature(&mut self, verify: CertificateVerifyRef) -> Result<(), TlsError> {
        // Forward only in the exact expected state: embedded-tls unwraps the certificate transcript,
        // which is gone after the first CertificateVerify (a repeat would panic) or never set.
        if !self.seen.cert.get() || self.seen.signature.get() {
            return Err(TlsError::InvalidCertificate);
        }
        self.inner.verify_signature(verify)?;
        self.seen.signature.set(true);
        Ok(())
    }
}

struct Provider<'a, R, C: TlsClock> {
    rng: R,
    verifier: Checked<'a, C>,
}

impl<R: CryptoRngCore, C: TlsClock> CryptoProvider for Provider<'_, R, C> {
    type CipherSuite = Aes128GcmSha256;
    type Signature = [u8; 0]; // no client certificate

    fn rng(&mut self) -> impl CryptoRngCore {
        &mut self.rng
    }

    fn verifier(&mut self) -> Result<&mut impl TlsVerifier<Self::CipherSuite>, TlsError> {
        Ok(&mut self.verifier)
    }
}

/// TLS 1.3 handshake with `host`, verified against the pinned `root` (DER). `Ok` means the server
/// proved possession of a certificate chain that passes every check; send nothing before it.
/// On error the caller must drop the connection.
pub async fn open_verified<S: Read + Write, C: TlsClock>(
    conn: &mut TlsConnection<'_, S, Aes128GcmSha256>,
    host: &str,
    root: &[u8],
    rng: impl CryptoRngCore,
) -> Result<(), OpenError> {
    let seen = Seen::default();
    let config = TlsConfig::new().with_server_name(host);
    let provider = Provider {
        rng,
        verifier: Checked::<C> {
            inner: CertVerifier::new(Certificate::X509(root)),
            root,
            seen: &seen,
        },
    };
    conn.open(TlsContext::new(&config, provider)).await.map_err(OpenError::Tls)?;
    if seen.cert.get() && seen.signature.get() {
        Ok(())
    } else {
        Err(OpenError::NoServerAuth)
    }
}
