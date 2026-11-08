//! Builds Cullery's icon into the exe, so Explorer, shortcuts and the taskbar show it.
//! The icon file comes from tools\make-icon.ps1.

fn main() {
    println!("cargo:rerun-if-changed=assets/cullery.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        // Resource 1: window.rs loads it by that number.
        res.set_icon_with_id("assets/cullery.ico", "1");
        res.compile().expect("couldn't build the icon into the exe");
    }
}
