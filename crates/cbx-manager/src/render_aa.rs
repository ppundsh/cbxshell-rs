use cbx_core::image_dec::create_hbitmap_from_bgra;
use windows::Win32::Graphics::Gdi::{
    AlphaBlend, CreateCompatibleDC, DeleteDC, DeleteObject, SelectObject,
    AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, HDC,
};

/// Blends a 32bpp premultiplied BGRA buffer directly onto the target HDC.
pub unsafe fn blend_bgra_to_hdc(
    hdc: HDC,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    bgra: &[u8],
) {
    if w <= 0 || h <= 0 || bgra.len() < (w * h * 4) as usize {
        return;
    }

    let Ok(hbmp) = create_hbitmap_from_bgra(w as u32, h as u32, bgra) else {
        return;
    };

    let mem_dc = CreateCompatibleDC(hdc);
    let old_bmp = SelectObject(mem_dc, hbmp);

    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };

    let _ = AlphaBlend(hdc, x, y, w, h, mem_dc, 0, 0, w, h, blend);

    SelectObject(mem_dc, old_bmp);
    let _ = DeleteDC(mem_dc);
    let _ = DeleteObject(hbmp);
}

/// Renders a mathematically anti-aliased smooth rounded capsule / pill.
/// Colors are in standard RGB format: 0x00RRGGBB.
pub unsafe fn draw_aa_pill(
    hdc: HDC,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    fill_rgb: u32,
    fill_alpha: f32,
    border_rgb: u32,
    border_alpha: f32,
    border_width: f32,
) {
    if w <= 0 || h <= 0 {
        return;
    }

    let mut buf = vec![0u8; (w * h * 4) as usize];
    let r = (h as f32) / 2.0;
    let max_rx = (w as f32 - r).max(r);

    let (fr, fg, fb) = (
        ((fill_rgb >> 16) & 0xFF) as f32,
        ((fill_rgb >> 8) & 0xFF) as f32,
        (fill_rgb & 0xFF) as f32,
    );
    let (br, bg, bb) = (
        ((border_rgb >> 16) & 0xFF) as f32,
        ((border_rgb >> 8) & 0xFF) as f32,
        (border_rgb & 0xFF) as f32,
    );

    for py in 0..h {
        let cy = py as f32 + 0.5;
        let dy = cy - r;
        let dy2 = dy * dy;

        for px in 0..w {
            let cx = px as f32 + 0.5;
            let clamped_x = cx.clamp(r, max_rx);
            let dx = cx - clamped_x;
            let dist = (dx * dx + dy2).sqrt();

            let signed_dist = dist - r;
            // Anti-aliased outer edge coverage in [0.0, 1.0]
            let outer_cov = (0.5 - signed_dist).clamp(0.0, 1.0);
            if outer_cov <= 0.0 {
                continue;
            }

            let (cr, cg, cb, ca) = if border_width > 0.0 {
                let border_edge = signed_dist + border_width;
                if border_edge >= 0.0 {
                    // In border zone
                    let t = (border_edge / 0.75).clamp(0.0, 1.0);
                    let red = fr + (br - fr) * t;
                    let green = fg + (bg - fg) * t;
                    let blue = fb + (bb - fb) * t;
                    let alpha = fill_alpha + (border_alpha - fill_alpha) * t;
                    (red, green, blue, alpha * outer_cov)
                } else {
                    // Inside fill
                    (fr, fg, fb, fill_alpha * outer_cov)
                }
            } else {
                (fr, fg, fb, fill_alpha * outer_cov)
            };

            // Premultiplied BGRA
            let a_clamped = ca.clamp(0.0, 1.0);
            let pr = (cr * a_clamped).round().clamp(0.0, 255.0) as u8;
            let pg = (cg * a_clamped).round().clamp(0.0, 255.0) as u8;
            let pb = (cb * a_clamped).round().clamp(0.0, 255.0) as u8;
            let pa = (a_clamped * 255.0).round().clamp(0.0, 255.0) as u8;

            let idx = ((py * w + px) * 4) as usize;
            buf[idx] = pb;
            buf[idx + 1] = pg;
            buf[idx + 2] = pr;
            buf[idx + 3] = pa;
        }
    }

    blend_bgra_to_hdc(hdc, x, y, w, h, &buf);
}

/// Renders a mathematically anti-aliased pure circle.
/// Colors are in standard RGB format: 0x00RRGGBB.
pub unsafe fn draw_aa_circle(
    hdc: HDC,
    center_x: i32,
    center_y: i32,
    radius: f32,
    fill_rgb: u32,
    fill_alpha: f32,
    border_rgb: u32,
    border_alpha: f32,
    border_width: f32,
) {
    if radius <= 0.0 {
        return;
    }

    let side = ((radius + border_width) * 2.0).ceil() as i32 + 3;
    let mut buf = vec![0u8; (side * side * 4) as usize];
    let center = side as f32 / 2.0;

    let (fr, fg, fb) = (
        ((fill_rgb >> 16) & 0xFF) as f32,
        ((fill_rgb >> 8) & 0xFF) as f32,
        (fill_rgb & 0xFF) as f32,
    );
    let (br, bg, bb) = (
        ((border_rgb >> 16) & 0xFF) as f32,
        ((border_rgb >> 8) & 0xFF) as f32,
        (border_rgb & 0xFF) as f32,
    );

    for py in 0..side {
        let cy = py as f32 + 0.5;
        let dy = cy - center;
        let dy2 = dy * dy;

        for px in 0..side {
            let cx = px as f32 + 0.5;
            let dx = cx - center;
            let dist = (dx * dx + dy2).sqrt();

            let signed_dist = dist - radius;
            let outer_cov = (0.5 - signed_dist).clamp(0.0, 1.0);
            if outer_cov <= 0.0 {
                continue;
            }

            let (cr, cg, cb, ca) = if border_width > 0.0 {
                let border_edge = signed_dist + border_width;
                if border_edge >= 0.0 {
                    let t = (border_edge / 0.75).clamp(0.0, 1.0);
                    let red = fr + (br - fr) * t;
                    let green = fg + (bg - fg) * t;
                    let blue = fb + (bb - fb) * t;
                    let alpha = fill_alpha + (border_alpha - fill_alpha) * t;
                    (red, green, blue, alpha * outer_cov)
                } else {
                    (fr, fg, fb, fill_alpha * outer_cov)
                }
            } else {
                (fr, fg, fb, fill_alpha * outer_cov)
            };

            let a_clamped = ca.clamp(0.0, 1.0);
            let pr = (cr * a_clamped).round().clamp(0.0, 255.0) as u8;
            let pg = (cg * a_clamped).round().clamp(0.0, 255.0) as u8;
            let pb = (cb * a_clamped).round().clamp(0.0, 255.0) as u8;
            let pa = (a_clamped * 255.0).round().clamp(0.0, 255.0) as u8;

            let idx = ((py * side + px) * 4) as usize;
            buf[idx] = pb;
            buf[idx + 1] = pg;
            buf[idx + 2] = pr;
            buf[idx + 3] = pa;
        }
    }

    let top_left_x = center_x - side / 2;
    let top_left_y = center_y - side / 2;
    blend_bgra_to_hdc(hdc, top_left_x, top_left_y, side, side, &buf);
}

/// Linear interpolation between two RGB colors (0x00RRGGBB).
pub fn lerp_color_rgb(c1: u32, c2: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let r1 = ((c1 >> 16) & 0xFF) as f32;
    let g1 = ((c1 >> 8) & 0xFF) as f32;
    let b1 = (c1 & 0xFF) as f32;

    let r2 = ((c2 >> 16) & 0xFF) as f32;
    let g2 = ((c2 >> 8) & 0xFF) as f32;
    let b2 = (c2 & 0xFF) as f32;

    let r = (r1 + (r2 - r1) * t).round() as u32;
    let g = (g1 + (g2 - g1) * t).round() as u32;
    let b = (b1 + (b2 - b1) * t).round() as u32;

    (r << 16) | (g << 8) | b
}

/// Blends a uniform semi-transparent rectangular curtain over the target area.
pub unsafe fn draw_semi_transparent_rect(
    hdc: HDC,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    fill_rgb: u32,
    alpha: f32,
) {
    if w <= 0 || h <= 0 {
        return;
    }
    let a_byte = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    let r = (((fill_rgb >> 16) & 0xFF) as f32 * alpha).round() as u8;
    let g = (((fill_rgb >> 8) & 0xFF) as f32 * alpha).round() as u8;
    let b = ((fill_rgb & 0xFF) as f32 * alpha).round() as u8;

    let total_pixels = (w * h) as usize;
    let mut buf = Vec::with_capacity(total_pixels * 4);
    for _ in 0..total_pixels {
        buf.push(b);
        buf.push(g);
        buf.push(r);
        buf.push(a_byte);
    }
    blend_bgra_to_hdc(hdc, x, y, w, h, &buf);
}

fn dist_point_to_segment(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    let vx = bx - ax;
    let vy = by - ay;
    let len2 = vx * vx + vy * vy;
    if len2 <= 1e-6 {
        return ((px - ax).powi(2) + (py - ay).powi(2)).sqrt();
    }
    let t = (((px - ax) * vx + (py - ay) * vy) / len2).clamp(0.0, 1.0);
    let proj_x = ax + vx * t;
    let proj_y = ay + vy * t;
    ((px - proj_x).powi(2) + (py - proj_y).powi(2)).sqrt()
}

/// Renders a Fluent dark-mode checkbox with rounded corners, hover color transitions,
/// and a left-to-right animated checkmark stroke (`check_progress` in `0.0..=1.0`).
/// Colors use standard `0x00RRGGBB` layout.
pub unsafe fn draw_aa_checkbox(
    hdc: HDC,
    x: i32,
    y: i32,
    size: i32,
    is_checked: bool,
    is_hovered: bool,
    is_disabled: bool,
    check_progress: f32,
) {
    if size <= 2 {
        return;
    }
    let s = size as f32;
    let half = s * 0.5;
    let corner_r = (4.0 * (s / 18.0)).clamp(2.0, half - 1.0);
    let border_w = if !is_checked { 1.15f32 } else { 0.0f32 };

    let dark = crate::is_dark_theme();
    let (fill_rgb, border_rgb, mark_rgb) = if is_disabled {
        if is_checked {
            if dark {
                (0x00383840u32, 0x004E4E56u32, 0x00888888u32)
            } else {
                (0x00C2C2C6u32, 0x00B4B4B8u32, 0x00FFFFFFu32)
            }
        } else if dark {
            (0x00222020u32, 0x003E3A3Au32, 0x00888888u32)
        } else {
            (0x00F3F3F3u32, 0x00D0D0D0u32, 0x00A0A0A0u32)
        }
    } else if is_checked {
        let fill = if is_hovered {
            crate::accent_checkbox_hover_rgb()
        } else {
            crate::accent_base_rgb()
        };
        let mark = crate::accent_contrast_text_rgb();
        (fill, fill, mark)
    } else if is_hovered {
        if dark {
            (0x00363131u32, 0x00A5A5AAu32, 0x00FFFFFFu32)
        } else {
            (0x00EFEFEFu32, 0x005C5C5Cu32, 0x001A1A1Au32)
        }
    } else if dark {
        (0x00242020u32, 0x00666666u32, 0x00FFFFFFu32)
    } else {
        (0x00FBFBFBu32, 0x008A8A8Au32, 0x001A1A1Au32)
    };

    let (fr, fg, fb) = (
        ((fill_rgb >> 16) & 0xFF) as f32,
        ((fill_rgb >> 8) & 0xFF) as f32,
        (fill_rgb & 0xFF) as f32,
    );
    let (br, bg, bb) = (
        ((border_rgb >> 16) & 0xFF) as f32,
        ((border_rgb >> 8) & 0xFF) as f32,
        (border_rgb & 0xFF) as f32,
    );
    let (mr, mg, mb) = (
        ((mark_rgb >> 16) & 0xFF) as f32,
        ((mark_rgb >> 8) & 0xFF) as f32,
        (mark_rgb & 0xFF) as f32,
    );

    let p0 = (0.23 * s, 0.52 * s);
    let p1 = (0.43 * s, 0.72 * s);
    let p2 = (0.78 * s, 0.29 * s);
    let l1 = ((p1.0 - p0.0).powi(2) + (p1.1 - p0.1).powi(2)).sqrt();
    let l2 = ((p2.0 - p1.0).powi(2) + (p2.1 - p1.1).powi(2)).sqrt();
    let total_len = l1 + l2;
    let drawn_len = if is_checked {
        check_progress.clamp(0.0, 1.0) * total_len
    } else {
        0.0
    };
    let stroke_r = (s * 0.075).max(1.10);

    let bx = half - corner_r;
    let by = half - corner_r;
    let mut buf = vec![0u8; (size * size * 4) as usize];

    for py in 0..size {
        let cy = py as f32 + 0.5;
        let qy = (cy - half).abs() - by;
        for px in 0..size {
            let cx = px as f32 + 0.5;
            let qx = (cx - half).abs() - bx;
            let dx = qx.max(0.0);
            let dy = qy.max(0.0);
            let signed_dist = (dx * dx + dy * dy).sqrt() + qx.max(qy).min(0.0) - corner_r;
            let outer_cov = (0.5 - signed_dist).clamp(0.0, 1.0);
            if outer_cov <= 0.0 {
                continue;
            }

            let (mut r, mut g, mut b) = if border_w > 0.0 {
                let border_t = ((signed_dist + border_w) / 0.85).clamp(0.0, 1.0);
                (
                    fr + (br - fr) * border_t,
                    fg + (bg - fg) * border_t,
                    fb + (bb - fb) * border_t,
                )
            } else {
                (fr, fg, fb)
            };

            if drawn_len > 0.01 {
                let end1_t = (drawn_len / l1).clamp(0.0, 1.0);
                let end1 = (
                    p0.0 + (p1.0 - p0.0) * end1_t,
                    p0.1 + (p1.1 - p0.1) * end1_t,
                );
                let mut md = dist_point_to_segment(cx, cy, p0.0, p0.1, end1.0, end1.1);
                if drawn_len > l1 {
                    let end2_t = ((drawn_len - l1) / l2).clamp(0.0, 1.0);
                    let end2 = (
                        p1.0 + (p2.0 - p1.0) * end2_t,
                        p1.1 + (p2.1 - p1.1) * end2_t,
                    );
                    md = md.min(dist_point_to_segment(cx, cy, p1.0, p1.1, end2.0, end2.1));
                }
                let mark_cov = ((stroke_r + 0.5 - md) / 1.0).clamp(0.0, 1.0);
                if mark_cov > 0.0 {
                    r = r + (mr - r) * mark_cov;
                    g = g + (mg - g) * mark_cov;
                    b = b + (mb - b) * mark_cov;
                }
            }

            let pr = (r * outer_cov).round().clamp(0.0, 255.0) as u8;
            let pg = (g * outer_cov).round().clamp(0.0, 255.0) as u8;
            let pb = (b * outer_cov).round().clamp(0.0, 255.0) as u8;
            let pa = (outer_cov * 255.0).round().clamp(0.0, 255.0) as u8;

            let idx = ((py * size + px) * 4) as usize;
            buf[idx] = pb;
            buf[idx + 1] = pg;
            buf[idx + 2] = pr;
            buf[idx + 3] = pa;
        }
    }

    blend_bgra_to_hdc(hdc, x, y, size, size, &buf);
}


