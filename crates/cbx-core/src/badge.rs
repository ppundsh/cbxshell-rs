//! Anti-aliased silhouette corner badge renderer for thumbnails.
//!
//! Renders compact (50% reduced footprint), semi-transparent corner badges where the outer
//! silhouette itself directly represents the content category:
//! - **EPUB Badge (`BadgeIcon::Bookmark`)**: Swallowtail ribbon bookmark silhouette (`燕尾書籤外框`)
//!   with `"EPUB"` label.
//! - **Animated Image Badge (`BadgeIcon::Video`)**: Filmstrip frame silhouette with top & bottom
//!   punched sprocket holes (`電影膠卷外框`) and animated format label (`"WEBP"`, `"GIF"`, `"APNG"`, `"JXL"`).
//! - **Static Image Archive Badge (`BadgeIcon::StaticImage`)**: Dog-eared folded-corner file/photo
//!   frame silhouette (`右上折角圖檔外框`) with actual archive extension label (`"CBZ"`, `"ZIP"`,
//!   `"CBR"`, `"RAR"`, `"CB7"`, `"7Z"`, `"CBT"`, `"TAR"`).

#![forbid(unsafe_code)]

use image::{Rgba, RgbaImage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderDecoration {
    None = 0,
    Filmstrip = 1,  // 影片捲軸
    DropShadow = 2, // 柔和投影
    PhotoFrame = 3, // 現代相框
    BookCover = 4,  // 書籍封面
    Rounded = 5,    // 無外框圓弧
}

impl BorderDecoration {
    pub fn from_u32(val: u32) -> Self {
        match val {
            1 => Self::Filmstrip,
            2 => Self::DropShadow,
            3 => Self::PhotoFrame,
            4 => Self::BookCover,
            5 => Self::Rounded,
            _ => Self::None,
        }
    }
}

/// Applies one of the border decoration styles:
/// Filmstrip, DropShadow, PhotoFrame, BookCover, Rounded, or None.
pub fn apply_border_decoration(img: &RgbaImage, deco: BorderDecoration) -> RgbaImage {
    match deco {
        BorderDecoration::None => img.clone(),
        BorderDecoration::Filmstrip => apply_filmstrip_border(img),
        BorderDecoration::DropShadow => apply_drop_shadow_border(img),
        BorderDecoration::PhotoFrame => apply_photo_frame_border(img),
        BorderDecoration::BookCover => apply_book_cover_border(img),
        BorderDecoration::Rounded => apply_rounded_corners(img),
    }
}

/// Applies smooth anti-aliased rounded corners without any outer frame or shadow.
fn apply_rounded_corners(img: &RgbaImage) -> RgbaImage {
    let (w, h) = (img.width(), img.height());
    if w < 16 || h < 16 {
        return img.clone();
    }
    let min_side = w.min(h) as f32;
    let r = (min_side * 0.08).clamp(8.0, 20.0);
    let wf = w as f32;
    let hf = h as f32;
    let cx0 = r;
    let cx1 = wf - r;
    let cy0 = r;
    let cy1 = hf - r;

    let mut out = img.clone();
    for y in 0..h {
        let py = y as f32 + 0.5;
        let dy = if py < cy0 {
            cy0 - py
        } else if py > cy1 {
            py - cy1
        } else {
            0.0
        };
        if dy <= 0.0 {
            continue;
        }
        for x in 0..w {
            let px = x as f32 + 0.5;
            let dx = if px < cx0 {
                cx0 - px
            } else if px > cx1 {
                px - cx1
            } else {
                0.0
            };
            if dx <= 0.0 {
                continue;
            }
            let dist = (dx * dx + dy * dy).sqrt() - r;
            if dist > -0.5 {
                let cov = (0.5 - dist).clamp(0.0, 1.0);
                let p = out.get_pixel_mut(x, y);
                if cov <= 0.0 {
                    *p = Rgba([0, 0, 0, 0]);
                } else {
                    p.0[3] = ((p.0[3] as f32) * cov).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    out
}

/// Bilinear RGB sampler in normalized `[0, 1]` UV coordinates.
#[inline]
fn sample_bilinear_rgb(img: &RgbaImage, u: f32, v: f32) -> [f32; 3] {
    let w = img.width().max(1);
    let h = img.height().max(1);
    let max_sx = (w - 1) as f32;
    let max_sy = (h - 1) as f32;
    let sx = (u.clamp(0.0, 1.0) * max_sx).clamp(0.0, max_sx);
    let sy = (v.clamp(0.0, 1.0) * max_sy).clamp(0.0, max_sy);
    let x0 = sx.floor() as u32;
    let y0 = sy.floor() as u32;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = sx - x0 as f32;
    let fy = sy - y0 as f32;
    let p00 = img.get_pixel(x0, y0).0;
    let p10 = img.get_pixel(x1, y0).0;
    let p01 = img.get_pixel(x0, y1).0;
    let p11 = img.get_pixel(x1, y1).0;
    let mut out_rgb = [0.0f32; 3];
    for c in 0..3 {
        let top = p00[c] as f32 * (1.0 - fx) + p10[c] as f32 * fx;
        let bot = p01[c] as f32 * (1.0 - fx) + p11[c] as f32 * fx;
        out_rgb[c] = top * (1.0 - fy) + bot * fy;
    }
    out_rgb
}

/// Composites a foreground straight-RGB color and coverage (`0..1`) over a black transparent shadow
/// (`0..1`) returning straight-alpha `Rgba<u8>`.
#[inline]
fn composite_fg_over_black_shadow(fg_rgb: [f32; 3], fg_cov: f32, shadow_alpha: f32) -> Rgba<u8> {
    let out_a = (fg_cov + shadow_alpha * (1.0 - fg_cov)).clamp(0.0, 1.0);
    if out_a <= 0.002 {
        return Rgba([0, 0, 0, 0]);
    }
    // Since shadow RGB is (0, 0, 0), straight output color = fg_rgb * fg_cov / out_a.
    let weight = (fg_cov / out_a).clamp(0.0, 1.0);
    Rgba([
        (fg_rgb[0] * weight).round().clamp(0.0, 255.0) as u8,
        (fg_rgb[1] * weight).round().clamp(0.0, 255.0) as u8,
        (fg_rgb[2] * weight).round().clamp(0.0, 255.0) as u8,
        (out_a * 255.0).round().clamp(0.0, 255.0) as u8,
    ])
}

fn apply_filmstrip_border(img: &RgbaImage) -> RgbaImage {
    let (w, h) = (img.width(), img.height());
    if w < 16 || h < 16 {
        return img.clone();
    }

    // Classic Media Preview style with Windows 11 smooth rounded outer corners:
    // IMPORTANT: The resulting thumbnail MUST strictly fit within the target dimensions (<= 256px).
    // Never expand outer width beyond the maximum container bounds, otherwise Windows Explorer's
    // thumbnail caching pipeline renders the inner content completely black!
    let (new_w, new_h, band_w, content_w, content_h, offset_x) = if w >= h {
        // Landscape video (e.g. 256x145, 16:9):
        // Carve out left and right filmstrip bands from the total width so new_w never exceeds cx.
        let band = ((h as f32) * 0.095).round().clamp(10.0, 26.0) as u32;
        let c_w = w.saturating_sub(band * 2).max(1);
        (w, h, band, c_w, h, band)
    } else {
        // Portrait video (e.g. 131x256, 9:16):
        // Outer expansion is safe only if new_w <= h (the maximum side).
        let band = ((w as f32) * 0.12).round().clamp(8.0, 24.0) as u32;
        let expanded_w = w + band * 2;
        if expanded_w <= h {
            (expanded_w, h, band, w, h, band)
        } else {
            let c_w = h.saturating_sub(band * 2).max(1);
            (h, h, band, c_w, h, band)
        }
    };

    let nwf = new_w as f32;
    let nhf = new_h as f32;
    let min_side = nwf.min(nhf);
    let r_outer = (min_side * 0.068).clamp(7.0, 15.0);
    let r_inner = (r_outer * 0.42).clamp(2.5, 5.5);

    let band_xf = offset_x as f32;
    let content_end_xf = (offset_x + content_w) as f32;
    let content_wf = content_w as f32;
    let content_hf = content_h as f32;

    let mut out = RgbaImage::from_pixel(new_w, new_h, Rgba([0, 0, 0, 0]));

    // 1. Render rounded filmstrip body, slate gradient bands, 1px seam groove, inner artwork & outer rim
    for y in 0..new_h {
        let py = y as f32 + 0.5;
        let t_y = if new_h > 1 { y as f32 / (new_h - 1) as f32 } else { 0.0 };
        let band_light = (1.0 - t_y).powf(0.85);
        let band_rgb = [
            31.0 + (66.0 - 31.0) * band_light,
            33.0 + (70.0 - 33.0) * band_light,
            37.0 + (79.0 - 37.0) * band_light,
        ];

        for x in 0..new_w {
            let px = x as f32 + 0.5;
            let d_outer = rounded_rect_asym_sdf(px, py, 0.0, 0.0, nwf, nhf, r_outer, r_outer);
            let outer_cov = (0.5 - d_outer).clamp(0.0, 1.0);
            if outer_cov <= 0.002 {
                continue;
            }

            // Inner artwork bounds with soft inner corner rounding
            let d_inner = rounded_rect_asym_sdf(
                px,
                py,
                band_xf,
                0.0,
                content_end_xf,
                content_hf,
                r_inner,
                r_inner,
            );
            let inner_cov = (0.5 - d_inner).clamp(0.0, 1.0);

            // Filmstrip band color with subtle 1px vertical seam groove beside content
            let seam_factor = if (d_inner > 0.0 && d_inner < 1.25)
                && (x + 1 == offset_x || x == offset_x + content_w)
            {
                0.55
            } else {
                1.0
            };
            let bg_rgb = [
                band_rgb[0] * seam_factor,
                band_rgb[1] * seam_factor,
                band_rgb[2] * seam_factor,
            ];

            let mut pixel_rgb = if inner_cov > 0.002 {
                let u = (px - band_xf) / content_wf;
                let v = py / content_hf;
                let art_rgb = sample_bilinear_rgb(img, u, v);
                if inner_cov >= 0.999 {
                    art_rgb
                } else {
                    [
                        art_rgb[0] * inner_cov + bg_rgb[0] * (1.0 - inner_cov),
                        art_rgb[1] * inner_cov + bg_rgb[1] * (1.0 - inner_cov),
                        art_rgb[2] * inner_cov + bg_rgb[2] * (1.0 - inner_cov),
                    ]
                }
            } else {
                bg_rgb
            };

            // Subtle Win11 outer specular rim along the rounded filmstrip perimeter
            if d_outer > -1.3 && d_outer <= 0.5 {
                let rim_t = smoothstep(-1.3, -0.35, d_outer) * (1.0 - smoothstep(-0.2, 0.45, d_outer));
                let top_bias = if py < nhf * 0.45 { 1.0 } else { 0.45 };
                let lift = 24.0 * rim_t * top_bias;
                for c in 0..3 {
                    pixel_rgb[c] = (pixel_rgb[c] + lift).clamp(0.0, 255.0);
                }
            }

            out.put_pixel(
                x,
                y,
                Rgba([
                    pixel_rgb[0].round().clamp(0.0, 255.0) as u8,
                    pixel_rgb[1].round().clamp(0.0, 255.0) as u8,
                    pixel_rgb[2].round().clamp(0.0, 255.0) as u8,
                    (outer_cov * 255.0).round().clamp(0.0, 255.0) as u8,
                ]),
            );
        }
    }

    // 2. Draw crisp rounded rectangular sprocket holes inside safe vertical band margins
    let margin_y = (r_outer * 0.48).max(3.0);
    let usable_h = (nhf - margin_y * 2.0).max(20.0);
    let hole_count = ((usable_h / 28.0).round().clamp(3.0, 7.0)) as u32;
    let hole_w = ((band_w as f32) * 0.58).round().max(4.0);
    let hole_h = (hole_w * 0.85).round().max(4.0);
    let hole_r = (hole_w.min(hole_h) * 0.24).clamp(1.2, 3.0);
    let hole_x_left = (band_w as f32 - hole_w) * 0.5;
    let hole_x_right = content_end_xf + (band_w as f32 - hole_w) * 0.5;
    let segment_h = usable_h / hole_count as f32;

    for i in 0..hole_count {
        let center_y = margin_y + (i as f32 + 0.5) * segment_h;
        let top_y = (center_y - hole_h * 0.5).max(margin_y);
        let y_start = (top_y.floor() as u32).saturating_sub(1);
        let y_end = ((top_y + hole_h).ceil() as u32 + 1).min(new_h);

        for band_left in [hole_x_left, hole_x_right] {
            let x_start = (band_left.floor() as u32).saturating_sub(1);
            let x_end = ((band_left + hole_w).ceil() as u32 + 1).min(new_w);
            for y in y_start..y_end {
                let py = y as f32 + 0.5;
                for x in x_start..x_end {
                    let px = x as f32 + 0.5;
                    let d_outer = rounded_rect_asym_sdf(px, py, 0.0, 0.0, nwf, nhf, r_outer, r_outer);
                    if d_outer > -1.0 {
                        continue;
                    }
                    let d_hole = rounded_rect_asym_sdf(
                        px,
                        py,
                        band_left,
                        top_y,
                        band_left + hole_w,
                        top_y + hole_h,
                        hole_r,
                        hole_r,
                    );
                    let cov = (0.5 - d_hole).clamp(0.0, 1.0);
                    if cov <= 0.002 {
                        continue;
                    }
                    // Subtle inner lip shading for punched film perforation depth
                    let local_v = ((py - top_y) / hole_h).clamp(0.0, 1.0);
                    let hole_rgb = if local_v < 0.16 {
                        [196.0, 200.0, 210.0]
                    } else if local_v > 0.84 {
                        [255.0, 255.0, 255.0]
                    } else {
                        [245.0, 247.0, 252.0]
                    };
                    let base = out.get_pixel(x, y).0;
                    out.put_pixel(
                        x,
                        y,
                        Rgba([
                            (hole_rgb[0] * cov + base[0] as f32 * (1.0 - cov)).round() as u8,
                            (hole_rgb[1] * cov + base[1] as f32 * (1.0 - cov)).round() as u8,
                            (hole_rgb[2] * cov + base[2] as f32 * (1.0 - cov)).round() as u8,
                            base[3],
                        ]),
                    );
                }
            }
        }
    }

    out
}

/// Windows 11 Drop Shadow style: anti-aliased rounded corners, multi-stop soft elevation shadow,
/// and subtle ambient lighting / specular rim gradient.
fn apply_drop_shadow_border(img: &RgbaImage) -> RgbaImage {
    let (w, h) = (img.width(), img.height());
    if w < 20 || h < 20 {
        return img.clone();
    }

    let wf = w as f32;
    let hf = h as f32;
    let min_side = wf.min(hf);

    let pad_tl = (min_side * 0.022).round().clamp(3.0, 6.0);
    let pad_br = (min_side * 0.068).round().clamp(8.0, 17.0);
    let c_w = (wf - pad_tl - pad_br).max(16.0);
    let c_h = (hf - pad_tl - pad_br).max(16.0);
    let r = (c_w.min(c_h) * 0.065).clamp(7.0, 15.0);

    let shadow_dx = pad_br * 0.36;
    let shadow_dy = pad_br * 0.46;
    let shadow_blur = pad_br * 0.88;

    let mut out = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));

    for y in 0..h {
        let py = y as f32 + 0.5;
        for x in 0..w {
            let px = x as f32 + 0.5;
            let d_card = rounded_rect_asym_sdf(px, py, pad_tl, pad_tl, pad_tl + c_w, pad_tl + c_h, r, r);
            let card_cov = (0.5 - d_card).clamp(0.0, 1.0);

            // Soft directional + ambient elevation drop shadow
            let d_shadow = rounded_rect_asym_sdf(
                px - shadow_dx,
                py - shadow_dy,
                pad_tl,
                pad_tl,
                pad_tl + c_w,
                pad_tl + c_h,
                r,
                r,
            );
            let sh_dir = 1.0 - smoothstep(-2.0, shadow_blur, d_shadow);
            let sh_amb = 1.0 - smoothstep(-1.0, shadow_blur * 0.55, d_card);
            let shadow_alpha = ((sh_dir * sh_dir * 0.46) + (sh_amb * sh_amb * 0.14)).clamp(0.0, 0.54);

            if card_cov <= 0.002 && shadow_alpha <= 0.002 {
                continue;
            }

            let fg_rgb = if card_cov > 0.002 {
                let u = ((px - pad_tl) / c_w).clamp(0.0, 1.0);
                let v = ((py - pad_tl) / c_h).clamp(0.0, 1.0);
                let mut rgb = sample_bilinear_rgb(img, u, v);

                // Subtle Win11 top-to-bottom lighting depth
                let grad = 5.0 * (1.0 - v).powi(2) - 4.0 * v.powi(2);
                for c in 0..3 {
                    rgb[c] = (rgb[c] + grad).clamp(0.0, 255.0);
                }

                // Specular rim along rounded border: top-left catchlight, bottom-right subtle outline
                if d_card > -1.4 && d_card <= 0.5 {
                    let edge_t = smoothstep(-1.4, -0.35, d_card) * (1.0 - smoothstep(-0.2, 0.45, d_card));
                    let tl_bias = (1.0 - 0.5 * u - 0.5 * v).clamp(0.0, 1.0);
                    let highlight = 26.0 * tl_bias - 10.0 * (1.0 - tl_bias);
                    let rim_dark = smoothstep(-0.6, 0.3, d_card) * 0.14;
                    for c in 0..3 {
                        rgb[c] = ((rgb[c] + highlight * edge_t) * (1.0 - rim_dark)).clamp(0.0, 255.0);
                    }
                }
                rgb
            } else {
                [0.0, 0.0, 0.0]
            };

            out.put_pixel(x, y, composite_fg_over_black_shadow(fg_rgb, card_cov, shadow_alpha));
        }
    }

    out
}

/// Windows 11 Photo Frame style: soft outer elevation shadow, rounded outer frame with Pearl/Mica
/// lighting gradient, subtle outer outline, inner bevel, and smoothly rounded artwork opening.
fn apply_photo_frame_border(img: &RgbaImage) -> RgbaImage {
    let (w, h) = (img.width(), img.height());
    if w < 24 || h < 24 {
        return img.clone();
    }

    let wf = w as f32;
    let hf = h as f32;
    let min_side = wf.min(hf);

    let sh_tl = (min_side * 0.018).round().clamp(2.0, 5.0);
    let sh_br = (min_side * 0.056).round().clamp(6.0, 14.0);
    let frame_w = (wf - sh_tl - sh_br).max(20.0);
    let frame_h = (hf - sh_tl - sh_br).max(20.0);

    let frame_t = (min_side * 0.048).round().clamp(5.0, 12.0);
    let c_w = (frame_w - frame_t * 2.0).max(12.0);
    let c_h = (frame_h - frame_t * 2.0).max(12.0);

    let r_outer = (frame_w.min(frame_h) * 0.068).clamp(8.0, 16.0);
    let r_inner = (r_outer - frame_t * 0.55).clamp(4.0, 10.0);

    let art_left = sh_tl + frame_t;
    let art_top = sh_tl + frame_t;
    let art_right = art_left + c_w;
    let art_bottom = art_top + c_h;

    let shadow_dx = sh_br * 0.34;
    let shadow_dy = sh_br * 0.44;
    let shadow_blur = sh_br * 0.88;

    let mut out = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));

    for y in 0..h {
        let py = y as f32 + 0.5;
        for x in 0..w {
            let px = x as f32 + 0.5;
            let d_frame = rounded_rect_asym_sdf(
                px,
                py,
                sh_tl,
                sh_tl,
                sh_tl + frame_w,
                sh_tl + frame_h,
                r_outer,
                r_outer,
            );
            let frame_cov = (0.5 - d_frame).clamp(0.0, 1.0);

            let d_shadow = rounded_rect_asym_sdf(
                px - shadow_dx,
                py - shadow_dy,
                sh_tl,
                sh_tl,
                sh_tl + frame_w,
                sh_tl + frame_h,
                r_outer,
                r_outer,
            );
            let sh_dir = 1.0 - smoothstep(-2.0, shadow_blur, d_shadow);
            let sh_amb = 1.0 - smoothstep(-1.0, shadow_blur * 0.55, d_frame);
            let shadow_alpha = ((sh_dir * sh_dir * 0.44) + (sh_amb * sh_amb * 0.12)).clamp(0.0, 0.50);

            if frame_cov <= 0.002 && shadow_alpha <= 0.002 {
                continue;
            }

            let fg_rgb = if frame_cov > 0.002 {
                let d_art = rounded_rect_asym_sdf(px, py, art_left, art_top, art_right, art_bottom, r_inner, r_inner);
                let art_cov = (0.5 - d_art).clamp(0.0, 1.0);

                // Pearl / Mica diagonal-vertical mat gradient on the frame
                let fu = ((px - sh_tl) / frame_w).clamp(0.0, 1.0);
                let fv = ((py - sh_tl) / frame_h).clamp(0.0, 1.0);
                let grad_t = (fv * 0.72 + fu * 0.28).clamp(0.0, 1.0);
                let mut mat_rgb = [
                    252.0 + (224.0 - 252.0) * grad_t,
                    252.0 + (228.0 - 252.0) * grad_t,
                    254.0 + (236.0 - 254.0) * grad_t,
                ];

                // Inner recessed bevel shadow right around the rounded artwork opening
                if d_art > -0.5 && d_art < 2.2 {
                    let bev_t = 1.0 - smoothstep(0.0, 2.0, d_art.max(0.0));
                    let tl_in_shadow = ((1.0 - fu) * 0.55 + (1.0 - fv) * 0.45).clamp(0.0, 1.0);
                    let bevel_shade = -38.0 * tl_in_shadow - 14.0 * (1.0 - tl_in_shadow);
                    for c in 0..3 {
                        mat_rgb[c] = (mat_rgb[c] + bevel_shade * bev_t).clamp(0.0, 255.0);
                    }
                }

                // Crisp outer frame definition + top-left inner highlight
                if d_frame > -1.3 && d_frame <= 0.5 {
                    let edge_t = smoothstep(-1.3, -0.35, d_frame);
                    let outer_shade = [194.0f32, 199.0f32, 208.0f32];
                    let tl_lift = if fv < 0.4 { 8.0 } else { 0.0 };
                    for c in 0..3 {
                        mat_rgb[c] = (mat_rgb[c] * (1.0 - edge_t * 0.65) + outer_shade[c] * (edge_t * 0.65) + tl_lift)
                            .clamp(0.0, 255.0);
                    }
                }

                if art_cov > 0.002 {
                    let au = ((px - art_left) / c_w).clamp(0.0, 1.0);
                    let av = ((py - art_top) / c_h).clamp(0.0, 1.0);
                    let art_rgb = sample_bilinear_rgb(img, au, av);
                    if art_cov >= 0.999 {
                        art_rgb
                    } else {
                        [
                            art_rgb[0] * art_cov + mat_rgb[0] * (1.0 - art_cov),
                            art_rgb[1] * art_cov + mat_rgb[1] * (1.0 - art_cov),
                            art_rgb[2] * art_cov + mat_rgb[2] * (1.0 - art_cov),
                        ]
                    }
                } else {
                    mat_rgb
                }
            } else {
                [0.0, 0.0, 0.0]
            };

            out.put_pixel(x, y, composite_fg_over_black_shadow(fg_rgb, frame_cov, shadow_alpha));
        }
    }

    out
}

#[inline]
pub(crate) fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Signed distance to an axis-aligned rectangle with independent left and right corner radii.
#[inline]
pub(crate) fn rounded_rect_asym_sdf(
    px: f32,
    py: f32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    r_left: f32,
    r_right: f32,
) -> f32 {
    let cx = 0.5 * (left + right);
    let cy = 0.5 * (top + bottom);
    let hx = 0.5 * (right - left);
    let hy = 0.5 * (bottom - top);
    let r = if px < cx { r_left } else { r_right }.min(hx).min(hy).max(0.0);
    let qx = (px - cx).abs() - (hx - r);
    let qy = (py - cy).abs() - (hy - r);
    let out_dist = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let in_dist = qx.max(qy).min(0.0);
    out_dist + in_dist - r
}

/// Renders a Windows 11 Fluent hardcover book-cover border:
/// anti-aliased rounded corners, smooth multi-stop cylindrical spine lighting & crease gradient,
/// softly rounded fanned page-edge stack with ambient occlusion, and smooth drop shadow.
fn apply_book_cover_border(img: &RgbaImage) -> RgbaImage {
    let (w, h) = (img.width(), img.height());
    if w < 24 || h < 24 {
        return img.clone();
    }

    let wf = w as f32;
    let hf = h as f32;
    let min_side = wf.min(hf);

    let shadow_pad_r = (wf * 0.024).round().clamp(3.0, 6.0);
    let shadow_pad_b = (hf * 0.026).round().clamp(3.0, 7.0);
    let book_w = (wf - shadow_pad_r).max(16.0);
    let book_h = (hf - shadow_pad_b).max(16.0);

    let page_t = (min_side * 0.032).clamp(3.5, 8.0);
    let cover_w = (book_w - page_t).max(12.0);
    let cover_h = (book_h - page_t).max(12.0);
    let spine_w = (wf * 0.068).clamp(7.0, 17.0);

    // Win11 asymmetrical book corners: gentle spine radius + warmer page-side radius
    let r_book_left = (min_side * 0.022).clamp(3.0, 6.0);
    let r_book_right = (min_side * 0.055).clamp(6.0, 14.0);
    let r_cover_left = r_book_left;
    let r_cover_right = (r_book_right - page_t * 0.25).max(4.5);

    let shadow_dx = shadow_pad_r * 0.55;
    let shadow_dy = shadow_pad_b * 0.65;
    let shadow_blur = shadow_pad_b.max(shadow_pad_r) * 1.15;

    let max_sx = wf - 1.0;
    let max_sy = hf - 1.0;
    let mut out = RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 0]));

    // Bilinear sampler for crisp front cover resizing
    let sample_cover = |u: f32, v: f32| -> [f32; 3] {
        let sx = (u.clamp(0.0, 1.0) * max_sx).clamp(0.0, max_sx);
        let sy = (v.clamp(0.0, 1.0) * max_sy).clamp(0.0, max_sy);
        let x0 = sx.floor() as u32;
        let y0 = sy.floor() as u32;
        let x1 = (x0 + 1).min(w - 1);
        let y1 = (y0 + 1).min(h - 1);
        let fx = sx - x0 as f32;
        let fy = sy - y0 as f32;
        let p00 = img.get_pixel(x0, y0).0;
        let p10 = img.get_pixel(x1, y0).0;
        let p01 = img.get_pixel(x0, y1).0;
        let p11 = img.get_pixel(x1, y1).0;
        let mut out_rgb = [0.0f32; 3];
        for c in 0..3 {
            let top = p00[c] as f32 * (1.0 - fx) + p10[c] as f32 * fx;
            let bot = p01[c] as f32 * (1.0 - fx) + p11[c] as f32 * fx;
            out_rgb[c] = top * (1.0 - fy) + bot * fy;
        }
        out_rgb
    };

    for y in 0..h {
        let py = y as f32 + 0.5;
        for x in 0..w {
            let px = x as f32 + 0.5;

            let d_book = rounded_rect_asym_sdf(
                px,
                py,
                0.0,
                0.0,
                book_w,
                book_h,
                r_book_left,
                r_book_right,
            );
            let d_cover = rounded_rect_asym_sdf(
                px,
                py,
                0.0,
                0.0,
                cover_w,
                cover_h,
                r_cover_left,
                r_cover_right,
            );

            // Soft ambient drop shadow behind the book silhouette
            let d_shadow = rounded_rect_asym_sdf(
                px - shadow_dx,
                py - shadow_dy,
                0.0,
                0.0,
                book_w,
                book_h,
                r_book_left,
                r_book_right,
            );
            let shadow_t = 1.0 - smoothstep(-1.0, shadow_blur, d_shadow);
            let shadow_alpha = (shadow_t * shadow_t * 0.44).clamp(0.0, 0.48);

            // Book outer coverage (1.0 inside book, 0.0 outside)
            let book_cov = (0.5 - d_book).clamp(0.0, 1.0);
            if book_cov <= 0.001 && shadow_alpha <= 0.002 {
                continue;
            }

            // Compute page-stack pixel color (visible where d_cover > -0.5 and inside book)
            let depth_from_cover = d_cover.max(0.0);
            let depth_norm = (depth_from_cover / page_t.max(1.0)).clamp(0.0, 1.0);
            // Smooth warm paper stripe wave + soft under-cover ambient occlusion
            let wave = 0.5 + 0.5 * (depth_from_cover * std::f32::consts::PI * 1.15).cos();
            let ao = 0.72 + 0.28 * smoothstep(0.0, (page_t * 0.55).max(1.0), depth_from_cover);
            let outer_fade = 1.0 - 0.14 * depth_norm * depth_norm;
            let lum_mul = ao * outer_fade;
            let page_base = [
                (226.0 + 18.0 * wave) * lum_mul,
                (223.0 + 18.0 * wave) * lum_mul,
                (217.0 + 17.0 * wave) * lum_mul,
            ];
            // Soft outer back-cover rim at the very edge of the page stack
            let rim_mix = smoothstep(-1.6, -0.1, d_book);
            let rim_col = [56.0f32, 60.0f32, 68.0f32];
            let page_rgb = [
                page_base[0] * (1.0 - rim_mix) + rim_col[0] * rim_mix,
                page_base[1] * (1.0 - rim_mix) + rim_col[1] * rim_mix,
                page_base[2] * (1.0 - rim_mix) + rim_col[2] * rim_mix,
            ];

            // Compute front cover pixel color (cover artwork + smooth cylindrical spine & Fluent inner highlight)
            let cover_cov = (0.5 - d_cover).clamp(0.0, 1.0);
            let mut cover_rgb = if cover_cov > 0.001 {
                let mut rgb = sample_cover(px / cover_w, py / cover_h);

                // Smooth cylindrical spine + binding groove gradient on the left
                if px < spine_w + 1.5 {
                    let u = (px / spine_w).clamp(0.0, 1.0);
                    let spine_fade = 1.0 - smoothstep(spine_w - 1.0, spine_w + 1.5, px);
                    // Cylindrical shading curve:
                    //  0.00..0.18: subtle left bevel darkening (0.72 -> 1.0)
                    //  0.18..0.55: smooth rounded spine highlight (~1.16)
                    //  0.55..0.78: soft binding crease shadow (~0.60)
                    //  0.78..1.00: crease catchlight easing smoothly to 1.0
                    let mul = if u < 0.18 {
                        0.72 + 0.36 * smoothstep(0.0, 0.18, u)
                    } else if u < 0.52 {
                        1.08 + 0.08 * ((u - 0.18) / 0.34 * std::f32::consts::PI).sin()
                    } else if u < 0.76 {
                        1.08 - 0.46 * smoothstep(0.52, 0.74, u)
                    } else if u < 0.88 {
                        0.62 + 0.44 * smoothstep(0.76, 0.88, u)
                    } else {
                        1.06 - 0.06 * smoothstep(0.88, 1.0, u)
                    };
                    let lift = if u > 0.16 && u < 0.50 {
                        10.0 * ((u - 0.16) / 0.34 * std::f32::consts::PI).sin()
                    } else if u > 0.80 && u < 0.94 {
                        6.0 * ((u - 0.80) / 0.14 * std::f32::consts::PI).sin()
                    } else {
                        0.0
                    };
                    for c in 0..3 {
                        let shaded = (rgb[c] * mul + lift).clamp(0.0, 255.0);
                        rgb[c] = rgb[c] * (1.0 - spine_fade) + shaded * spine_fade;
                    }
                }

                // Subtle Win11 top/outer inner bevel highlight (top edge & right cover edge)
                if d_cover > -1.35 && d_cover <= 0.5 {
                    let edge_t = smoothstep(-1.35, -0.35, d_cover) * (1.0 - smoothstep(-0.2, 0.4, d_cover));
                    let top_bias = if py < cover_h * 0.45 { 1.0 } else { 0.45 };
                    let hl = 28.0 * edge_t * top_bias;
                    // Gentle dark separation outline right on the cover lip
                    let lip_dark = smoothstep(-0.7, 0.25, d_cover) * 0.22;
                    for c in 0..3 {
                        rgb[c] = ((rgb[c] + hl) * (1.0 - lip_dark)).clamp(0.0, 255.0);
                    }
                }
                rgb
            } else {
                [0.0, 0.0, 0.0]
            };

            // Front cover casts a tiny 1px soft contact tint over adjacent page stack
            let book_rgb = if cover_cov >= 0.999 {
                cover_rgb
            } else {
                // Blend front cover over page stack
                for c in 0..3 {
                    cover_rgb[c] = cover_rgb[c] * cover_cov + page_rgb[c] * (1.0 - cover_cov);
                }
                cover_rgb
            };

            // Composite book over soft drop shadow (alpha-premultiplied-safe dark outer fringe)
            let out_a = (book_cov + shadow_alpha * (1.0 - book_cov)).clamp(0.0, 1.0);
            if out_a <= 0.002 {
                continue;
            }
            let r = (book_rgb[0] * book_cov).round().clamp(0.0, 255.0) as u8;
            let g = (book_rgb[1] * book_cov).round().clamp(0.0, 255.0) as u8;
            let b = (book_rgb[2] * book_cov).round().clamp(0.0, 255.0) as u8;
            let a = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
            out.put_pixel(x, y, Rgba([r, g, b, a]));
        }
    }

    out
}

/// Renders a corner badge with a format string on top of `img`.
pub fn draw_media_corner_badge(img: &mut RgbaImage, label: &str, is_video: bool) {
    if label.is_empty() {
        return;
    }
    let spec = BadgeSpec {
        icon: if is_video {
            BadgeIcon::Video
        } else {
            BadgeIcon::Bookmark
        },
        label,
        is_animated: is_video,
    };
    draw_corner_badges(img, &[spec]);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeIcon {
    /// Swallowtail ribbon bookmark silhouette (`燕尾書籤外框`) for EPUB e-books.
    Bookmark,
    /// Filmstrip frame silhouette with sprocket perforations (`電影膠卷外框`) for animated images.
    Video,
    /// Dog-eared folded-corner image/archive silhouette (`右上折角圖檔外框`) for static image archives.
    StaticImage,
    /// Security alert badge / warning shield silhouette (`安全警告盾牌外框`) for detected archive anomalies.
    Risk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BadgeSpec<'a> {
    pub icon: BadgeIcon,
    pub label: &'a str,
    pub is_animated: bool,
}

/// Renders a security risk warning badge in the top-left corner of `img`.
pub fn draw_risk_warning_badge(img: &mut RgbaImage, label: &str) {
    let (img_w, img_h) = img.dimensions();
    if img_w < 24 || img_h < 24 {
        return;
    }
    let ref_dim = img_h.max(img_w);
    let scale: i32 = if ref_dim >= 160 { 2 } else { 1 };
    let margin: i32 = if scale >= 2 { 5 } else { 3 };
    let spec = BadgeSpec {
        icon: BadgeIcon::Risk,
        label,
        is_animated: false,
    };
    draw_badge_at(img, spec, margin, margin, scale);
}

/// Renders a single silhouette badge (or stacked badges) in the bottom-right corner of `img`.
pub fn draw_corner_badges(img: &mut RgbaImage, badges: &[BadgeSpec<'_>]) {
    if badges.is_empty() {
        return;
    }
    let (img_w, img_h) = img.dimensions();
    if img_w < 24 || img_h < 24 {
        return;
    }

    let ref_dim = img_h.max(img_w);
    let scale: i32 = if ref_dim >= 160 { 2 } else { 1 };
    let margin: i32 = if scale >= 2 { 5 } else { 3 };
    let v_gap: i32 = 2 * scale;

    let mut bottom_y = img_h as i32 - margin;
    let right_x = img_w as i32 - margin;

    for spec in badges {
        if spec.label.is_empty() {
            continue;
        }
        let bh = draw_single_silhouette_badge(img, *spec, right_x, bottom_y, scale);
        bottom_y -= bh + v_gap;
        if bottom_y < 8 {
            break;
        }
    }
}

fn draw_single_silhouette_badge(
    img: &mut RgbaImage,
    spec: BadgeSpec<'_>,
    right_x: i32,
    bottom_y: i32,
    scale: i32,
) -> i32 {
    let (bw, bh) = measure_badge_size(spec, scale);
    let bx = right_x - bw;
    let by = bottom_y - bh;
    draw_badge_at(img, spec, bx, by, scale);
    bh
}

pub fn measure_badge_size(spec: BadgeSpec<'_>, scale: i32) -> (i32, i32) {
    let char_count = spec.label.chars().take(8).count();
    let glyph_w = 3 * scale;
    let gap = scale;
    let text_w = (char_count as i32) * glyph_w + (char_count.saturating_sub(1) as i32) * gap;
    match spec.icon {
        BadgeIcon::Bookmark => {
            let pad = 6 * scale;
            (pad * 2 + text_w, 9 * scale)
        }
        BadgeIcon::Video => {
            let pad_x = 4 * scale;
            (pad_x * 2 + text_w, 10 * scale)
        }
        BadgeIcon::StaticImage => {
            let pad_l = 4 * scale;
            let pad_r = 5 * scale;
            (pad_l + text_w + pad_r, 9 * scale)
        }
        BadgeIcon::Risk => {
            let pad = 5 * scale;
            (pad * 2 + text_w, 9 * scale)
        }
    }
}

pub fn draw_badge_at(
    img: &mut RgbaImage,
    spec: BadgeSpec<'_>,
    bx: i32,
    by: i32,
    scale: i32,
) {
    let (img_w, img_h) = img.dimensions();
    let mut chars_buf = [' '; 8];
    let mut char_count = 0usize;
    for c in spec.label.chars().take(chars_buf.len()) {
        chars_buf[char_count] = c.to_ascii_uppercase();
        char_count += 1;
    }
    let chars = &chars_buf[..char_count];

    let glyph_w = 3 * scale;
    let glyph_h = 5 * scale;
    let gap = scale;
    let text_w = (char_count as i32) * glyph_w + (char_count.saturating_sub(1) as i32) * gap;

    let (bw, bh, bg_rgb, border_rgb, accent_rgb, text_x_off, text_y_off) = match spec.icon {
        BadgeIcon::Bookmark => {
            let pad_l = 6 * scale;
            let pad_r = 6 * scale;
            let h = 9 * scale; // 18 px on 256px thumbnail
            let w = pad_l + text_w + pad_r;
            (
                w,
                h,
                (38.0f32, 28.0f32, 14.0f32),
                (245.0f32, 190.0f32, 65.0f32),
                (245.0f32, 190.0f32, 65.0f32),
                pad_l - 1,
                (h - glyph_h) / 2,
            )
        }
        BadgeIcon::Video => {
            let pad_x = 4 * scale;
            let h = 10 * scale; // 20 px on 256px thumbnail (sprocket holes top & bottom)
            let w = pad_x * 2 + text_w;
            (
                w,
                h,
                (14.0f32, 30.0f32, 44.0f32),
                (90.0f32, 205.0f32, 250.0f32),
                (90.0f32, 205.0f32, 250.0f32),
                pad_x,
                (h - glyph_h) / 2,
            )
        }
        BadgeIcon::StaticImage => {
            let pad_l = 4 * scale;
            let pad_r = 5 * scale;
            let h = 9 * scale; // 18 px on 256px thumbnail
            let w = pad_l + text_w + pad_r;
            (
                w,
                h,
                (16.0f32, 36.0f32, 32.0f32),
                (85.0f32, 215.0f32, 165.0f32),
                (65.0f32, 175.0f32, 135.0f32),
                pad_l - 1,
                (h - glyph_h) / 2 + (scale / 2),
            )
        }
        BadgeIcon::Risk => {
            let pad = 5 * scale;
            let h = 9 * scale; // 18 px on 256px thumbnail
            let w = pad * 2 + text_w;
            (
                w,
                h,
                (185.0f32, 28.0f32, 28.0f32),
                (255.0f32, 85.0f32, 85.0f32),
                (255.0f32, 215.0f32, 60.0f32),
                pad - 1,
                (h - glyph_h) / 2,
            )
        }
    };

    // Semi-transparent alphas so background cover artwork shows through naturally
    let body_alpha = if spec.icon == BadgeIcon::Risk { 185.0f32 } else { 150.0f32 };
    let border_alpha = if spec.icon == BadgeIcon::Risk { 235.0f32 } else { 195.0f32 };
    let accent_alpha = if spec.icon == BadgeIcon::Risk { 220.0f32 } else { 185.0f32 };

    // 1. Render 4x4 anti-aliased outer silhouette
    for py in 0..bh {
        let dst_y = by + py;
        if dst_y < 0 || dst_y >= img_h as i32 {
            continue;
        }
        for px in 0..bw {
            let dst_x = bx + px;
            if dst_x < 0 || dst_x >= img_w as i32 {
                continue;
            }

            let mut b_hits = 0u32;
            let mut e_hits = 0u32;
            let mut a_hits = 0u32;

            for sy in 0..4 {
                let v = py as f32 + (sy as f32 + 0.5) * 0.25;
                for sx in 0..4 {
                    let u = px as f32 + (sx as f32 + 0.5) * 0.25;
                    let (inside, is_border, is_accent) =
                        eval_silhouette_shape(spec.icon, u, v, bw as f32, bh as f32, scale as f32);
                    if inside {
                        if is_border {
                            e_hits += 1;
                        } else if is_accent {
                            a_hits += 1;
                        } else {
                            b_hits += 1;
                        }
                    }
                }
            }

            let total = b_hits + e_hits + a_hits;
            if total == 0 {
                continue;
            }

            let total_f = total as f32;
            let cov = total_f / 16.0;
            let ew = e_hits as f32 / total_f;
            let aw = a_hits as f32 / total_f;
            let bw_w = b_hits as f32 / total_f;

            let r = border_rgb.0 * ew + accent_rgb.0 * aw + bg_rgb.0 * bw_w;
            let g = border_rgb.1 * ew + accent_rgb.1 * aw + bg_rgb.1 * bw_w;
            let b = border_rgb.2 * ew + accent_rgb.2 * aw + bg_rgb.2 * bw_w;
            let alpha = (border_alpha * ew + accent_alpha * aw + body_alpha * bw_w) * cov;

            blend_pixel(img, dst_x, dst_y, r, g, b, alpha);
        }
    }

    // 2. Render crisp 3x5 label text centered inside the silhouette (~88% opacity + soft shadow)
    let mut cur_x = bx + text_x_off;
    let cur_y = by + text_y_off;
    for &ch in chars {
        let rows = glyph_3x5(ch);
        for (r_i, &bits) in rows.iter().enumerate() {
            for c_i in 0..3 {
                if (bits >> (2 - c_i)) & 1 != 0 {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let tx = cur_x + (c_i as i32) * scale + dx;
                            let ty = cur_y + (r_i as i32) * scale + dy;
                            blend_pixel(img, tx + 1, ty + 1, 0.0, 0.0, 0.0, 110.0);
                            blend_pixel(img, tx, ty, 250.0, 252.0, 255.0, 224.0);
                        }
                    }
                }
            }
        }
        cur_x += glyph_w + gap;
    }
}

#[inline]
fn blend_pixel(img: &mut RgbaImage, x: i32, y: i32, r: f32, g: f32, b: f32, a: f32) {
    let (w, h) = img.dimensions();
    if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 || a <= 0.0 {
        return;
    }
    let dst = img.get_pixel_mut(x as u32, y as u32);
    let src_a = (a / 255.0).clamp(0.0, 1.0);
    let dst_r = dst.0[0] as f32;
    let dst_g = dst.0[1] as f32;
    let dst_b = dst.0[2] as f32;
    let dst_a = dst.0[3] as f32 / 255.0;

    let out_a = src_a + dst_a * (1.0 - src_a);
    if out_a > 1e-5 {
        let inv_a = 1.0 / out_a;
        let out_r = (r * src_a + dst_r * dst_a * (1.0 - src_a)) * inv_a;
        let out_g = (g * src_a + dst_g * dst_a * (1.0 - src_a)) * inv_a;
        let out_b = (b * src_a + dst_b * dst_a * (1.0 - src_a)) * inv_a;
        *dst = Rgba([
            out_r.clamp(0.0, 255.0).round() as u8,
            out_g.clamp(0.0, 255.0).round() as u8,
            out_b.clamp(0.0, 255.0).round() as u8,
            (out_a * 255.0).clamp(0.0, 255.0).round() as u8,
        ]);
    }
}

/// Evaluates `(inside_body, is_border, is_accent)` for the three silhouette shapes.
#[inline]
fn eval_silhouette_shape(
    icon: BadgeIcon,
    u: f32,
    v: f32,
    bw: f32,
    bh: f32,
    scale: f32,
) -> (bool, bool, bool) {
    let s_factor = (scale * 0.5).max(0.5);
    match icon {
        BadgeIcon::Bookmark => {
            // Swallowtail Ribbon Bookmark Silhouette:
            // Left spine fold + right V-notch cutout
            let notch_depth = bh * 0.34;
            let cy = bh * 0.5;
            let dy = (v - cy).abs() / cy;
            let right_edge = bw - notch_depth * (1.0 - dy);

            let r = 2.5 * s_factor;
            let mut in_left_corner = false;
            if u < r && v < r {
                in_left_corner = (u - r).hypot(v - r) > r;
            } else if u < r && v > bh - r {
                in_left_corner = (u - r).hypot(v - (bh - r)) > r;
            }

            let inside = (0.0..=right_edge).contains(&u)
                && (0.0..=bh).contains(&v)
                && !in_left_corner;
            if !inside {
                return (false, false, false);
            }

            let dist_right = (right_edge - u) * 0.78;
            let dist_edge = u.min(v).min(bh - v).min(dist_right);
            let is_border = dist_edge < 1.25 * s_factor;
            let is_accent = ((2.2 * s_factor)..=(3.6 * s_factor)).contains(&u)
                && ((2.5 * s_factor)..=(bh - 2.5 * s_factor)).contains(&v);
            (true, is_border, is_accent)
        }
        BadgeIcon::Video => {
            // Filmstrip Frame Silhouette:
            // Rounded frame with punched sprocket holes along the top & bottom edges
            if u < 0.0 || v < 0.0 || u > bw || v > bh {
                return (false, false, false);
            }
            let r = 2.5 * s_factor;
            if u < r && v < r && (u - r).hypot(v - r) > r {
                return (false, false, false);
            }
            if u > bw - r && v < r && (u - (bw - r)).hypot(v - r) > r {
                return (false, false, false);
            }
            if u < r && v > bh - r && (u - r).hypot(v - (bh - r)) > r {
                return (false, false, false);
            }
            if u > bw - r && v > bh - r && (u - (bw - r)).hypot(v - (bh - r)) > r {
                return (false, false, false);
            }

            let step = 5.5 * s_factor;
            let margin_x = 4.0 * s_factor;
            let hole_w = 2.6 * s_factor;
            let hole_h = 2.4 * s_factor;
            let mut dist_sprocket = 99.0f32;

            if u >= margin_x && u <= bw - margin_x {
                let local_x = (u - margin_x) % step;
                if local_x <= hole_w {
                    if v < hole_h || (bh - v) < hole_h {
                        return (false, false, false);
                    }
                    dist_sprocket = (v - hole_h).min((bh - hole_h) - v);
                }
            }

            let dist_outer = u.min(bw - u).min(v).min(bh - v).min(dist_sprocket);
            let is_border = dist_outer < 1.15 * s_factor;
            (true, is_border, false)
        }
        BadgeIcon::StaticImage => {
            // Dog-Eared Folded-Corner File/Photo Card Silhouette:
            // Top-right corner is cut at 45 degrees with a folded flap triangle
            if u < 0.0 || v < 0.0 || u > bw || v > bh {
                return (false, false, false);
            }
            let fold = bh * 0.42;
            if (u - (bw - fold)) > v {
                return (false, false, false);
            }

            let r = 2.2 * s_factor;
            if u < r && v < r && (u - r).hypot(v - r) > r {
                return (false, false, false);
            }
            if u < r && v > bh - r && (u - r).hypot(v - (bh - r)) > r {
                return (false, false, false);
            }
            if u > bw - r && v > bh - r && (u - (bw - r)).hypot(v - (bh - r)) > r {
                return (false, false, false);
            }

            let in_flap = u >= (bw - fold - 0.5 * s_factor) && v <= (fold + 0.5 * s_factor);
            let diag_dist = (v - (u - (bw - fold))) * std::f32::consts::FRAC_1_SQRT_2;
            let dist_outer = u.min(bw - u).min(v).min(bh - v).min(diag_dist);
            let is_border = dist_outer < 1.2 * s_factor
                || (in_flap
                    && ((u - (bw - fold)).abs() < 1.1 * s_factor
                        || (v - fold).abs() < 1.1 * s_factor));
            (true, is_border, in_flap && !is_border)
        }
        BadgeIcon::Risk => {
            // Hexagonal Alert Shield Silhouette
            if u < 0.0 || v < 0.0 || u > bw || v > bh {
                return (false, false, false);
            }
            let cut = 2.5 * s_factor;
            if (u + v < cut)
                || ((bw - u) + v < cut)
                || (u + (bh - v) < cut)
                || ((bw - u) + (bh - v) < cut)
            {
                return (false, false, false);
            }
            let diag1 = (u + v - cut) * std::f32::consts::FRAC_1_SQRT_2;
            let diag2 = ((bw - u) + v - cut) * std::f32::consts::FRAC_1_SQRT_2;
            let diag3 = (u + (bh - v) - cut) * std::f32::consts::FRAC_1_SQRT_2;
            let diag4 = ((bw - u) + (bh - v) - cut) * std::f32::consts::FRAC_1_SQRT_2;
            let dist_outer = u
                .min(bw - u)
                .min(v)
                .min(bh - v)
                .min(diag1)
                .min(diag2)
                .min(diag3)
                .min(diag4);
            let is_border = dist_outer < 1.25 * s_factor;
            let is_accent = is_border && (v < 2.0 * s_factor || (bh - v) < 2.0 * s_factor);
            (true, is_border, is_accent)
        }
    }
}

/// Compact 3x5 uppercase pixel font for miniature corner badges.
const fn glyph_3x5(ch: char) -> [u8; 5] {
    match ch {
        '!' => [0b010, 0b010, 0b010, 0b000, 0b010],
        'A' => [0b010, 0b101, 0b111, 0b101, 0b101],
        'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        'C' => [0b011, 0b100, 0b100, 0b100, 0b011],
        'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        'E' => [0b111, 0b100, 0b110, 0b100, 0b111],
        'F' => [0b111, 0b100, 0b110, 0b100, 0b100],
        'G' => [0b011, 0b100, 0b101, 0b101, 0b011],
        'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        'J' => [0b001, 0b001, 0b001, 0b101, 0b010],
        'K' => [0b101, 0b101, 0b110, 0b101, 0b101],
        'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        'N' => [0b101, 0b111, 0b111, 0b111, 0b101],
        'O' => [0b010, 0b101, 0b101, 0b101, 0b010],
        'P' => [0b110, 0b101, 0b110, 0b100, 0b100],
        'Q' => [0b010, 0b101, 0b101, 0b111, 0b011],
        'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        'S' => [0b011, 0b100, 0b010, 0b001, 0b110],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'U' => [0b101, 0b101, 0b101, 0b101, 0b111],
        'V' => [0b101, 0b101, 0b101, 0b101, 0b010],
        'W' => [0b101, 0b101, 0b111, 0b111, 0b101],
        'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
        'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        'Z' => [0b111, 0b001, 0b010, 0b100, 0b111],
        '0' => [0b010, 0b101, 0b101, 0b101, 0b010],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b110, 0b001, 0b010, 0b100, 0b111],
        '3' => [0b110, 0b001, 0b010, 0b001, 0b110],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b110, 0b001, 0b110],
        '6' => [0b011, 0b100, 0b110, 0b101, 0b010],
        '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
        '8' => [0b010, 0b101, 0b010, 0b101, 0b010],
        '9' => [0b010, 0b101, 0b011, 0b001, 0b110],
        _ => [0; 5],
    }
}
