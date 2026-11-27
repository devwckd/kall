//! Connecting to a kall server over QUIC.
//!
//! A server reached by domain name must present a certificate a public CA (Let's Encrypt)
//! vouches for. A server reached by IP address or `localhost` (development, LAN) usually has a
//! self-signed certificate: the client pins its SHA-256 fingerprint the first time (trust on
//! first use) and refuses a different one afterwards. A pin saved earlier for a name also
//! still works, so a server that was self-signed before getting a real certificate keeps
//! working through the switch.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use rustls::DigitallySignedStruct;
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};

pub fn fingerprint(cert: &[u8]) -> String {
    Sha256::digest(cert).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

#[derive(Debug, Default, Clone)]
struct Seen {
    fingerprint: Option<String>,
    ca_verified: bool,
}

#[derive(Debug)]
struct Verifier {
    expected: Option<String>,
    /// Self-signed certificates may be trusted on first use (IP / localhost servers).
    tofu: bool,
    webpki: Arc<WebPkiServerVerifier>,
    seen: Arc<Mutex<Seen>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = fingerprint(end_entity);
        let ca = self.webpki.verify_server_cert(end_entity, intermediates, server_name, ocsp, now);
        *self.seen.lock().unwrap() = Seen { fingerprint: Some(fp.clone()), ca_verified: ca.is_ok() };
        if ca.is_ok() {
            return ca;
        }
        match &self.expected {
            Some(pin) if *pin == fp => Ok(ServerCertVerified::assertion()),
            Some(pin) => Err(rustls::Error::General(format!("server certificate changed (expected {pin}, got {fp})"))),
            None if self.tofu => Ok(ServerCertVerified::assertion()),
            None => Err(rustls::Error::General(format!(
                "the server's certificate isn't trusted for {}: {} (a self-signed development server can be reached by IP address instead)",
                server_name.to_str(),
                ca.err().map_or_else(String::new, |e| e.to_string())
            ))),
        }
    }

    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

pub struct Connected {
    pub endpoint: quinn::Endpoint,
    pub conn: quinn::Connection,
    pub send: quinn::SendStream,
    pub recv: quinn::RecvStream,
    /// The server certificate's fingerprint.
    pub fingerprint: String,
    /// A public CA vouched for the certificate: nothing to pin (it renews every few months).
    pub ca_verified: bool,
}

/// Split `host[:port]` (or `[v6]:port`) and resolve it.
pub async fn resolve(addr: &str) -> Result<(String, SocketAddr)> {
    let addr = addr.trim();
    let (host, port) = match addr.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) && (!h.contains(':') || h.starts_with('[')) => {
            (h.trim_matches(['[', ']']).to_string(), p.parse()?)
        }
        _ => (addr.trim_matches(['[', ']']).to_string(), crate::DEFAULT_PORT),
    };
    let sock = tokio::net::lookup_host((host.as_str(), port))
        .await
        .with_context(|| format!("can't find {host}"))?
        .next()
        .ok_or_else(|| anyhow!("can't find {host}"))?;
    Ok((host, sock))
}

/// Connect, check the pin and open the control stream.
pub async fn connect(addr: &str, pin: Option<String>) -> Result<Connected> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let (host, sock) = resolve(addr).await?;
    let seen = Arc::new(Mutex::new(Seen::default()));
    let roots = Arc::new(rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() });
    let webpki = WebPkiServerVerifier::builder_with_provider(roots, provider.clone()).build()?;
    let is_ip = host.parse::<std::net::IpAddr>().is_ok();
    let tofu = is_ip || host.eq_ignore_ascii_case("localhost") || std::env::var_os("KALL_TRUST_SELF_SIGNED").is_some();
    let verifier = Arc::new(Verifier { expected: pin, tofu, webpki, seen: seen.clone(), provider: provider.clone() });
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    tls.alpn_protocols = vec![crate::ALPN.to_vec()];
    let crypto = quinn::crypto::rustls::QuicClientConfig::try_from(tls)?;
    let mut cfg = quinn::ClientConfig::new(Arc::new(crypto));
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(std::time::Duration::from_secs(10)));
    transport.max_idle_timeout(Some(std::time::Duration::from_secs(30).try_into()?));
    cfg.transport_config(Arc::new(transport));

    let bind: SocketAddr = if sock.is_ipv6() { "[::]:0".parse()? } else { "0.0.0.0:0".parse()? };
    let mut endpoint = quinn::Endpoint::client(bind)?;
    endpoint.set_default_client_config(cfg);
    let sni = if is_ip { "localhost" } else { host.as_str() };
    let conn = endpoint.connect(sock, sni)?.await.map_err(|e| anyhow!("can't reach {addr}: {e}"))?;
    let (send, recv) = conn.open_bi().await?;
    let seen = seen.lock().unwrap().clone();
    Ok(Connected { endpoint, conn, send, recv, fingerprint: seen.fingerprint.unwrap_or_default(), ca_verified: seen.ca_verified })
}

// ---- file transfers (see `crate::files`) ----

/// Upload `size` bytes from `src` as a file named `name`, on a stream of its own. `progress`
/// gets the bytes sent so far. Dropping the future cancels the upload.
pub async fn upload<R: tokio::io::AsyncRead + Unpin>(
    conn: &quinn::Connection,
    name: &str,
    size: u64,
    mut src: R,
    mut progress: impl FnMut(u64),
) -> Result<crate::files::Attachment> {
    use crate::files::{Transfer, TransferReply};
    use tokio::io::AsyncReadExt;
    let (mut send, mut recv) = conn.open_bi().await?;
    crate::send(&mut send, &Transfer::Upload { name: name.into(), size }).await?;
    match crate::recv::<TransferReply>(&mut recv).await? {
        Some(TransferReply::Go) => {}
        Some(TransferReply::Error(e)) => return Err(anyhow!(e)),
        other => return Err(anyhow!("unexpected reply: {other:?}")),
    }
    let mut buf = vec![0u8; 64 * 1024];
    let mut sent = 0u64;
    loop {
        let n = src.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        send.write_all(&buf[..n]).await?;
        sent += n as u64;
        progress(sent);
    }
    send.finish()?;
    match crate::recv::<TransferReply>(&mut recv).await? {
        Some(TransferReply::Stored(a)) => Ok(a),
        Some(TransferReply::Error(e)) => Err(anyhow!(e)),
        other => Err(anyhow!("unexpected reply: {other:?}")),
    }
}

/// Download a file (or its preview, `thumb`) into `dst`. `progress` gets (received, total).
/// Returns the size. Dropping the future cancels the download.
pub async fn download<W: tokio::io::AsyncWrite + Unpin>(
    conn: &quinn::Connection,
    file: crate::Id,
    thumb: bool,
    mut dst: W,
    mut progress: impl FnMut(u64, u64),
) -> Result<u64> {
    use crate::files::{Transfer, TransferReply};
    use tokio::io::AsyncWriteExt;
    let (mut send, mut recv) = conn.open_bi().await?;
    crate::send(&mut send, &Transfer::Download { file, thumb }).await?;
    send.finish()?;
    let size = match crate::recv::<TransferReply>(&mut recv).await? {
        Some(TransferReply::Size(n)) => n,
        Some(TransferReply::Error(e)) => return Err(anyhow!(e)),
        other => return Err(anyhow!("unexpected reply: {other:?}")),
    };
    let mut got = 0u64;
    while let Some(chunk) = recv.read_chunk(64 * 1024, true).await? {
        dst.write_all(&chunk.bytes).await?;
        got += chunk.bytes.len() as u64;
        progress(got, size);
    }
    dst.flush().await?;
    anyhow::ensure!(got == size, "the download stopped early");
    Ok(size)
}
