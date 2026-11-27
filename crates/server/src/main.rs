//! kall-server: one kall server for one group of friends.
//!
//! ```text
//! kall-server [serve]   run the server (default)
//! kall-server invite    print a new invite code   [--uses N] [--hours H]
//! kall-server cert      get/renew the certificate once and exit
//! kall-server ddns      update the DNS record once and exit
//! ```
//!
//! Settings come from flags or the environment (flags win):
//!
//! | flag | env | meaning |
//! | --- | --- | --- |
//! | `--data DIR` | `KALL_DATA` | where the database and certificates live (`data`) |
//! | `--bind ADDR` | `KALL_BIND` | UDP address to listen on (`[::]:7744`) |
//! | `--name NAME` | `KALL_NAME` | the server's display name (`kall`) |
//! | `--domain D` | `KALL_DOMAIN` | public name, e.g. `kall.example.com` |
//! | | `CF_API_TOKEN` | Cloudflare token (DNS:Edit + Zone:Read): enables Let's Encrypt + DDNS |
//! | `--email E` | `KALL_ACME_EMAIL` | contact for Let's Encrypt expiry mail (optional) |
//! | `--staging` | `KALL_ACME_STAGING=1` | use Let's Encrypt's staging CA (for trying things out) |
//! | `--no-ddns` | `KALL_DDNS=0` | don't touch the A/AAAA record |
//! | `--ipv6` | `KALL_DDNS_IPV6=1` | also publish an AAAA record |

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use kall_server::acme::Acme;
use kall_server::cloudflare::Cloudflare;
use kall_server::db::{self, Db};
use kall_server::ddns::Ddns;
use kall_server::{hub, tls};

struct Args {
    cmd: String,
    data: PathBuf,
    bind: SocketAddr,
    name: String,
    domain: Option<String>,
    cf_token: Option<String>,
    email: Option<String>,
    staging: bool,
    ddns: bool,
    ipv6: bool,
    uses: Option<u32>,
    hours: Option<u32>,
}

fn truthy(v: Option<String>) -> Option<bool> {
    v.map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
}

fn args() -> Result<Args> {
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let mut a = Args {
        cmd: "serve".into(),
        data: env("KALL_DATA").unwrap_or_else(|| "data".into()).into(),
        bind: env("KALL_BIND").unwrap_or_else(|| format!("[::]:{}", kall_proto::DEFAULT_PORT)).parse().context("bad KALL_BIND")?,
        name: env("KALL_NAME").unwrap_or_else(|| "kall".into()),
        domain: env("KALL_DOMAIN"),
        cf_token: env("CF_API_TOKEN"),
        email: env("KALL_ACME_EMAIL"),
        staging: truthy(env("KALL_ACME_STAGING")).unwrap_or(false),
        ddns: truthy(env("KALL_DDNS")).unwrap_or(true),
        ipv6: truthy(env("KALL_DDNS_IPV6")).unwrap_or(false),
        uses: Some(1),
        hours: Some(24 * 7),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = || it.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "serve" | "invite" | "cert" | "ddns" => a.cmd = arg.clone(),
            "--data" => a.data = val()?.into(),
            "--bind" => a.bind = val()?.parse().context("bad --bind")?,
            "--name" => a.name = val()?,
            "--domain" => a.domain = Some(val()?),
            "--email" => a.email = Some(val()?),
            "--staging" => a.staging = true,
            "--no-ddns" => a.ddns = false,
            "--ipv6" => a.ipv6 = true,
            "--uses" => a.uses = Some(val()?.parse()?).filter(|&n| n > 0),
            "--hours" => a.hours = Some(val()?.parse()?).filter(|&n| n > 0),
            "-h" | "--help" => {
                println!("kall-server [serve|invite|cert|ddns] [--data DIR] [--bind ADDR] [--name NAME] [--domain D] [--email E] [--staging] [--no-ddns] [--ipv6] [--uses N] [--hours H]");
                println!("Let's Encrypt + DDNS need --domain and CF_API_TOKEN in the environment.");
                std::process::exit(0);
            }
            other => bail!("unknown argument {other}"),
        }
    }
    a.domain = a.domain.map(|d| d.trim().trim_end_matches('.').to_lowercase());
    Ok(a)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,kall_server=info".into()))
        .init();
    rustls::crypto::ring::default_provider().install_default().ok();
    let a = args()?;
    std::fs::create_dir_all(&a.data).with_context(|| format!("creating {}", a.data.display()))?;

    let cf = a.cf_token.as_ref().map(Cloudflare::new);
    let acme = match (&a.domain, &cf) {
        (Some(domain), Some(cf)) => Some(Acme {
            domain: domain.clone(),
            email: a.email.clone(),
            staging: a.staging,
            dir: a.data.join("acme"),
            dns: kall_server::acme::Dns::Cloudflare(cf.clone()),
            directory: None,
        }),
        _ => None,
    };
    let ddns = match (&a.domain, &cf) {
        (Some(domain), Some(cf)) if a.ddns => Some(Ddns { domain: domain.clone(), cf: cf.clone(), ipv6: a.ipv6 }),
        _ => None,
    };

    match a.cmd.as_str() {
        "invite" => {
            let db = Db::open(&a.data.join("kall.db"))?;
            let expires = a.hours.map(|h| db::now() + h as i64 * 3_600_000);
            println!("{}", db.create_invite(&hub::random_code(10), None, a.uses, expires, false)?.code);
            return Ok(());
        }
        "cert" => {
            let acme = acme.context("`cert` needs --domain and CF_API_TOKEN")?;
            let id = acme.ensure().await?;
            println!("certificate for {} valid for {} days ({})", acme.domain, Acme::days_left(&id), if acme.staging { "staging" } else { "production" });
            return Ok(());
        }
        "ddns" => {
            let ddns = ddns.context("`ddns` needs --domain and CF_API_TOKEN (and DDNS not disabled)")?;
            let ips = ddns.update(&reqwest::Client::new()).await?;
            println!("{} → {}", ddns.domain, ips.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(", "));
            return Ok(());
        }
        _ => {}
    }

    // DNS first: Let's Encrypt doesn't need the A record, but friends do.
    if let Some(d) = &ddns {
        tokio::spawn(d.clone().run_forever());
    }

    // A real certificate when we can get one; otherwise self-signed (clients pin it).
    let (identity, real) = match &acme {
        Some(acme) => match acme.ensure().await {
            Ok(id) => (id, true),
            Err(e) => {
                tracing::error!("couldn't get a Let's Encrypt certificate, serving a self-signed one for now: {e:#}");
                (tls::self_signed(&a.data, &a.domain.iter().cloned().collect::<Vec<_>>())?, false)
            }
        },
        None => {
            if a.domain.is_some() {
                tracing::warn!("KALL_DOMAIN is set but CF_API_TOKEN isn't: using a self-signed certificate");
            }
            (tls::self_signed(&a.data, &a.domain.iter().cloned().collect::<Vec<_>>())?, false)
        }
    };
    if !real {
        tracing::info!("self-signed certificate fingerprint {}", identity.fingerprint());
    }

    let db = Db::open(&a.data.join("kall.db"))?;
    let server = kall_server::Server::start(db, a.name, a.bind, identity)?;
    if let Some(acme) = acme {
        tokio::spawn(kall_server::acme::renew_forever(acme, server.endpoint.clone(), real));
    }
    tokio::select! {
        _ = server.run() => {}
        _ = shutdown() => tracing::info!("shutting down"),
    }
    server.endpoint.close(0u32.into(), b"server shutting down");
    Ok(())
}

/// Ctrl-C, or SIGTERM (what `docker stop` sends).
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await.ok();
}
