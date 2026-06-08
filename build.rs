// Share the procedural icon renderer with the crate (std-only, no extra deps).
include!("src/icon_render.rs");

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=src/icon_render.rs");
    slint_build::compile("ui/app-window.slint").expect("failed to compile Slint UI");
    embed_app_icon();
    emit_git_version();
}

/// Derives the version shown in the About panel from git and injects it as the
/// `GIT_VERSION` compile-time env var. Precedence:
///
/// 1. `git describe --tags --dirty` — when a tag is reachable, e.g. `v0.2.0`,
///    `v0.2.0-3-gabc1234`, or `v0.2.0-dirty`.
/// 2. `CARGO_PKG_VERSION` + short commit hash — when the repo has no tags yet,
///    e.g. `0.1.0+g05b8b3c` (or `0.1.0+g05b8b3c-dirty`).
/// 3. `CARGO_PKG_VERSION` alone — when git is unavailable or this is not a
///    checkout (e.g. building from a packaged source tarball).
fn emit_git_version() {
    // Rebuild when the checked-out commit or tags change. Working-tree
    // dirtiness is best-effort and not fully tracked here.
    for path in [
        ".git/HEAD",
        ".git/index",
        ".git/packed-refs",
        ".git/refs/tags",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }

    let cargo_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let version = git_version().unwrap_or(cargo_version);
    println!("cargo:rustc-env=GIT_VERSION={version}");
}

/// Returns the git-derived version string, or `None` if git is unavailable or
/// the directory is not a git checkout.
fn git_version() -> Option<String> {
    // Tagged build: let git format the canonical describe string.
    if let Some(desc) = run_git(&["describe", "--tags", "--dirty"]) {
        return Some(desc);
    }

    // No tags reachable: fall back to Cargo version + short commit hash.
    let hash = run_git(&["rev-parse", "--short=7", "HEAD"])?;
    let cargo_version = std::env::var("CARGO_PKG_VERSION").ok()?;
    let dirty = match run_git(&["status", "--porcelain"]) {
        Some(out) if !out.is_empty() => "-dirty",
        _ => "",
    };
    Some(format!("{cargo_version}+g{hash}{dirty}"))
}

/// Runs `git <args>` and returns trimmed stdout on success, or `None` on any
/// failure (git missing, non-zero exit, not a repo, empty output).
fn run_git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
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
