//! Self-update from GitHub Releases: a signed `latest.json` names the newest version and, per
//! platform, the installer and its minisign signature (checked against `updater.pub`).
//!
//! The release workflow bakes the repository's `latest.json` URL in at build time
//! (`KALL_RELEASES`), so only official builds self-update; local builds don't check.
//! At run time `KALL_UPDATE_URL` points at another `latest.json` (for testing).

use anyhow::{Result, anyhow};
use cargo_packager_updater::{Config, Update, UpdaterBuilder, semver::Version, url::Url};

/// Where official builds look for updates; unset in local builds.
pub const RELEASES: Option<&str> = option_env!("KALL_RELEASES");
const PUBKEY: &str = include_str!("../updater.pub");

pub fn endpoint() -> Option<String> {
    if let Ok(u) = std::env::var("KALL_UPDATE_URL")
        && !u.is_empty()
    {
        return Some(u);
    }
    RELEASES.filter(|u| !u.is_empty()).map(str::to_string)
}

/// Ask whether a newer version exists (blocking: call off the UI thread).
pub fn check() -> Result<Option<Update>> {
    let Some(url) = endpoint() else { return Ok(None) };
    check_at(&url)
}

pub fn check_at(url: &str) -> Result<Option<Update>> {
    let config = Config { endpoints: vec![Url::parse(url)?], pubkey: PUBKEY.trim().to_string(), ..Default::default() };
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    UpdaterBuilder::new(current, config).build().map_err(|e| anyhow!("{e}"))?.check().map_err(|e| anyhow!("{e}"))
}

/// Where people download kall by hand when the self-update can't run.
pub const DOWNLOADS: &str = "https://github.com/devwckd/kall/releases/latest";

/// Can this copy of kall replace itself? Checked before hanging up for an update, so a
/// self-update that can't work says why instead of restarting the old version.
pub fn preflight() -> Result<()> {
    if cfg!(target_os = "linux") {
        return appimage_preflight();
    }
    Ok(())
}

/// The AppImage the user started (the runtime sets `APPIMAGE`); kall's own executable lives
/// inside its read-only mount and can't be replaced.
fn appimage() -> Option<std::path::PathBuf> {
    std::env::var_os("APPIMAGE").filter(|p| !p.is_empty()).map(std::path::PathBuf::from)
}

fn appimage_preflight() -> Result<()> {
    let Some(file) = appimage() else {
        return Err(anyhow!(
            "only the AppImage updates itself. Download the new version, or with the .deb,              install the new .deb"
        ));
    };
    let dir = file.parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    // The updater moves the old AppImage aside and writes the new one in its place, so both
    // the file and its folder must be writable.
    let file_ok = std::fs::OpenOptions::new().append(true).open(&file).is_ok();
    let probe = dir.join(format!(".kall-update-{}", std::process::id()));
    let dir_ok = std::fs::write(&probe, b"").is_ok();
    std::fs::remove_file(&probe).ok();
    if !file_ok || !dir_ok {
        return Err(anyhow!(
            "no permission to replace {}. Move kall to a folder you own (like ~/Applications),              or download the new version",
            file.display()
        ));
    }
    Ok(())
}

/// Download, verify the signature, install, and restart (on Windows the installer restarts
/// kall itself).
pub fn install(update: &Update) -> Result<()> {
    preflight()?;
    update.download_and_install().map_err(|e| anyhow!("{e}"))?;
    if !cfg!(windows) {
        // An AppImage restarts from the (now replaced) file, not from the old image's mount.
        let exe = match appimage() {
            Some(file) => file,
            None => std::env::current_exe()?,
        };
        std::process::Command::new(exe).spawn()?;
    }
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    /// Against a local server with a signed installer: `KALL_UPDATE_URL=http://…/latest.json
    /// KALL_UPDATE_BAD=http://…/latest-bad.json cargo test -p kall --release updater_live -- --ignored`
    #[test]
    #[ignore = "needs a local release server"]
    fn updater_live() {
        let u = super::check().unwrap().expect("an update is offered");
        assert_eq!(u.version.trim_start_matches('v'), "0.2.0");
        let bytes = u.download().expect("downloads and the signature verifies");
        assert!(bytes.len() > 1_000_000);
        // A file that doesn't match its signature is refused.
        let bad = super::check_at(&std::env::var("KALL_UPDATE_BAD").unwrap()).unwrap().expect("offered");
        assert!(bad.download().is_err(), "tampered download must fail verification");
    }
}
