//! The ACME DNS-01 flow against Pebble (Let's Encrypt's test CA), and swapping the new
//! certificate into a running server. Needs Docker:
//!
//! ```text
//! docker compose -f deploy/pebble.compose.yaml up -d
//! cargo test -p kall-server --test acme_pebble -- --ignored
//! ```

use std::path::PathBuf;

use anyhow::Result;
use kall_server::acme::{Acme, Dns};
use kall_server::{Server, db::Db, tls};

fn pebble(dir: PathBuf) -> Acme {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/pebble.minica.pem");
    Acme {
        domain: "kall.test".into(),
        email: None,
        staging: false,
        dir,
        dns: Dns::ChallTest("http://localhost:8055".into()),
        directory: Some(("https://localhost:14000/dir".into(), root)),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs Pebble: docker compose -f deploy/pebble.compose.yaml up -d"]
async fn issues_a_certificate_and_hot_swaps_it() -> Result<()> {
    rustls::crypto::ring::default_provider().install_default().ok();
    let dir = tempfile::tempdir()?;
    let acme = pebble(dir.path().join("acme"));

    let issued = acme.ensure().await?;
    assert_eq!(issued.names(), vec!["kall.test"]);
    assert!(Acme::days_left(&issued) > kall_server::acme::RENEW_DAYS);
    assert!(issued.chain.len() >= 2, "leaf plus intermediate");

    // A second call reuses the saved certificate instead of ordering another.
    let again = acme.ensure().await?;
    assert_eq!(again.fingerprint(), issued.fingerprint());

    // A server running on its self-signed certificate picks the new one up without restarting.
    let selfsigned = tls::self_signed(dir.path(), &[])?;
    let server = Server::start(Db::memory()?, "T".into(), "127.0.0.1:0".parse()?, selfsigned, kall_server::files::FileStore::defaults(std::env::temp_dir().join("kall-acme-test-files")))?;
    let addr = server.endpoint.local_addr()?.to_string();
    let endpoint = server.endpoint.clone();
    tokio::spawn(async move { server.run().await });
    let before = kall_proto::client::connect(&addr, None).await?.fingerprint;
    endpoint.set_server_config(Some(tls::server_config(issued.clone_key())?));
    let after = kall_proto::client::connect(&addr, None).await?.fingerprint;
    assert_ne!(before, after);
    assert_eq!(after, issued.fingerprint());
    Ok(())
}
