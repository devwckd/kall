//! Let's Encrypt certificates over ACME DNS-01, with Cloudflare holding the challenge record.
//!
//! The certificate and key live in `<data>/acme/<domain>.{crt,key}.pem`, the ACME account in
//! `<data>/acme/account-<env>.json`. [`renew_forever`] checks twice a day and renews 30 days
//! before expiry, swapping the new certificate into the running QUIC endpoint.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use instant_acme::{Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, LetsEncrypt, NewAccount, NewOrder, OrderStatus, RetryPolicy};

use crate::cloudflare::Cloudflare;
use crate::tls::{self, Identity};

/// Renew when fewer than this many days are left.
pub const RENEW_DAYS: i64 = 30;

/// Where the DNS-01 TXT record goes.
#[derive(Clone)]
pub enum Dns {
    Cloudflare(Cloudflare),
    /// Pebble's `challtestsrv` management API (tests only).
    ChallTest(String),
}

impl Dns {
    /// Publish the record; returns handles for [`Dns::clear`].
    async fn set_txt(&self, name: &str, value: &str) -> Result<Vec<String>> {
        match self {
            Dns::Cloudflare(cf) => {
                let zone = cf.zone_for(name.trim_start_matches("_acme-challenge.")).await?;
                // Stale records from an earlier failed run would confuse the check.
                for old in cf.records(&zone, "TXT", name).await? {
                    cf.delete(&zone, &old.id).await.ok();
                }
                let rec = cf.create(&zone, "TXT", name, value).await?;
                wait_for_txt(name, value).await;
                Ok(vec![zone, rec.id])
            }
            Dns::ChallTest(url) => {
                reqwest::Client::new()
                    .post(format!("{url}/set-txt"))
                    .json(&serde_json::json!({ "host": format!("{name}."), "value": value }))
                    .send()
                    .await?
                    .error_for_status()?;
                Ok(vec![name.to_string()])
            }
        }
    }

    async fn clear(&self, handle: &[String]) {
        let r = match (self, handle) {
            (Dns::Cloudflare(cf), [zone, id]) => cf.delete(zone, id).await,
            (Dns::ChallTest(url), [name]) => reqwest::Client::new()
                .post(format!("{url}/clear-txt"))
                .json(&serde_json::json!({ "host": format!("{name}.") }))
                .send()
                .await
                .map(|_| ())
                .map_err(Into::into),
            _ => Ok(()),
        };
        if let Err(e) = r {
            tracing::warn!("couldn't remove the ACME TXT record: {e:#}");
        }
    }
}

#[derive(Clone)]
pub struct Acme {
    pub domain: String,
    pub email: Option<String>,
    pub staging: bool,
    pub dir: PathBuf,
    pub dns: Dns,
    /// Another ACME directory instead of Let's Encrypt, and the PEM root to trust for it
    /// (tests run against Pebble).
    pub directory: Option<(String, PathBuf)>,
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl Acme {
    fn env(&self) -> &'static str {
        match (&self.directory, self.staging) {
            (Some(_), _) => "custom",
            (None, true) => "staging",
            (None, false) => "production",
        }
    }

    fn builder(&self) -> Result<instant_acme::AccountBuilder> {
        Ok(match &self.directory {
            Some((_, root)) => Account::builder_with_root(root)?,
            None => Account::builder()?,
        })
    }

    fn paths(&self) -> (PathBuf, PathBuf) {
        let base = format!("{}{}", self.domain, if self.staging { ".staging" } else { "" });
        (self.dir.join(format!("{base}.crt.pem")), self.dir.join(format!("{base}.key.pem")))
    }

    /// The saved certificate, if there is one for this domain.
    pub fn load(&self) -> Option<Identity> {
        let (c, k) = self.paths();
        let id = Identity::from_pem(&std::fs::read_to_string(c).ok()?, &std::fs::read_to_string(k).ok()?).ok()?;
        id.names().iter().any(|n| n.eq_ignore_ascii_case(&self.domain)).then_some(id)
    }

    /// Days the saved certificate has left (negative when expired).
    pub fn days_left(id: &Identity) -> i64 {
        id.not_after().map_or(-1, |t| (t - now()) / 86_400)
    }

    /// A certificate good for at least [`RENEW_DAYS`]: the saved one, or a fresh one.
    pub async fn ensure(&self) -> Result<Identity> {
        if let Some(id) = self.load() {
            let left = Self::days_left(&id);
            if left > RENEW_DAYS {
                tracing::info!("certificate for {} valid for {left} more days", self.domain);
                return Ok(id);
            }
            tracing::info!("certificate for {} has {left} days left: renewing", self.domain);
        }
        self.issue().await
    }

    async fn account(&self) -> Result<Account> {
        let path = self.dir.join(format!("account-{}.json", self.env()));
        if let Ok(json) = std::fs::read_to_string(&path) {
            let creds: AccountCredentials = serde_json::from_str(&json).context("reading ACME account")?;
            return Ok(self.builder()?.from_credentials(creds).await?);
        }
        let url = match &self.directory {
            Some((url, _)) => url.as_str(),
            None if self.staging => LetsEncrypt::Staging.url(),
            None => LetsEncrypt::Production.url(),
        };
        let contact: Vec<String> = self.email.iter().map(|e| format!("mailto:{e}")).collect();
        let contact: Vec<&str> = contact.iter().map(String::as_str).collect();
        let (account, creds) = self.builder()?
            .create(&NewAccount { contact: &contact, terms_of_service_agreed: true, only_return_existing: false }, url.to_owned(), None)
            .await
            .context("creating the Let's Encrypt account")?;
        std::fs::write(&path, serde_json::to_string_pretty(&creds)?)?;
        tracing::info!("created Let's Encrypt {} account", self.env());
        Ok(account)
    }

    /// Run a DNS-01 order end to end and save the result.
    pub async fn issue(&self) -> Result<Identity> {
        std::fs::create_dir_all(&self.dir)?;
        tracing::info!("requesting a {} certificate for {}", self.env(), self.domain);
        let account = self.account().await?;
        let mut order = account.new_order(&NewOrder::new(&[Identifier::Dns(self.domain.clone())])).await?;

        let record_name = format!("_acme-challenge.{}", self.domain);
        let mut created = Vec::new();
        let result = async {
            let mut authorizations = order.authorizations();
            while let Some(authz) = authorizations.next().await {
                let mut authz = authz?;
                match authz.status {
                    AuthorizationStatus::Pending => {}
                    AuthorizationStatus::Valid => continue,
                    other => bail!("authorization is {other:?}"),
                }
                let mut challenge = authz.challenge(ChallengeType::Dns01).ok_or_else(|| anyhow!("the CA offered no DNS-01 challenge"))?;
                let value = challenge.key_authorization().dns_value();
                created.push(self.dns.set_txt(&record_name, &value).await?);
                challenge.set_ready().await?;
            }
            let status = order.poll_ready(&RetryPolicy::new().timeout(Duration::from_secs(180))).await?;
            if status != OrderStatus::Ready {
                bail!("order ended as {status:?}");
            }
            let key_pem = order.finalize().await?;
            let chain_pem = order.poll_certificate(&RetryPolicy::new().timeout(Duration::from_secs(120))).await?;
            Ok((chain_pem, key_pem))
        }
        .await;
        for handle in created {
            self.dns.clear(&handle).await;
        }
        let (chain_pem, key_pem) = result.with_context(|| format!("getting a certificate for {}", self.domain))?;
        let (c, k) = self.paths();
        std::fs::write(&k, &key_pem)?;
        std::fs::write(&c, &chain_pem)?;
        let id = Identity::from_pem(&chain_pem, &key_pem)?;
        tracing::info!("got a certificate for {} valid for {} days", self.domain, Self::days_left(&id));
        Ok(id)
    }
}

/// Wait (up to two minutes) until a public resolver sees the TXT record, plus a little margin.
async fn wait_for_txt(name: &str, value: &str) {
    let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().expect("http client");
    for _ in 0..24 {
        let seen = async {
            let r = http
                .get("https://cloudflare-dns.com/dns-query")
                .query(&[("name", name), ("type", "TXT")])
                .header("accept", "application/dns-json")
                .send()
                .await
                .ok()?;
            let v: serde_json::Value = r.json().await.ok()?;
            Some(v["Answer"].as_array()?.iter().any(|a| a["data"].as_str().is_some_and(|d| d.trim_matches('"') == value)))
        }
        .await
        .unwrap_or(false);
        if seen {
            tokio::time::sleep(Duration::from_secs(5)).await;
            return;
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    tracing::warn!("TXT record not visible yet; asking Let's Encrypt to check anyway");
}

/// Check twice a day; renew near expiry and hot-swap the endpoint's certificate. While the
/// server is still on its fallback self-signed certificate, retry every hour.
pub async fn renew_forever(acme: Acme, endpoint: quinn::Endpoint, mut have_real: bool) {
    loop {
        tokio::time::sleep(Duration::from_secs(if have_real { 12 * 3600 } else { 3600 })).await;
        let due = acme.load().is_none_or(|id| Acme::days_left(&id) <= RENEW_DAYS);
        if !due && have_real {
            continue;
        }
        match acme.ensure().await.and_then(tls::server_config) {
            Ok(cfg) => {
                endpoint.set_server_config(Some(cfg));
                have_real = true;
                tracing::info!("now serving the renewed certificate");
            }
            Err(e) => tracing::error!("certificate renewal failed: {e:#}"),
        }
    }
}
