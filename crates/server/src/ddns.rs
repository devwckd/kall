//! Keep `kall.<domain>` pointing at the home connection's public IP.
//!
//! Every few minutes, ask Cloudflare's trace endpoint what our public IPv4 (and optionally
//! IPv6) is, and update the A/AAAA record when it changed.

use std::net::IpAddr;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};

use crate::cloudflare::Cloudflare;

pub const TRACE_V4: &str = "https://1.1.1.1/cdn-cgi/trace";
pub const TRACE_V6: &str = "https://[2606:4700:4700::1111]/cdn-cgi/trace";

#[derive(Clone)]
pub struct Ddns {
    pub domain: String,
    pub cf: Cloudflare,
    pub ipv6: bool,
}

/// `ip=…` from a cdn-cgi/trace body.
pub fn parse_trace(body: &str) -> Option<IpAddr> {
    body.lines().find_map(|l| l.strip_prefix("ip=")).and_then(|ip| ip.trim().parse().ok())
}

async fn public_ip(http: &reqwest::Client, url: &str) -> Result<IpAddr> {
    let body = http.get(url).send().await?.text().await?;
    parse_trace(&body).ok_or_else(|| anyhow!("no ip= in {url}"))
}

impl Ddns {
    /// One pass: returns the addresses now published.
    pub async fn update(&self, http: &reqwest::Client) -> Result<Vec<IpAddr>> {
        let zone = self.cf.zone_for(&self.domain).await?;
        let mut out = Vec::new();
        let v4 = public_ip(http, TRACE_V4).await.context("finding the public IPv4 address")?;
        if self.cf.upsert_address(&zone, &self.domain, v4).await? {
            tracing::info!("DNS: {} → {v4}", self.domain);
        }
        out.push(v4);
        if self.ipv6 {
            match public_ip(http, TRACE_V6).await {
                Ok(v6) => {
                    if self.cf.upsert_address(&zone, &self.domain, v6).await? {
                        tracing::info!("DNS: {} → {v6}", self.domain);
                    }
                    out.push(v6);
                }
                Err(e) => tracing::warn!("no public IPv6: {e:#}"),
            }
        }
        Ok(out)
    }

    pub async fn run_forever(self) {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(15)).build().expect("http client");
        let mut last: Vec<IpAddr> = Vec::new();
        loop {
            match self.update(&http).await {
                Ok(ips) => {
                    if ips != last {
                        tracing::info!("DDNS: {} is at {}", self.domain, ips.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", "));
                        last = ips;
                    }
                }
                Err(e) => tracing::warn!("DDNS update failed: {e:#}"),
            }
            tokio::time::sleep(Duration::from_secs(300)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_the_trace() {
        let body = "fl=123\nh=1.1.1.1\nip=203.0.113.7\nts=1\n";
        assert_eq!(super::parse_trace(body), Some("203.0.113.7".parse().unwrap()));
        assert_eq!(super::parse_trace("ip=2001:db8::5\n"), Some("2001:db8::5".parse().unwrap()));
        assert_eq!(super::parse_trace("nothing"), None);
    }
}
