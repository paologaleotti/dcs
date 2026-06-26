//! Embeds the app icon into the Windows `.exe` so Explorer, the taskbar, and the
//! installer shortcut show it. No-op on every other platform. The runtime
//! window/dock icon is set separately in `main.rs` via `with_icon`.

fn main() {
    #[cfg(windows)]
    {
        println!("cargo:rerun-if-changed=../../assets/icon.ico");
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon.ico");
        if let Err(e) = res.compile() {
            // A missing resource compiler shouldn't kill a dev build; the icon is
            // cosmetic. CI has the Windows SDK, so release artifacts get it.
            println!("cargo:warning=failed to embed Windows icon: {e}");
        }
    }
}
