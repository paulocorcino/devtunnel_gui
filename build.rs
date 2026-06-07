// Share the procedural icon renderer with the crate (std-only, no extra deps).
include!("src/icon_render.rs");

fn main() {
    println!("cargo:rerun-if-changed=src/icon_render.rs");
    slint_build::compile("ui/app-window.slint").expect("failed to compile Slint UI");
    embed_app_icon();
}

/// Renders the app icon at several resolutions, encodes a multi-size `.ico`, and
/// embeds it as the executable's icon resource so Explorer, the taskbar, and the
/// window title bar all show it (Slint exposes no window-icon API; on Windows the
/// window icon is taken from this resource). Best-effort: a failure only warns,
/// so the build still succeeds without the icon.
#[cfg(windows)]
fn embed_app_icon() {
    use std::path::PathBuf;

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let ico_path = PathBuf::from(&out_dir).join("app-icon.ico");

    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for size in [16u32, 24, 32, 48, 64, 128, 256] {
        let data = rgba(size, IconVariant::Normal);
        let image = ico::IconImage::from_rgba_data(size, size, data);
        match ico::IconDirEntry::encode(&image) {
            Ok(entry) => dir.add_entry(entry),
            Err(e) => {
                println!("cargo:warning=app icon: failed to encode {size}px: {e}");
                return;
            }
        }
    }

    let file = match std::fs::File::create(&ico_path) {
        Ok(f) => f,
        Err(e) => {
            println!(
                "cargo:warning=app icon: cannot create {}: {e}",
                ico_path.display()
            );
            return;
        }
    };
    if let Err(e) = dir.write(file) {
        println!("cargo:warning=app icon: cannot write .ico: {e}");
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico_path.to_str().expect("ico path is valid UTF-8"));
    if let Err(e) = res.compile() {
        println!("cargo:warning=app icon: embedding failed (build continues without it): {e}");
    }
}

#[cfg(not(windows))]
fn embed_app_icon() {}
