//! Link previews: fetch the first URL in a message and read its OpenGraph / <title> tags.
//!
//! The server sits on a home network, so it refuses to fetch anything that resolves to a
//! private, loopback or link-local address (a friend's link must not probe the LAN).

use std::net::IpAddr;
use std::time::Duration;

use anyhow::{Result, bail};
use kall_proto::Preview;

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .redirect(reqwest::redirect::Policy::limited(4))
        .user_agent("Mozilla/5.0 (compatible; kall-preview/0.1)")
        .build()
        .expect("http client")
}

/// The first http(s) URL in `body`, skipping ones inside `inline code`.
pub fn first_url(body: &str) -> Option<String> {
    let mut in_code = false;
    let mut i = 0;
    let bytes = body.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'`' {
            in_code = !in_code;
        } else if !in_code && (body[i..].starts_with("https://") || body[i..].starts_with("http://")) {
            let end = body[i..].find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`')).map_or(body.len(), |e| i + e);
            let url = body[i..end].trim_end_matches(['.', ',', ')', '!', '?']);
            return Some(url.to_string());
        }
        i += body[i..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

fn forbidden(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() || v4.is_broadcast() || v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64,
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80 || v6.to_ipv4_mapped().is_some_and(|v4| forbidden(IpAddr::V4(v4)))
        }
    }
}

pub async fn fetch(http: &reqwest::Client, url: &str) -> Result<Option<Preview>> {
    let parsed = reqwest::Url::parse(url)?;
    let host = parsed.host_str().unwrap_or_default().to_string();
    let port = parsed.port_or_known_default().unwrap_or(443);
    let addrs: Vec<_> = tokio::net::lookup_host((host.as_str(), port)).await?.collect();
    if addrs.is_empty() || addrs.iter().any(|a| forbidden(a.ip())) {
        bail!("refusing to preview {host}: private address");
    }
    let resp = http.get(parsed.clone()).send().await?;
    if resp.url().host_str() != Some(host.as_str()) {
        // A redirect to another host: check it too.
        let h = resp.url().host_str().unwrap_or_default().to_string();
        let addrs: Vec<_> = tokio::net::lookup_host((h.as_str(), 443)).await?.collect();
        if addrs.iter().any(|a| forbidden(a.ip())) {
            bail!("refusing redirect to private address");
        }
    }
    let html = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("html"));
    if !resp.status().is_success() || !html {
        return Ok(None);
    }
    // Only the head matters; cap what we read.
    let mut body = Vec::new();
    let mut resp = resp;
    while let Some(chunk) = resp.chunk().await? {
        body.extend_from_slice(&chunk);
        if body.len() > 256 * 1024 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&body);
    Ok(parse(url, &host, &text))
}

/// Pull og:* / twitter:* / <title> out of an HTML head.
pub fn parse(url: &str, host: &str, html: &str) -> Option<Preview> {
    let mut title = None;
    let mut desc = None;
    let mut site = None;
    let lower = html.to_ascii_lowercase();
    let mut at = 0;
    while let Some(i) = lower[at..].find("<meta") {
        let start = at + i;
        let end = lower[start..].find('>').map_or(lower.len(), |e| start + e);
        let tag = &html[start..end];
        let key = attr(tag, "property").or_else(|| attr(tag, "name")).map(|k| k.to_ascii_lowercase());
        if let (Some(key), Some(content)) = (key, attr(tag, "content").filter(|c| !c.trim().is_empty())) {
            match key.as_str() {
                "og:title" | "twitter:title" => title = title.or(Some(content)),
                "og:description" | "twitter:description" | "description" => desc = desc.or(Some(content)),
                "og:site_name" => site = site.or(Some(content)),
                _ => {}
            }
        }
        at = end;
    }
    if title.is_none()
        && let Some(i) = lower.find("<title")
        && let Some(gt) = lower[i..].find('>')
        && let Some(close) = lower[i + gt..].find("</title")
    {
        title = Some(html[i + gt + 1..i + gt + close].trim().to_string());
    }
    let title = decode(&title?);
    if title.is_empty() {
        return None;
    }
    let clip = |s: String, n: usize| if s.chars().count() > n { s.chars().take(n).collect::<String>() + "…" } else { s };
    Some(Preview {
        url: url.into(),
        site: clip(decode(&site.unwrap_or_else(|| host.trim_start_matches("www.").to_string())), 60),
        title: clip(title, 140),
        description: clip(decode(&desc.unwrap_or_default()), 300),
    })
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let i = from + i;
        let before_ok = i == 0 || lower.as_bytes()[i - 1].is_ascii_whitespace();
        let rest = lower[i + name.len()..].trim_start();
        if before_ok && rest.starts_with('=') {
            let vstart = tag.len() - rest.len() + 1;
            let v = tag[vstart..].trim_start();
            let (q, body) = match v.chars().next()? {
                c @ ('"' | '\'') => (Some(c), &v[1..]),
                _ => (None, v),
            };
            let end = match q {
                Some(q) => body.find(q)?,
                None => body.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(body.len()),
            };
            return Some(body[..end].to_string());
        }
        from = i + name.len();
    }
    None
}

fn decode(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_urls() {
        assert_eq!(first_url("see https://example.com/a, ok").as_deref(), Some("https://example.com/a"));
        assert_eq!(first_url("`https://no.code` then http://yes.io").as_deref(), Some("http://yes.io"));
        assert_eq!(first_url("nothing here"), None);
    }

    #[test]
    fn parses_og_tags() {
        let html = r#"<html><head><title>Fallback</title>
            <meta property="og:title" content="Rune &amp; friends">
            <meta name="description" content='A GPU UI   framework'>
            <meta property="og:site_name" content="GitHub"/></head>"#;
        let p = parse("https://github.com/x", "github.com", html).unwrap();
        assert_eq!(p.title, "Rune & friends");
        assert_eq!(p.description, "A GPU UI framework");
        assert_eq!(p.site, "GitHub");
        let p = parse("https://a.b", "www.a.b", "<meta property=\"og:title\" content=\"\"><title> Plain </title>").unwrap();
        assert_eq!((p.title.as_str(), p.site.as_str()), ("Plain", "a.b"));
    }

    #[test]
    fn blocks_private_addresses() {
        assert!(forbidden("192.168.1.10".parse().unwrap()));
        assert!(forbidden("127.0.0.1".parse().unwrap()));
        assert!(forbidden("::1".parse().unwrap()));
        assert!(forbidden("100.64.0.1".parse().unwrap()));
        assert!(!forbidden("1.1.1.1".parse().unwrap()));
    }
}

#[cfg(test)]
mod live {
    #[tokio::test]
    #[ignore = "hits the network"]
    async fn fetches_a_real_page() {
        let http = super::client();
        let p = super::fetch(&http, "https://www.rust-lang.org").await;
        println!("{p:?}");
        assert!(p.unwrap().is_some());
    }
}
