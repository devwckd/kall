//! Embed the ghost icon (and version info) in kall.exe, for Explorer, the Start menu and pinned
//! taskbar buttons. The running window's icon is set separately (`rune::set_app_icon`).

fn main() {
    println!("cargo:rerun-if-changed=icons/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("icons/icon.ico").set("ProductName", "kall").set("FileDescription", "kall");
        if let Err(e) = res.compile() {
            println!("cargo:warning=couldn't embed the Windows icon: {e}");
        }
    }
}
