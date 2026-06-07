// Procedural app icon: a rounded indigo tile with a concentric "tunnel portal"
// mark — two rings narrowing to a bright centre dot (the light at the end of the
// tunnel). Rendered at any size so one design drives the tray icon, the
// preflight-warning variant, and the embedded executable / taskbar icon
// (`build.rs` `include!`s this file to encode the `.ico`).
//
// Kept dependency-free (std only) so it can be shared between the crate and the
// build script without pulling extra build dependencies. Uses `//` (not `//!`)
// comments and outer attributes so it stays valid when `include!`d mid-file.

/// Colour scheme to render.
#[allow(dead_code)] // `Warning` is unused in the build.rs `include!`.
#[derive(Clone, Copy)]
pub enum IconVariant {
    /// Brand indigo — the normal app / tray / executable icon.
    Normal,
    /// Amber — shown on the tray while the app is not "ready"
    /// (CLI missing / re-login required).
    Warning,
}

/// Renders the icon into a straight (non-premultiplied) RGBA8 buffer of
/// `size * size` pixels, ready for `tray_icon::Icon::from_rgba` or `.ico`
/// encoding.
pub fn rgba(size: u32, variant: IconVariant) -> Vec<u8> {
    let s = size as f32;

    // Vertical gradient endpoints (top, bottom) per variant. Indigo matches the
    // UI accent (Theme.accent #4f46e5 / #6366f1).
    let (top, bottom) = match variant {
        IconVariant::Normal => ([0x6c, 0x6f, 0xf5], [0x4f, 0x46, 0xe5]),
        IconVariant::Warning => ([0xfb, 0xbf, 0x24], [0xd9, 0x77, 0x06]),
    };
    let white = [0xff, 0xff, 0xff];

    // Geometry as fractions of the icon size.
    let margin = s * 0.04;
    let half = s * 0.5 - margin;
    let corner = s * 0.24;
    let cx = s * 0.5;
    let cy = s * 0.5;
    // Concentric rings read as depth down a tunnel, narrowing to a bright dot —
    // the light at the end. The motif is size-adaptive: thin three-ring depth at
    // large sizes, but fewer/bolder rings at small sizes so it stays crisp and
    // high-contrast in the tray (a thin three-ring mark turns muddy at 16px).
    let (rings, ring, r_dot): (Vec<f32>, f32, f32) = if size <= 20 {
        (vec![s * 0.34], s * 0.075, s * 0.10)
    } else if size <= 48 {
        (vec![s * 0.35, s * 0.20], s * 0.055, s * 0.085)
    } else {
        (vec![s * 0.355, s * 0.260, s * 0.175], s * 0.034, s * 0.072)
    };

    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            // Rounded-rect tile coverage (anti-aliased); fully transparent
            // outside it so the icon is a tile, not a square.
            let tile_cov = coverage(sd_round_rect(px - cx, py - cy, half, corner));
            if tile_cov <= 0.0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
                continue;
            }

            // Background vertical gradient.
            let t = (py / s).clamp(0.0, 1.0);
            let base = [
                lerp_u8(top[0], bottom[0], t),
                lerp_u8(top[1], bottom[1], t),
                lerp_u8(top[2], bottom[2], t),
            ];

            // Concentric tunnel mark (thin rings + centre dot), in white.
            let dist = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            let mut mark = coverage(dist - r_dot);
            for r in &rings {
                mark = mark.max(coverage((dist - r).abs() - ring));
            }
            let mark = mark.clamp(0.0, 1.0);

            out.extend_from_slice(&[
                lerp_u8(base[0], white[0], mark),
                lerp_u8(base[1], white[1], mark),
                lerp_u8(base[2], white[2], mark),
                (tile_cov * 255.0).round() as u8,
            ]);
        }
    }
    out
}

/// Signed distance from a point (relative to the rect centre) to a rounded
/// rectangle of half-extent `half` and corner radius `cr` (negative = inside).
fn sd_round_rect(px: f32, py: f32, half: f32, cr: f32) -> f32 {
    let qx = px.abs() - (half - cr);
    let qy = py.abs() - (half - cr);
    let ox = qx.max(0.0);
    let oy = qy.max(0.0);
    let outside = (ox * ox + oy * oy).sqrt();
    let inside = qx.max(qy).min(0.0);
    outside + inside - cr
}

/// 1px anti-aliased coverage for a signed-distance field (inside = `d < 0`).
fn coverage(d: f32) -> f32 {
    (0.5 - d).clamp(0.0, 1.0)
}

/// Rounds a linear interpolation between two channel values to a `u8`.
fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t.clamp(0.0, 1.0)).round() as u8
}
