//! Distinctive static server-rack system tray icon with status badges.
//!
//! Provides static cached pixmaps at 16x16, 22x22, 32x32, and 48x48.
//! Features:
//! - Server rack chassis with rails, mounting screws, 3 blades, activity LEDs, and drive slits.
//! - Theme-independent dual contour (dark + light) for high contrast on black or white panels.
//! - Shape-distinct badges: Online (✓), Warning/Attention (!), Down (✕), LocalIssue (/),
//!   Disabled (⏸), and Unknown (hollow ring).
//! - Subpixel antialiased rasterization with OnceLock caching across all states and sizes.

use crate::model::Overall;
use ksni::Icon;
use std::sync::OnceLock;

pub const ICON_SIZES: [i32; 4] = [16, 22, 32, 48];

static CACHE: OnceLock<[Vec<Icon>; 7]> = OnceLock::new();

#[inline]
fn state_idx(overall: Overall) -> usize {
    match overall {
        Overall::Unknown => 0,
        Overall::Online => 1,
        Overall::Warning => 2,
        Overall::Down => 3,
        Overall::Attention => 4,
        Overall::LocalIssue => 5,
        Overall::Disabled => 6,
    }
}

/// Returns cached icon pixmaps at 16, 22, 32, and 48 px for the given overall status.
pub fn icons(overall: Overall) -> Vec<Icon> {
    CACHE.get_or_init(render_all)[state_idx(overall)].clone()
}

fn render_all() -> [Vec<Icon>; 7] {
    const STATES: [Overall; 7] = [
        Overall::Unknown,
        Overall::Online,
        Overall::Warning,
        Overall::Down,
        Overall::Attention,
        Overall::LocalIssue,
        Overall::Disabled,
    ];
    let mut all: [Vec<Icon>; 7] = Default::default();
    for (i, &state) in STATES.iter().enumerate() {
        all[i] = ICON_SIZES
            .iter()
            .map(|&sz| render_icon(state, sz))
            .collect();
    }
    all
}

fn render_icon(overall: Overall, sz: i32) -> Icon {
    let mut data = Vec::with_capacity((sz * sz * 4) as usize);
    for py in 0..sz {
        for px in 0..sz {
            let mut sa = 0.0f32;
            let mut sr = 0.0f32;
            let mut sg = 0.0f32;
            let mut sb = 0.0f32;
            for &sy in &[0.25f32, 0.75f32] {
                for &sx in &[0.25f32, 0.75f32] {
                    let (a, r, g, b) = sample(px as f32 + sx, py as f32 + sy, sz as f32, overall);
                    sa += a;
                    sr += a * r;
                    sg += a * g;
                    sb += a * b;
                }
            }
            let a = (sa * 0.25 * 255.0).round().clamp(0.0, 255.0) as u8;
            let (r, g, b) = if sa > 1e-4 {
                (
                    (sr / sa).round().clamp(0.0, 255.0) as u8,
                    (sg / sa).round().clamp(0.0, 255.0) as u8,
                    (sb / sa).round().clamp(0.0, 255.0) as u8,
                )
            } else {
                (0, 0, 0)
            };
            data.extend_from_slice(&[a, r, g, b]);
        }
    }
    Icon {
        width: sz,
        height: sz,
        data,
    }
}

#[inline]
fn dist_seg(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let (dx, dy) = (bx - ax, by - ay);
    let lsq = dx * dx + dy * dy;
    let t = if lsq > 1e-6 {
        (((px - ax) * dx + (py - ay) * dy) / lsq).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (nx, ny) = (ax + t * dx, ay + t * dy);
    ((px - nx).powi(2) + (py - ny).powi(2)).sqrt()
}

#[inline]
fn dist_box(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32) -> f32 {
    let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
    let (hx, hy) = ((x1 - x0) * 0.5, (y1 - y0) * 0.5);
    let dx = (px - cx).abs() - hx;
    let dy = (py - cy).abs() - hy;
    (dx.max(0.0).powi(2) + dy.max(0.0).powi(2)).sqrt() + dx.max(dy).min(0.0)
}

fn sample(x: f32, y: f32, sz: f32, overall: Overall) -> (f32, f32, f32, f32) {
    let s = sz / 16.0;
    let (cx, cy) = (11.5 * s, 11.5 * s);
    let (rb, rcut) = (3.75 * s, 4.55 * s);
    let (dx, dy) = (x - cx, y - cy);
    let dist_b = (dx * dx + dy * dy).sqrt();

    if dist_b <= rcut {
        if dist_b <= rb {
            return sample_badge(dx, dy, dist_b, rb, s, overall);
        }
        let k = ((rcut - dist_b) / (rcut - rb)).clamp(0.0, 1.0).powi(2);
        let rack = sample_rack(x, y, s);
        if rack.0 > 0.0 {
            return (
                rack.0,
                rack.1 * (1.0 - k) + 18.0 * k,
                rack.2 * (1.0 - k) + 20.0 * k,
                rack.3 * (1.0 - k) + 24.0 * k,
            );
        }
        return (k * 0.75, 18.0, 20.0, 24.0);
    }

    sample_rack(x, y, s)
}

fn sample_badge(
    u: f32,
    v: f32,
    dist: f32,
    rb: f32,
    s: f32,
    overall: Overall,
) -> (f32, f32, f32, f32) {
    if dist > rb - 0.65 * s {
        return (1.0, 18.0, 22.0, 26.0); // Dark outer rim
    }
    if overall == Overall::Unknown {
        let r_in = rb - 1.4 * s;
        if dist < r_in - 0.5 * s {
            return (0.0, 0.0, 0.0, 0.0); // Hollow transparent center
        }
        if dist < r_in {
            return (1.0, 24.0, 28.0, 34.0); // Inner dark rim
        }
        return (1.0, 156.0, 163.0, 175.0); // Ring body
    }

    let (base_rgb, in_glyph, glyph_rgb) = match overall {
        Overall::Online => {
            let d1 = dist_seg(u, v, -1.5 * s, -0.1 * s, -0.5 * s, 1.2 * s);
            let d2 = dist_seg(u, v, -0.5 * s, 1.2 * s, 1.5 * s, -1.2 * s);
            (
                (34.0, 197.0, 94.0),
                d1.min(d2) <= 0.40 * s,
                (255.0, 255.0, 255.0),
            )
        }
        Overall::Warning => {
            let db = dist_seg(u, v, 0.0, -1.8 * s, 0.0, 0.3 * s);
            let dd = (u * u + (v - 1.4 * s).powi(2)).sqrt();
            (
                (234.0, 179.0, 8.0),
                db <= 0.40 * s || dd <= 0.45 * s,
                (20.0, 24.0, 30.0),
            )
        }
        Overall::Attention => {
            let db = dist_seg(u, v, 0.0, -1.8 * s, 0.0, 0.3 * s);
            let dd = (u * u + (v - 1.4 * s).powi(2)).sqrt();
            (
                (249.0, 115.0, 22.0),
                db <= 0.42 * s || dd <= 0.48 * s,
                (255.0, 255.0, 255.0),
            )
        }
        Overall::Down => {
            let d1 = dist_seg(u, v, -1.3 * s, -1.3 * s, 1.3 * s, 1.3 * s);
            let d2 = dist_seg(u, v, -1.3 * s, 1.3 * s, 1.3 * s, -1.3 * s);
            (
                (239.0, 68.0, 68.0),
                d1.min(d2) <= 0.40 * s,
                (255.0, 255.0, 255.0),
            )
        }
        Overall::LocalIssue => {
            let d = dist_seg(u, v, -1.4 * s, 1.4 * s, 1.4 * s, -1.4 * s);
            ((217.0, 119.0, 6.0), d <= 0.42 * s, (255.0, 255.0, 255.0))
        }
        Overall::Disabled => {
            let d1 = dist_seg(u, v, -0.8 * s, -1.4 * s, -0.8 * s, 1.4 * s);
            let d2 = dist_seg(u, v, 0.8 * s, -1.4 * s, 0.8 * s, 1.4 * s);
            (
                (100.0, 116.0, 139.0),
                d1.min(d2) <= 0.35 * s,
                (255.0, 255.0, 255.0),
            )
        }
        Overall::Unknown => unreachable!(),
    };

    if in_glyph {
        (1.0, glyph_rgb.0, glyph_rgb.1, glyph_rgb.2)
    } else {
        (1.0, base_rgb.0, base_rgb.1, base_rgb.2)
    }
}

fn sample_rack(x: f32, y: f32, s: f32) -> (f32, f32, f32, f32) {
    let (x0, x1, y0, y1) = (1.0 * s, 11.5 * s, 1.0 * s, 14.5 * s);
    let d = dist_box(x, y, x0, y0, x1, y1);
    if d > 0.0 {
        return (0.0, 0.0, 0.0, 0.0);
    }
    // Dual contour
    if d >= -0.75 * s {
        return (1.0, 22.0, 26.0, 32.0); // Outer dark
    }
    if d >= -1.45 * s {
        return (1.0, 225.0, 232.0, 240.0); // Inner light
    }
    // Vertical rails with mounting screws
    if x <= x0 + 2.0 * s {
        let x_sc = x0 + 1.2 * s;
        for &ys in &[y0 + 2.2 * s, y0 + 6.75 * s, y0 + 11.3 * s] {
            if ((x - x_sc).powi(2) + (y - ys).powi(2)).sqrt() <= 0.45 * s {
                return (1.0, 210.0, 215.0, 225.0);
            }
        }
        return (1.0, 75.0, 82.0, 95.0);
    }
    if x >= x1 - 1.2 * s {
        return (1.0, 75.0, 82.0, 95.0);
    }

    // 3 stacked server blades
    let (ytop, ybot) = (y0 + 1.45 * s, y1 - 1.45 * s);
    let uh = (ybot - ytop) / 3.0;
    let yrel = (y - ytop).max(0.0);
    let idx = ((yrel / uh).floor() as usize).min(2);
    let yu = yrel - (idx as f32) * uh;

    if yu <= 0.35 * s || yu >= uh - 0.35 * s {
        return (1.0, 28.0, 32.0, 40.0); // Divider slot
    }
    let (x_led, y_led) = (x0 + 2.8 * s, ytop + (idx as f32 + 0.5) * uh);
    if ((x - x_led).powi(2) + (y - y_led).powi(2)).sqrt() <= 0.45 * s {
        return (1.0, 52.0, 211.0, 153.0); // Drive activity LED
    }
    if x >= x0 + 3.8 * s && (y - y_led).abs() <= 0.30 * s {
        return (1.0, 35.0, 40.0, 50.0); // Drive bay slit
    }
    let t = (yu / uh).clamp(0.0, 1.0);
    (1.0, 170.0 - 25.0 * t, 178.0 - 26.0 * t, 190.0 - 25.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Overall;

    const ALL_STATES: [Overall; 7] = [
        Overall::Unknown,
        Overall::Online,
        Overall::Warning,
        Overall::Down,
        Overall::Attention,
        Overall::LocalIssue,
        Overall::Disabled,
    ];

    #[test]
    fn test_distinct_states_and_dual_contour() {
        let mut pixmaps = Vec::new();
        for state in ALL_STATES {
            let state_icons = icons(state);
            assert_eq!(state_icons.len(), 4);
            pixmaps.push((
                state,
                state_icons[0].data.clone(),
                state_icons[1].data.clone(),
            ));

            // Verify dual contour and transparent canvas for theme independence
            for icon in &state_icons {
                let mut has_trans = false;
                let mut has_dark = false;
                let mut has_light = false;
                for &[a, r, g, b] in icon.data.as_chunks::<4>().0 {
                    if a == 0 {
                        has_trans = true;
                    } else if a >= 200 {
                        if r <= 60 && g <= 60 && b <= 60 {
                            has_dark = true;
                        }
                        if r >= 170 && g >= 170 && b >= 170 {
                            has_light = true;
                        }
                    }
                }
                assert!(
                    has_trans && has_dark && has_light,
                    "State {:?} missing theme contrast at {}px",
                    state,
                    icon.width
                );
            }
        }

        // Pairwise distinctness across all states
        for i in 0..pixmaps.len() {
            for j in (i + 1)..pixmaps.len() {
                assert_ne!(
                    pixmaps[i].1, pixmaps[j].1,
                    "{:?} vs {:?} identical at 16px",
                    pixmaps[i].0, pixmaps[j].0
                );
                assert_ne!(
                    pixmaps[i].2, pixmaps[j].2,
                    "{:?} vs {:?} identical at 22px",
                    pixmaps[i].0, pixmaps[j].0
                );
            }
        }
    }

    #[test]
    fn test_structural_badge_shapes() {
        let unknown = &icons(Overall::Unknown)[2]; // 32px
        let online = &icons(Overall::Online)[2];
        let down = &icons(Overall::Down)[2];
        let disabled = &icons(Overall::Disabled)[2];

        // Center of badge at 32px is (23, 23)
        let center_offset = (23 * 32 + 23) * 4;
        assert_eq!(
            unknown.data[center_offset], 0,
            "Unknown badge must be hollow (alpha 0) at center"
        );
        assert!(
            online.data[center_offset] > 200,
            "Online badge must be solid (alpha > 200) at center"
        );

        // Cross vs Pause raster data must differ structurally
        assert_ne!(
            down.data, disabled.data,
            "Down cross and Disabled pause must differ geometrically"
        );
    }
}
