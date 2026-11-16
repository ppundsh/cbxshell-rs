//! Application icon (`.ico` file and Win32 `HICON`) generator.

use crate::image_dec::create_hbitmap_from_bgra;
use windows::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

/// Renders the application icon as top-down 32bpp BGRA pixels of `size x size`.
pub fn render_app_icon_bgra(size: u32) -> Vec<u8> {
    let s = size.max(16) as usize;
    let mut bgra = vec![0u8; s * s * 4];
    let inv = 1.0 / (s as f32);

    for py in 0..s {
        let v = (py as f32 + 0.5) * inv;
        for px in 0..s {
            let u = (px as f32 + 0.5) * inv;
            let idx = (py * s + px) * 4;

            // Outer rounded square (u, v in [0.06, 0.94], corner radius 0.20)
            let d_outer = sd_rounded_box(u - 0.06, v - 0.06, 0.88, 0.88, 0.20);
            if d_outer > 0.015 {
                continue;
            }
            let edge_aa = ((0.015 - d_outer) / 0.03).clamp(0.0, 1.0);

            // Rich modern indigo-violet diagonal gradient background
            let t = (u * 0.55 + v * 0.45).clamp(0.0, 1.0);
            let mut r = 48.0 * (1.0 - t) + 124.0 * t;
            let mut g = 36.0 * (1.0 - t) + 58.0 * t;
            let mut b = 110.0 * (1.0 - t) + 215.0 * t;

            // Subtle inner glowing border
            if d_outer > -0.045 {
                r = (r + 55.0).min(255.0);
                g = (g + 65.0).min(255.0);
                b = (b + 40.0).min(255.0);
            }

            // Inner white comic/book page card: u in [0.20, 0.78], v in [0.18, 0.82]
            let d_page = sd_rounded_box(u - 0.20, v - 0.18, 0.58, 0.64, 0.07);
            if d_page <= 0.0 {
                r = 246.0;
                g = 248.0;
                b = 252.0;

                // Comic panel frame inside page: u in [0.26, 0.72], v in [0.25, 0.68]
                if (0.26..=0.72).contains(&u) && (0.25..=0.68).contains(&v) {
                    // Sky-blue landscape + mountain silhouette inside comic panel
                    let m_top = 0.46 + (u - 0.54).abs() * 0.75;
                    if v >= m_top {
                        r = 45.0;
                        g = 175.0;
                        b = 135.0;
                    } else {
                        r = 56.0;
                        g = 182.0;
                        b = 244.0;
                    }
                }
            }

            // Golden bookmark ribbon on the top-right of the book page: u in [0.56, 0.74], v in [0.14, 0.52]
            if (0.56..=0.74).contains(&u) && (0.14..=0.52).contains(&v) {
                let du = (u - 0.65).abs() / 0.09;
                let v_limit = 0.42 + du * 0.10;
                if v <= v_limit {
                    r = 255.0;
                    g = 196.0;
                    b = 40.0;
                }
            }

            bgra[idx] = b.clamp(0.0, 255.0).round() as u8;
            bgra[idx + 1] = g.clamp(0.0, 255.0).round() as u8;
            bgra[idx + 2] = r.clamp(0.0, 255.0).round() as u8;
            bgra[idx + 3] = (edge_aa * 255.0).round() as u8;
        }
    }

    bgra
}

#[inline]
fn sd_rounded_box(px: f32, py: f32, w: f32, h: f32, r: f32) -> f32 {
    let qx = (px - w * 0.5).abs() - (w * 0.5 - r);
    let qy = (py - h * 0.5).abs() - (h * 0.5 - r);
    let out_dist = (qx.max(0.0) * qx.max(0.0) + qy.max(0.0) * qy.max(0.0)).sqrt();
    let in_dist = qx.max(qy).min(0.0);
    out_dist + in_dist - r
}

/// Generates a multi-resolution Windows `.ico` file (`32x32` and `64x64` 32bpp ARGB DIBs).
pub fn generate_app_ico_bytes() -> Vec<u8> {
    let sizes: &[u32] = &[32, 64];
    let mut dibs: Vec<Vec<u8>> = Vec::with_capacity(sizes.len());

    for &sz in sizes {
        let top_down_bgra = render_app_icon_bgra(sz);
        let mut dib = Vec::new();
        // BITMAPINFOHEADER (40 bytes), height = sz * 2 (XOR + AND masks), bottom-up rows
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&(sz as i32).to_le_bytes());
        dib.extend_from_slice(&((sz * 2) as i32).to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes()); // planes
        dib.extend_from_slice(&32u16.to_le_bytes()); // 32 bpp
        dib.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
        dib.extend_from_slice(&(sz * sz * 4).to_le_bytes());
        dib.extend_from_slice(&0i32.to_le_bytes());
        dib.extend_from_slice(&0i32.to_le_bytes());
        dib.extend_from_slice(&0u32.to_le_bytes());
        dib.extend_from_slice(&0u32.to_le_bytes());

        // Bottom-up XOR BGRA rows
        let row_bytes = (sz as usize) * 4;
        for row in (0..sz as usize).rev() {
            let start = row * row_bytes;
            dib.extend_from_slice(&top_down_bgra[start..start + row_bytes]);
        }

        // 1bpp AND mask (DWORD-aligned rows, 0 = opaque, 1 = transparent)
        let mask_row_bytes = ((sz as usize + 31) / 32) * 4;
        let mut mask_row = vec![0u8; mask_row_bytes];
        for row in (0..sz as usize).rev() {
            mask_row.fill(0);
            for col in 0..sz as usize {
                let a = top_down_bgra[(row * sz as usize + col) * 4 + 3];
                if a == 0 {
                    mask_row[col / 8] |= 1 << (7 - (col % 8));
                }
            }
            dib.extend_from_slice(&mask_row);
        }

        dibs.push(dib);
    }

    let mut ico = Vec::new();
    // ICONDIR header (6 bytes)
    ico.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // type = 1 (ICON)
    ico.extend_from_slice(&(sizes.len() as u16).to_le_bytes());

    let mut offset = 6 + (sizes.len() * 16) as u32;
    for (i, &sz) in sizes.iter().enumerate() {
        let dib_len = dibs[i].len() as u32;
        ico.push(if sz >= 256 { 0 } else { sz as u8 }); // width
        ico.push(if sz >= 256 { 0 } else { sz as u8 }); // height
        ico.push(0); // color count
        ico.push(0); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bit count
        ico.extend_from_slice(&dib_len.to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += dib_len;
    }

    for dib in dibs {
        ico.extend_from_slice(&dib);
    }

    ico
}

/// Creates a Win32 `HICON` for window titlebars and taskbars.
pub fn create_app_hicon(size: u32) -> HICON {
    let bgra = render_app_icon_bgra(size);
    let Ok(hbm_color) = create_hbitmap_from_bgra(size, size, &bgra) else {
        return HICON::default();
    };
    let hbm_mask = unsafe { CreateBitmap(size as i32, size as i32, 1, 1, None) };
    let ii = ICONINFO {
        fIcon: windows::Win32::Foundation::BOOL(1),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: hbm_mask,
        hbmColor: hbm_color,
    };
    let hicon = unsafe { CreateIconIndirect(&ii).unwrap_or_default() };
    unsafe {
        if !hbm_mask.is_invalid() {
            let _ = DeleteObject(hbm_mask);
        }
        let _ = DeleteObject(hbm_color);
    }
    hicon
}
