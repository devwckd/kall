//! The bits of the Cloudflare API kall needs: find the zone, and create/update/delete DNS
//! records (the ACME DNS-01 TXT record, and A/AAAA for DDNS).
//!
//! The token needs `Zone → DNS → Edit` and `Zone → Zone → Read` on the domain's zone.

use std::net::IpAddr;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::json;

pub const API: &str = "https://api.cloudflare.com/client/v4";

#[derive(Clone)]
pub struct Cloudflare {
    http: reqwest::Client,
    token: String,
    base: String,
}

#[derive(Deserialize)]
struct Envelope<T> {
    success: bool,
    #[serde(default)]
    errors: Vec<ApiError>,
    result: Option<T>,
}

#[derive(Deserialize)]
struct ApiError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
struct Zone {
    id: String,
    name: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Record {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub content: String,
}

impl Cloudflare {
    pub fn new(token: impl Into<String>) -> Cloudflare {
        Cloudflare::with_base(token, API)
    }

    pub fn with_base(token: impl Into<String>, base: impl Into<String>) -> Cloudflare {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().expect("http client");
        Cloudflare { http, token: token.into(), base: base.into().trim_end_matches('/').to_string() }
    }

    async fn call<T: for<'de> Deserialize<'de>>(&self, req: reqwest::RequestBuilder) -> Result<T> {
        let resp = req.bearer_auth(&self.token).send().await.context("calling the Cloudflare API")?;
        let status = resp.status();
        let env: Envelope<T> = resp.json().await.with_context(|| format!("Cloudflare answered {status} with something unreadable"))?;
        if !env.success {
            let why = env.errors.iter().map(|e| format!("{} ({})", e.message, e.code)).collect::<Vec<_>>().join("; ");
            bail!("Cloudflare API error: {why}");
        }
        env.result.ok_or_else(|| anyhow!("Cloudflare API returned no result"))
    }

    /// The zone holding `domain`: tries `a.b.example.com`, then `b.example.com`, `example.com`.
    pub async fn zone_for(&self, domain: &str) -> Result<String> {
        let labels: Vec<&str> = domain.trim_end_matches('.').split('.').collect();
        for i in 0..labels.len().saturating_sub(1) {
            let name = labels[i..].join(".");
            let zones: Vec<Zone> = self.call(self.http.get(format!("{}/zones", self.base)).query(&[("name", name.as_str())])).await?;
            if let Some(z) = zones.into_iter().find(|z| z.name.eq_ignore_ascii_case(&name)) {
                return Ok(z.id);
            }
        }
        bail!("no Cloudflare zone found for {domain} (does the token have Zone:Read on it?)")
    }

    pub async fn records(&self, zone: &str, kind: &str, name: &str) -> Result<Vec<Record>> {
        self.call(self.http.get(format!("{}/zones/{zone}/dns_records", self.base)).query(&[("type", kind), ("name", name)])).await
    }

    pub async fn create(&self, zone: &str, kind: &str, name: &str, content: &str) -> Result<Record> {
        let body = json!({ "type": kind, "name": name, "content": content, "ttl": 60, "proxied": false });
        self.call(self.http.post(format!("{}/zones/{zone}/dns_records", self.base)).json(&body)).await
    }

    pub async fn update(&self, zone: &str, id: &str, kind: &str, name: &str, content: &str) -> Result<Record> {
        let body = json!({ "type": kind, "name": name, "content": content, "ttl": 60, "proxied": false });
        self.call(self.http.put(format!("{}/zones/{zone}/dns_records/{id}", self.base)).json(&body)).await
    }

    pub async fn delete(&self, zone: &str, id: &str) -> Result<()> {
        let _: serde_json::Value = self.call(self.http.delete(format!("{}/zones/{zone}/dns_records/{id}", self.base))).await?;
        Ok(())
    }

    /// Point `name`'s A (IPv4) or AAAA (IPv6) record at `ip`, DNS-only (grey cloud: Cloudflare
    /// can't proxy QUIC to an arbitrary port). Returns true when something changed.
    pub async fn upsert_address(&self, zone: &str, name: &str, ip: IpAddr) -> Result<bool> {
        let kind = if ip.is_ipv4() { "A" } else { "AAAA" };
        let ip = ip.to_string();
        let existing = self.records(zone, kind, name).await?;
        match existing.first() {
            Some(r) if r.content == ip && existing.len() == 1 => Ok(false),
            Some(r) => {
                self.update(zone, &r.id, kind, name, &ip).await?;
                for extra in &existing[1..] {
                    self.delete(zone, &extra.id).await?;
                }
                Ok(true)
            }
            None => {
                self.create(zone, kind, name, &ip).await?;
                Ok(true)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_partial_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ok(result: serde_json::Value) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(json!({ "success": true, "errors": [], "result": result }))
    }

    #[tokio::test]
    async fn finds_the_zone_from_a_subdomain() {
        let s = MockServer::start().await;
        Mock::given(method("GET")).and(path("/zones")).and(query_param("name", "kall.example.com")).respond_with(ok(json!([]))).mount(&s).await;
        Mock::given(method("GET"))
            .and(path("/zones"))
            .and(query_param("name", "example.com"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ok(json!([{ "id": "z1", "name": "example.com" }])))
            .mount(&s)
            .await;
        let cf = Cloudflare::with_base("tok", s.uri());
        assert_eq!(cf.zone_for("kall.example.com").await.unwrap(), "z1");
    }

    #[tokio::test]
    async fn reports_api_errors() {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/zones"))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({ "success": false, "errors": [{ "code": 9109, "message": "Invalid access token" }], "result": null })))
            .mount(&s)
            .await;
        let err = Cloudflare::with_base("bad", s.uri()).zone_for("kall.example.com").await.unwrap_err();
        assert!(err.to_string().contains("Invalid access token"), "{err}");
    }

    #[tokio::test]
    async fn upserts_address_records() {
        let s = MockServer::start().await;
        // No AAAA yet: created. A exists with an old IP: updated. A already right: untouched.
        Mock::given(method("GET")).and(path("/zones/z1/dns_records")).and(query_param("type", "AAAA")).respond_with(ok(json!([]))).mount(&s).await;
        Mock::given(method("POST"))
            .and(path("/zones/z1/dns_records"))
            .and(body_partial_json(json!({ "type": "AAAA", "content": "2001:db8::1", "proxied": false })))
            .respond_with(ok(json!({ "id": "r2", "name": "kall.example.com", "type": "AAAA", "content": "2001:db8::1" })))
            .expect(1)
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(path("/zones/z1/dns_records"))
            .and(query_param("type", "A"))
            .respond_with(ok(json!([{ "id": "r1", "name": "kall.example.com", "type": "A", "content": "203.0.113.5" }])))
            .mount(&s)
            .await;
        Mock::given(method("PUT"))
            .and(path("/zones/z1/dns_records/r1"))
            .and(body_partial_json(json!({ "content": "203.0.113.9" })))
            .respond_with(ok(json!({ "id": "r1", "name": "kall.example.com", "type": "A", "content": "203.0.113.9" })))
            .expect(1)
            .mount(&s)
            .await;
        let cf = Cloudflare::with_base("tok", s.uri());
        assert!(cf.upsert_address("z1", "kall.example.com", "2001:db8::1".parse().unwrap()).await.unwrap());
        assert!(cf.upsert_address("z1", "kall.example.com", "203.0.113.9".parse().unwrap()).await.unwrap());
        assert!(!cf.upsert_address("z1", "kall.example.com", "203.0.113.5".parse().unwrap()).await.unwrap());
    }
}
