//! The QUIC endpoint's certificate: a Let's Encrypt chain from [`crate::acme`] when a domain
//! and Cloudflare token are set, otherwise a self-signed certificate made once and kept in
//! the data dir (clients pin its fingerprint).

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

pub struct Identity {
    /// Leaf first, then intermediates.
    pub chain: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
}

impl Identity {
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.chain[0])
    }

    /// Unix seconds when the leaf certificate expires.
    pub fn not_after(&self) -> Result<i64> {
        let (_, cert) = x509_parser::parse_x509_certificate(&self.chain[0]).map_err(|e| anyhow!("bad certificate: {e}"))?;
        Ok(cert.validity().not_after.timestamp())
    }

    /// DNS names the leaf is valid for.
    pub fn names(&self) -> Vec<String> {
        let Ok((_, cert)) = x509_parser::parse_x509_certificate(&self.chain[0]) else { return vec![] };
        let Ok(Some(san)) = cert.subject_alternative_name() else { return vec![] };
        san.value
            .general_names
            .iter()
            .filter_map(|n| match n {
                x509_parser::extensions::GeneralName::DNSName(d) => Some(d.to_string()),
                _ => None,
            })
            .collect()
    }

    pub fn from_pem(chain_pem: &str, key_pem: &str) -> Result<Identity> {
        let chain = CertificateDer::pem_slice_iter(chain_pem.as_bytes()).collect::<Result<Vec<_>, _>>().context("reading certificate PEM")?;
        anyhow::ensure!(!chain.is_empty(), "no certificate in PEM");
        let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).context("reading key PEM")?;
        Ok(Identity { chain, key })
    }

    pub fn clone_key(&self) -> Identity {
        Identity { chain: self.chain.clone(), key: self.key.clone_key() }
    }
}

pub fn fingerprint(cert: &[u8]) -> String {
    let d = Sha256::digest(cert);
    d.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

/// Load `cert.der`/`key.der` from `dir`, or make and save them.
pub fn self_signed(dir: &Path, names: &[String]) -> Result<Identity> {
    let (cp, kp) = (dir.join("cert.der"), dir.join("key.der"));
    if cp.exists() && kp.exists() {
        let cert = std::fs::read(&cp).context("reading cert.der")?;
        let key = std::fs::read(&kp).context("reading key.der")?;
        return Ok(Identity { chain: vec![cert.into()], key: PrivatePkcs8KeyDer::from(key).into() });
    }
    let mut sans = vec!["localhost".to_string()];
    sans.extend(names.iter().cloned());
    let ck = rcgen::generate_simple_self_signed(sans)?;
    let cert = ck.cert.der().to_vec();
    let key = ck.signing_key.serialize_der();
    std::fs::write(&cp, &cert)?;
    std::fs::write(&kp, &key)?;
    Ok(Identity { chain: vec![cert.into()], key: PrivatePkcs8KeyDer::from(key).into() })
}

pub fn server_config(id: Identity) -> Result<quinn::ServerConfig> {
    let mut tls = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(id.chain, id.key)?;
    tls.alpn_protocols = vec![kall_proto::ALPN.to_vec()];
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(crypto));
    let mut transport = quinn::TransportConfig::default();
    transport.max_idle_timeout(Some(std::time::Duration::from_secs(30).try_into()?));
    transport.keep_alive_interval(Some(std::time::Duration::from_secs(10)));
    cfg.transport_config(Arc::new(transport));
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pem_round_trip_and_expiry() {
        let ck = rcgen::generate_simple_self_signed(vec!["kall.example.com".into()]).unwrap();
        let id = Identity::from_pem(&ck.cert.pem(), &ck.signing_key.serialize_pem()).unwrap();
        assert_eq!(id.names(), vec!["kall.example.com"]);
        assert!(id.not_after().unwrap() > 0);
        assert_eq!(id.fingerprint().len(), 95);
    }
}
