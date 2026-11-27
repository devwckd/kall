//! Embed the ghost icon (and version info) in kall.exe, for Explorer, the Start menu and pinned
//! taskbar buttons. The running window's icon is set separately (`rune::set_app_icon`).

/// FFmpeg's DLLs that kall loads (the `ffmpeg` feature): the codec, format and utility
/// libraries and their converters. Not avdevice/avfilter or the command-line tools.
const FFMPEG_DLLS: &[&str] = &["avcodec", "avformat", "avutil", "swscale", "swresample"];

/// With the `ffmpeg` feature on Windows, put FFmpeg's DLLs next to the built kall.exe, so
/// `cargo run` and tests start without touching PATH. (Installers bundle them separately.)
fn copy_ffmpeg_dlls() {
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    if std::env::var_os("CARGO_FEATURE_FFMPEG").is_none() || std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let (Some(ff), Some(out)) = (std::env::var_os("FFMPEG_DIR"), std::env::var_os("OUT_DIR")) else { return };
    // OUT_DIR is target/<profile>/build/kall-*/out.
    let Some(profile) = std::path::Path::new(&out).ancestors().nth(3) else { return };
    let bin = std::path::Path::new(&ff).join("bin");
    let Ok(entries) = std::fs::read_dir(&bin) else { return };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(".dll") && FFMPEG_DLLS.iter().any(|d| name.starts_with(&format!("{d}-"))) {
            for dir in [profile.to_path_buf(), profile.join("deps")] {
                std::fs::copy(e.path(), dir.join(&name)).ok();
            }
        }
    }
}

fn main() {
    copy_ffmpeg_dlls();
    println!("cargo:rerun-if-changed=icons/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icons/icon.ico").set("ProductName", "kall").set("FileDescription", "kall");
        if let Err(e) = res.compile() {
            println!("cargo:warning=couldn't embed the Windows icon: {e}");
        }
    }
}
