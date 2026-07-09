//! Embeds the app icon into the Windows `.exe` (Explorer, taskbar, installer
//! shortcut — no-op elsewhere; the runtime window/dock icon is set in `main.rs`
//! via `with_icon`) and, when AI search is compiled in, adds an rpath so the
//! binary finds the ONNX Runtime WebGPU library (`libwebgpu_dawn`) shipped next
//! to it. Windows resolves DLLs from the exe directory natively, so only macOS
//! and Linux need the rpath.

fn main() {
    if std::env::var_os("CARGO_FEATURE_AI_SEARCH").is_some() {
        let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        // Several search paths per OS: next to the binary (dev builds, portable
        // archives, NSIS), plus the spots cargo-packager stages resources in the
        // .app bundle and the Linux packages.
        let rpaths: &[&str] = match target.as_str() {
            "macos" => &[
                "@executable_path",
                "@executable_path/../Frameworks",
                "@executable_path/../Resources",
            ],
            "linux" => &["$ORIGIN", "$ORIGIN/../lib", "$ORIGIN/../lib/dcs"],
            _ => &[],
        };
        for rpath in rpaths {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{rpath}");
        }
    }

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
