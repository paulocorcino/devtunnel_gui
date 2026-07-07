//! Generates the Microsoft Store (MSIX) visual assets from the procedural app
//! icon, so the package's `Assets\` folder is fully reproducible from source
//! (no binary blobs checked in). Reuses `icon_render.rs` — the same renderer that
//! drives the tray icon and the embedded executable icon — and encodes PNGs with
//! the `ico` crate's PNG writer.
//!
//! Usage:  `cargo run --features store --bin gen_msix_assets -- <out-dir>`
//! (defaults to `packaging/msix/Assets` when no argument is given). Invoked by
//! `packaging/msix/build-msix.ps1`.

// Share the std-only procedural renderer with the crate (same include! the build
// script uses to encode the .ico).
include!("../icon_render.rs");

use std::path::{Path, PathBuf};

/// One MSIX asset: output filename and the square edge size to render at.
struct Square {
    name: &'static str,
    size: u32,
}

/// Square logos the manifest references. Scale-100 baseline — enough for a valid
/// package and WACK pass; add scale-125/150/200/400 variants later for crisper
/// tiles on high-DPI displays (same names with a `.scale-200` infix).
const SQUARES: &[Square] = &[
    // App-list / taskbar / Start small icon.
    Square {
        name: "Square44x44Logo.png",
        size: 44,
    },
    // Small tile.
    Square {
        name: "Square71x71Logo.png",
        size: 71,
    },
    // Medium tile (required).
    Square {
        name: "Square150x150Logo.png",
        size: 150,
    },
    // Large tile.
    Square {
        name: "Square310x310Logo.png",
        size: 310,
    },
    // Store listing logo carried inside the package.
    Square {
        name: "StoreLogo.png",
        size: 50,
    },
];

/// Wide tile (310x150): the square mark centred on a transparent canvas.
const WIDE: (&str, u32, u32) = ("Wide310x150Logo.png", 310, 150);

fn main() {
    let out = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("packaging/msix/Assets"));

    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("error: cannot create {}: {e}", out.display());
        std::process::exit(1);
    }

    for sq in SQUARES {
        let data = rgba(sq.size, IconVariant::Normal);
        if let Err(e) = write_png(&out.join(sq.name), sq.size, sq.size, data) {
            eprintln!("error: writing {}: {e}", sq.name);
            std::process::exit(1);
        }
        println!("  {} ({}x{})", sq.name, sq.size, sq.size);
    }

    let (name, w, h) = WIDE;
    let data = wide_canvas(w, h);
    if let Err(e) = write_png(&out.join(name), w, h, data) {
        eprintln!("error: writing {name}: {e}");
        std::process::exit(1);
    }
    println!("  {name} ({w}x{h})");

    println!("MSIX assets written to {}", out.display());
}

/// Builds a `w`x`h` RGBA canvas (transparent) with the square icon centred,
/// sized to the shorter edge. Used for the non-square wide tile.
fn wide_canvas(w: u32, h: u32) -> Vec<u8> {
    let edge = w.min(h);
    let icon = rgba(edge, IconVariant::Normal);
    let ox = (w - edge) / 2;
    let oy = (h - edge) / 2;

    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..edge {
        for x in 0..edge {
            let src = ((y * edge + x) * 4) as usize;
            let dst = (((y + oy) * w + (x + ox)) * 4) as usize;
            out[dst..dst + 4].copy_from_slice(&icon[src..src + 4]);
        }
    }
    out
}

/// Encodes straight RGBA8 pixels as a PNG file via the `ico` crate's writer.
fn write_png(path: &Path, w: u32, h: u32, rgba: Vec<u8>) -> std::io::Result<()> {
    let image = ico::IconImage::from_rgba_data(w, h, rgba);
    let file = std::fs::File::create(path)?;
    image.write_png(file)
}
