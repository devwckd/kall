//! kall-server as a library, so tests can run a real server in-process.

pub mod acme;
pub mod cloudflare;
pub mod db;
pub mod ddns;
pub mod files;
pub mod hub;
pub mod preview;
pub mod tls;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;

use crate::db::Db;
use crate::hub::Hub;
use crate::tls::Identity;

pub struct Server {
    pub endpoint: quinn::Endpoint,
    pub hub: Arc<Hub>,
    /// The owner invite, while nobody has registered yet.
    pub owner_invite: Option<String>,
    pub fingerprint: String,
}

impl Server {
    pub fn start(db: Db, name: String, bind: SocketAddr, identity: Identity, files: files::FileStore) -> Result<Server> {
        rustls::crypto::ring::default_provider().install_default().ok();
        let owner_invite = if db.user_count()? == 0 {
            let code = match db.has_owner_invite()? {
                Some(code) => code,
                None => db.create_invite(&hub::random_code(10), None, Some(1), None, true)?.code,
            };
            tracing::info!("no accounts yet: register the owner account with invite code  {code}");
            Some(code)
        } else {
            None
        };
        let fingerprint = identity.fingerprint();
        let endpoint = quinn::Endpoint::server(tls::server_config(identity)?, bind)?;
        tracing::info!("{name} listening on udp {}", endpoint.local_addr()?);
        let hub = Arc::new(Hub::new(db, name, files));
        Ok(Server { endpoint, hub, owner_invite, fingerprint })
    }

    pub async fn run(&self) {
        self.hub.start_file_janitor();
        while let Some(incoming) = self.endpoint.accept().await {
            let hub = self.hub.clone();
            tokio::spawn(async move {
                if let Err(e) = hub::serve(hub, incoming).await {
                    tracing::debug!("connection: {e:#}");
                }
            });
        }
    }
}
