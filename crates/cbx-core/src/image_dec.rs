//! 100% Pure-Rust image decoder, HDR tone mapper, and thumbnail generator.
//!
//! All image decoding, animation frame probing, HDR-to-SDR tone mapping, and badge rendering
//! are implemented in 100% safe Pure Rust without `vcpkg` or C/C++ codec dependencies.

use crate::badge::{draw_corner_badges, BadgeIcon, BadgeSpec};
use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::imageops::FilterType;
use image::{
    AnimationDecoder, DynamicImage, GenericImageView, ImageDecoder, ImageFormat, ImageReader,
    Limits, RgbaImage,
};
use image_webp::WebPDecoder;
use jxl_oxide::integration::JxlDecoder;
use std::io::{Cursor, Read};
use windows::Win32::Graphics::Gdi::{
    CreateDIBSection, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC,
};

/// Hard security cap on single dimension (width or height) to prevent row-stride integer overflows.
pub const MAX_IMAGE_DIMENSION: u32 = 16_384;
/// Hard security cap on total source pixels (64 MP = 256 MiB at RGBA8) to prevent pixel-flood bombs.
pub const MAX_SOURCE_PIXELS: u64 = 64_000_000;
/// Hard security cap on decoder memory allocation (256 MiB).
pub const MAX_DECODER_ALLOC_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum animated frames probed to skip initial blank/transparent fade-in frames.
const MAX_ANIM_PROBE_FRAMES: u32 = 12;

#[derive(Debug, Clone, Copy)]
pub struct DecodeOptions {
    pub max_side: u32,
    pub crop_tall: bool,
    pub is_epub: bool,
    pub badge_epub: bool,
    pub badge_animated: bool,
    pub badge_static: bool,
    /// Actual archive extension label for static image archives (`"CBZ"`, `"ZIP"`, `"CBR"`, `"RAR"`, `"CB7"`, `"7Z"`, `"CBT"`, `"TAR"`).
    pub archive_badge_label: &'static str,
    /// Whether an internal security anomaly was detected in the archive.
    pub has_risk: bool,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            max_side: 256,
            crop_tall: true,
            is_epub: false,
            badge_epub: true,
            badge_animated: true,
            badge_static: true,
            archive_badge_label: "ZIP",
            has_risk: false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DecodeInfo {
    pub src_width: u32,
    pub src_height: u32,
    pub out_width: u32,
    pub out_height: u32,
    pub format_name: &'static str,
    pub is_webp: bool,
    pub is_animated: bool,
    pub is_hdr: bool,
    pub cropped: bool,
    pub has_epub_badge: bool,
    pub has_animated_badge: bool,
    pub has_static_badge: bool,
    pub has_risk_badge: bool,
    pub badge_label: &'static str,
}

#[derive(Debug, Clone)]
pub struct DecodedThumbnail {
    /// Straight-alpha BGRA pixels, top-down (`out_width * out_height * 4` bytes).
    pub bgra: Vec<u8>,
    pub info: DecodeInfo,
}

pub fn is_webp_bytes(data: &[u8]) -> bool {
    data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP"
}

pub fn is_jxl_bytes(data: &[u8]) -> bool {
    // Naked JXL codestream (0xFF 0x0A) or ISOBMFF JXL container
    (data.len() >= 2 && data[0] == 0xFF && data[1] == 0x0A)
        || (data.len() >= 12 && &data[0..12] == b"\x00\x00\x00\x0CJXL \x0D\x0A\x87\x0A")
}

fn validate_dimensions(w: u32, h: u32, fmt: &str) -> Result<(), String> {
    if w == 0 || h == 0 {
        return Err(format!("{fmt}: zero dimension ({w}x{h})"));
    }
    if w > MAX_IMAGE_DIMENSION || h > MAX_IMAGE_DIMENSION {
        return Err(format!(
            "{fmt}: dimension ({w}x{h}) exceeds security limit ({MAX_IMAGE_DIMENSION}px)"
        ));
    }
    if (w as u64) * (h as u64) > MAX_SOURCE_PIXELS {
        return Err(format!(
            "{fmt}: pixel count ({w}x{h}) exceeds security limit ({MAX_SOURCE_PIXELS} pixels)"
        ));
    }
    Ok(())
}

fn strict_image_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODER_ALLOC_BYTES);
    limits
}

/// Decodes an arbitrary image buffer with the same strict dimension / allocation limits used by
/// the main thumbnail pipeline. Use this instead of `image::load_from_memory` (whose default
/// limits allow 512 MiB allocations and unbounded dimensions) for every untrusted image.
pub fn decode_bytes_strict(data: &[u8]) -> Result<DynamicImage, String> {
    let mut reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| format!("Image format probe failed: {e}"))?;
    reader.limits(strict_image_limits());
    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| format!("Image header rejected: {e}"))?;
    validate_dimensions(w, h, "image")?;
    let mut reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| format!("Image format probe failed: {e}"))?;
    reader.limits(strict_image_limits());
    reader.decode().map_err(|e| format!("Image decode failed: {e}"))
}

pub fn decode_to_thumbnail(
    data: &[u8],
    opt: DecodeOptions,
) -> Result<DecodedThumbnail, String> {
    if data.len() < 4 {
        return Err("Image buffer too small".into());
    }

    let (mut img, format_name, is_webp, is_animated, detected_hdr) = if is_webp_bytes(data) {
        let (im, anim) = decode_webp(data)?;
        (im, "WebP", true, anim, false)
    } else if is_jxl_bytes(data) {
        let (im, hdr) = decode_jxl(data)?;
        (im, "JPEG XL", false, false, hdr)
    } else {
        decode_standard_image(data)?
    };

    let is_hdr = detected_hdr
        || matches!(
            img,
            DynamicImage::ImageRgb32F(_)
                | DynamicImage::ImageRgba32F(_)
                | DynamicImage::ImageRgb16(_)
                | DynamicImage::ImageRgba16(_)
        );

    let (src_width, src_height) = img.dimensions();
    validate_dimensions(src_width, src_height, format_name)?;

    let mut cropped = false;
    // Webtoon / vertical strip handling: if height > 3 * width, crop to the top 2:3 region.
    if opt.crop_tall && (src_height as u64) > 3 * (src_width as u64) {
        let crop_h = ((src_width as u64) * 3 / 2).min(src_height as u64) as u32;
        img = img.crop_imm(0, 0, src_width, crop_h);
        cropped = true;
    }

    let (work_w, work_h) = img.dimensions();
    let (out_width, out_height) = compute_target_size(work_w, work_h, opt.max_side);

    // If the decoded image is floating-point HDR (Radiance .hdr, OpenEXR .exr, or 32-bit float JXL/TIFF),
    // apply ACES Filmic HDR-to-SDR tone mapping + sRGB OETF so highlights (> 1.0) are smoothly compressed.
    let mut rgba: RgbaImage =
        if matches!(img, DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_)) {
            let resized = if out_width != work_w || out_height != work_h {
                img.resize_exact(out_width, out_height, FilterType::CatmullRom)
            } else {
                img
            };
            tone_map_hdr_to_rgba8(&resized)
        } else if out_width != work_w || out_height != work_h {
            img.resize_exact(out_width, out_height, FilterType::CatmullRom)
                .to_rgba8()
        } else {
            img.to_rgba8()
        };

    // Determine mutually-exclusive corner badge based on enabled options:
    // 1. When `opt.is_epub` is true: ONLY `EPUB` bookmark silhouette badge (if `opt.badge_epub`), never JPG/ZIP/WEBP.
    // 2. Else when `is_animated` is true and `opt.badge_animated`: Filmstrip silhouette badge with animated format ("WEBP", "GIF", "APNG").
    // 3. Else when `opt.badge_static` is true: Dog-eared archive silhouette badge with archive extension ("CBZ", "ZIP", "CBR", "RAR", "CB7", "7Z", "CBT", "TAR").
    let format_badge_label = short_format_badge_label(format_name, is_webp);
    let static_archive_label = if !opt.archive_badge_label.is_empty() {
        opt.archive_badge_label
    } else {
        "ZIP"
    };

    let (has_epub_badge, has_animated_badge, has_static_badge, badge_label, badge_spec) =
        if opt.is_epub {
            if opt.badge_epub {
                (
                    true,
                    false,
                    false,
                    "EPUB",
                    Some(BadgeSpec {
                        icon: BadgeIcon::Bookmark,
                        label: "EPUB",
                        is_animated: false,
                    }),
                )
            } else {
                (false, false, false, "", None)
            }
        } else if is_animated && opt.badge_animated {
            (
                false,
                true,
                false,
                format_badge_label,
                Some(BadgeSpec {
                    icon: BadgeIcon::Video,
                    label: format_badge_label,
                    is_animated: true,
                }),
            )
        } else if opt.badge_static {
            (
                false,
                false,
                true,
                static_archive_label,
                Some(BadgeSpec {
                    icon: BadgeIcon::StaticImage,
                    label: static_archive_label,
                    is_animated: false,
                }),
            )
        } else {
            (false, false, false, "", None)
        };

    let has_overhang = detect_image_trailer_overhang(data).is_some();
    let effective_risk = opt.has_risk || has_overhang;

    if let Some(spec) = badge_spec {
        draw_corner_badges(&mut rgba, &[spec]);
    }

    if effective_risk {
        crate::badge::draw_risk_warning_badge(&mut rgba, "RISK");
    }

    let bgra = rgba_to_bgra(rgba.into_raw());

    Ok(DecodedThumbnail {
        bgra,
        info: DecodeInfo {
            src_width,
            src_height,
            out_width,
            out_height,
            format_name,
            is_webp,
            is_animated,
            is_hdr,
            cropped,
            has_epub_badge,
            has_animated_badge,
            has_static_badge,
            has_risk_badge: effective_risk,
            badge_label,
        },
    })
}

/// Detects extraneous polyglot payloads or appended data after the legal image EOF marker
/// (e.g. "圖種" where an archive, executable, or script is appended to JPEG/PNG/GIF/WebP).
pub fn detect_image_trailer_overhang(data: &[u8]) -> Option<&'static str> {
    if data.len() < 16 {
        return None;
    }

    // 1. PNG: Walk standard chunks until IEND (signature is 8B, chunk is 4B len + 4B type + data + 4B crc)
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        let mut offset = 8;
        while offset + 8 <= data.len() {
            let chunk_len = u32::from_be_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]) as usize;
            let chunk_type = &data[offset + 4..offset + 8];
            let chunk_end = offset.saturating_add(12).saturating_add(chunk_len);
            if chunk_type == b"IEND" {
                let trailer_len = data.len().saturating_sub(chunk_end.min(data.len()));
                if trailer_len > 32 {
                    return Some("Appended polyglot trailer payload after PNG IEND");
                }
                return None;
            }
            if chunk_end <= offset || chunk_end > data.len() {
                break;
            }
            offset = chunk_end;
        }
    }

    // 2. JPEG: Parse markers to find the true EOI (FF D9) after SOS (FF DA)
    if data.starts_with(b"\xFF\xD8") {
        if let Some(eoi_pos) = find_jpeg_eoi(data) {
            let trailer_len = data.len().saturating_sub(eoi_pos + 2);
            if trailer_len > 32 {
                return Some("Appended polyglot trailer payload after JPEG EOI");
            }
        }
    }

    // 3. GIF: Walk blocks until 0x3B trailer
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        if let Some(pos) = find_gif_trailer(data) {
            let trailer_len = data.len().saturating_sub(pos + 1);
            if trailer_len > 32 {
                return Some("Appended polyglot trailer payload after GIF trailer");
            }
        }
    }

    // 4. WebP: RIFF container length at offset 4..8
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        let riff_len = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let expected_total = 8usize.saturating_add(riff_len);
        if data.len() > expected_total.saturating_add(32) {
            return Some("Appended polyglot trailer payload after WebP RIFF chunk");
        }
    }

    None
}

fn find_jpeg_eoi(data: &[u8]) -> Option<usize> {
    let mut offset = 2;
    while offset + 4 <= data.len() {
        if data[offset] != 0xFF {
            offset += 1;
            continue;
        }
        let marker = data[offset + 1];
        if marker == 0xD9 {
            return Some(offset);
        }
        if marker == 0xDA {
            // Start of scan: entropy data follows until unstuffed FF D9
            let mut scan = offset + 2;
            while scan + 1 < data.len() {
                if data[scan] == 0xFF && data[scan + 1] == 0xD9 {
                    return Some(scan);
                }
                scan += 1;
            }
            return None;
        }
        if (0xE0..=0xEF).contains(&marker)
            || marker == 0xDB
            || marker == 0xC4
            || marker == 0xC0
            || marker == 0xC2
            || marker == 0xFE
        {
            let len = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize;
            offset += 2 + len;
        } else {
            offset += 2;
        }
    }
    // Fallback: search for last FF D9
    data.windows(2).rposition(|w| w == b"\xFF\xD9")
}

fn find_gif_trailer(data: &[u8]) -> Option<usize> {
    if data.len() < 13 {
        return None;
    }
    let gct_flag = (data[10] & 0x80) != 0;
    let gct_size = if gct_flag {
        3 * (1 << ((data[10] & 0x07) + 1))
    } else {
        0
    };
    let mut offset = 13 + gct_size;
    while offset < data.len() {
        match data[offset] {
            0x3B => return Some(offset),
            0x21 => {
                offset += 2;
                while offset < data.len() {
                    let block_len = data[offset] as usize;
                    offset += 1;
                    if block_len == 0 {
                        break;
                    }
                    offset += block_len;
                }
            }
            0x2C => {
                if offset + 10 > data.len() {
                    return None;
                }
                let lct_flag = (data[offset + 9] & 0x80) != 0;
                let lct_size = if lct_flag {
                    3 * (1 << ((data[offset + 9] & 0x07) + 1))
                } else {
                    0
                };
                offset += 10 + lct_size + 1; // +1 for LZW min code size
                while offset < data.len() {
                    let block_len = data[offset] as usize;
                    offset += 1;
                    if block_len == 0 {
                        break;
                    }
                    offset += block_len;
                }
            }
            _ => break,
        }
    }
    // Fallback: last 0x3B
    data.iter().rposition(|&b| b == 0x3B)
}

fn short_format_badge_label(format_name: &'static str, is_webp: bool) -> &'static str {
    if is_webp || format_name == "WebP" {
        return "WEBP";
    }
    match format_name {
        "JPEG" => "JPG",
        "PNG" => "PNG",
        "APNG" => "APNG",
        "GIF" => "GIF",
        "JPEG XL" => "JXL",
        "Radiance HDR" => "HDR",
        "OpenEXR HDR" => "EXR",
        "TIFF" => "TIFF",
        "QOI" => "QOI",
        "BMP" => "BMP",
        "ICO" => "ICO",
        "DDS" => "DDS",
        "TGA" => "TGA",
        "PNM" => "PNM",
        "Farbfeld" => "FF",
        _ => "IMG",
    }
}

fn compute_target_size(work_w: u32, work_h: u32, max_side: u32) -> (u32, u32) {
    let max_s = max_side.max(1);
    let scale = (max_s as f64 / work_w as f64)
        .min(max_s as f64 / work_h as f64)
        .min(1.0);
    let out_w = ((work_w as f64 * scale).round() as u32).max(1);
    let out_h = ((work_h as f64 * scale).round() as u32).max(1);
    (out_w, out_h)
}

fn rgba_to_bgra(mut rgba: Vec<u8>) -> Vec<u8> {
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    rgba
}

fn decode_standard_image(
    data: &[u8],
) -> Result<(DynamicImage, &'static str, bool, bool, bool), String> {
    let mut reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    if reader.format().is_none() {
        // TGA lacks a leading magic number; try TGA when magic-byte sniffing returns None.
        reader.set_format(ImageFormat::Tga);
    }
    let fmt = reader.format();
    let fmt_name = match fmt {
        Some(ImageFormat::Jpeg) => "JPEG",
        Some(ImageFormat::Png) => "PNG",
        Some(ImageFormat::Gif) => "GIF",
        Some(ImageFormat::WebP) => "WebP",
        Some(ImageFormat::Tiff) => "TIFF",
        Some(ImageFormat::Bmp) => "BMP",
        Some(ImageFormat::Ico) => "ICO",
        Some(ImageFormat::Hdr) => "Radiance HDR",
        Some(ImageFormat::OpenExr) => "OpenEXR HDR",
        Some(ImageFormat::Qoi) => "QOI",
        Some(ImageFormat::Dds) => "DDS",
        Some(ImageFormat::Tga) => "TGA",
        Some(ImageFormat::Pnm) => "PNM",
        Some(ImageFormat::Farbfeld) => "Farbfeld",
        _ => "Image",
    };

    let (w, h) = reader.into_dimensions().map_err(|e| e.to_string())?;
    validate_dimensions(w, h, fmt_name)?;

    // Special handling for GIF (detect multi-frame animation & skip blank fade-in frame)
    if fmt == Some(ImageFormat::Gif) {
        if let Ok((gif_img, is_anim)) = decode_gif(data, w, h) {
            return Ok((gif_img, "GIF", false, is_anim, false));
        }
    }

    // Special handling for PNG / APNG (detect APNG animation & skip blank fade-in frame)
    if fmt == Some(ImageFormat::Png) {
        if let Ok(Some(apng_img)) = decode_apng_if_animated(data, w, h) {
            return Ok((apng_img, "APNG", false, true, false));
        }
    }

    let mut reader2 = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    if reader2.format().is_none() {
        reader2.set_format(ImageFormat::Tga);
    }
    reader2.limits(strict_image_limits());
    let decoded = reader2.decode().map_err(|e| e.to_string())?;
    let is_hdr = matches!(
        fmt,
        Some(ImageFormat::Hdr) | Some(ImageFormat::OpenExr)
    ) || matches!(
        decoded,
        DynamicImage::ImageRgb32F(_) | DynamicImage::ImageRgba32F(_)
    );
    Ok((decoded, fmt_name, fmt == Some(ImageFormat::WebP), false, is_hdr))
}

fn decode_gif(data: &[u8], w: u32, h: u32) -> Result<(DynamicImage, bool), String> {
    validate_dimensions(w, h, "GIF")?;
    let mut dec = GifDecoder::new(Cursor::new(data)).map_err(|e| format!("GIF error: {e}"))?;
    dec.set_limits(strict_image_limits())
        .map_err(|e| format!("GIF limit error: {e}"))?;

    let mut frames = dec.into_frames();
    let mut chosen: Option<RgbaImage> = None;
    let mut frame_count = 0u32;

    for _ in 0..MAX_ANIM_PROBE_FRAMES {
        match frames.next() {
            Some(Ok(frame)) => {
                frame_count += 1;
                let buf = frame.into_buffer();
                let non_blank = !is_blank_frame(buf.as_raw(), 4);
                if chosen.is_none() || non_blank {
                    chosen = Some(buf);
                }
                if non_blank && frame_count >= 2 {
                    break;
                }
            }
            Some(Err(_)) | None => break,
        }
    }

    // If we stopped at frame 1 because it was already non-blank, peek one more frame to know if it's animated
    if frame_count == 1 {
        if let Some(Ok(_)) = frames.next() {
            frame_count += 1;
        }
    }

    let img = chosen.ok_or_else(|| "GIF contained no readable frames".to_string())?;
    Ok((DynamicImage::ImageRgba8(img), frame_count > 1))
}

fn decode_apng_if_animated(
    data: &[u8],
    w: u32,
    h: u32,
) -> Result<Option<DynamicImage>, String> {
    validate_dimensions(w, h, "APNG")?;
    let dec = PngDecoder::with_limits(Cursor::new(data), strict_image_limits())
        .map_err(|e| format!("PNG error: {e}"))?;
    let is_apng = dec.is_apng().unwrap_or(false);
    if !is_apng {
        return Ok(None);
    }
    let apng = dec.apng().map_err(|e| format!("APNG error: {e}"))?;
    let mut frames = apng.into_frames();
    let mut chosen: Option<RgbaImage> = None;

    for _ in 0..MAX_ANIM_PROBE_FRAMES {
        match frames.next() {
            Some(Ok(frame)) => {
                let buf = frame.into_buffer();
                let non_blank = !is_blank_frame(buf.as_raw(), 4);
                chosen = Some(buf);
                if non_blank {
                    break;
                }
            }
            Some(Err(_)) | None => break,
        }
    }

    Ok(chosen.map(DynamicImage::ImageRgba8))
}

fn decode_jxl(data: &[u8]) -> Result<(DynamicImage, bool), String> {
    // JXL encoders (like cjxl) may omit trailing zero bytes of the final TOC group at EOF.
    // Chaining 128 zero bytes for naked codestreams ensures the final TOC group completes cleanly.
    let trailing_zeros = [0u8; 128];
    let pad_len = if data.starts_with(&[0xFF, 0x0A]) {
        trailing_zeros.len()
    } else {
        0
    };
    let reader = Cursor::new(data).chain(Cursor::new(&trailing_zeros[..pad_len]));
    let mut decoder = JxlDecoder::new(reader).map_err(|e| format!("JPEG XL error: {e}"))?;
    let (w, h) = decoder.dimensions();
    validate_dimensions(w, h, "JPEG XL")?;
    decoder
        .set_limits(strict_image_limits())
        .map_err(|e| format!("JPEG XL limit error: {e}"))?;
    let is_hdr = matches!(
        decoder.color_type(),
        image::ColorType::Rgb32F
            | image::ColorType::Rgba32F
            | image::ColorType::Rgb16
            | image::ColorType::Rgba16
    );
    let img =
        DynamicImage::from_decoder(decoder).map_err(|e| format!("JPEG XL decode failed: {e}"))?;
    Ok((img, is_hdr))
}

fn decode_webp(data: &[u8]) -> Result<(DynamicImage, bool), String> {
    let mut dec = WebPDecoder::new(Cursor::new(data)).map_err(|e| format!("WebP error: {e}"))?;
    let (w, h) = dec.dimensions();
    validate_dimensions(w, h, "WebP")?;

    let is_anim = dec.is_animated();
    let has_alpha = dec.has_alpha();
    let bpp: usize = if has_alpha { 4 } else { 3 };
    let buf_size = dec
        .output_buffer_size()
        .ok_or_else(|| "WebP output buffer size overflow".to_string())?;
    if (buf_size as u64) > MAX_DECODER_ALLOC_BYTES {
        return Err("WebP output buffer exceeds allocation limit".into());
    }

    if !is_anim {
        let mut buf = vec![0u8; buf_size];
        dec.read_image(&mut buf)
            .map_err(|e| format!("WebP decode failed: {e}"))?;
        let img = raw_webp_to_dynamic(w, h, bpp, buf)?;
        return Ok((img, false));
    }

    // Animated WebP: `image-webp` composites frames onto the canvas across `read_frame` calls.
    // Probe up to MAX_ANIM_PROBE_FRAMES and stop at the first frame that isn't blank/transparent.
    let num_frames = dec.num_frames().clamp(1, MAX_ANIM_PROBE_FRAMES);
    let mut frame_buf = vec![0u8; buf_size];
    let mut chosen: Option<Vec<u8>> = None;

    for _ in 0..num_frames {
        match dec.read_frame(&mut frame_buf) {
            Ok(_duration_ms) => {
                chosen = Some(frame_buf.clone());
                if !is_blank_frame(&frame_buf, bpp) {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let bytes = chosen.ok_or_else(|| "Animated WebP contained no readable frames".to_string())?;
    let img = raw_webp_to_dynamic(w, h, bpp, bytes)?;
    Ok((img, true))
}

fn raw_webp_to_dynamic(w: u32, h: u32, bpp: usize, buf: Vec<u8>) -> Result<DynamicImage, String> {
    if bpp == 4 {
        RgbaImage::from_raw(w, h, buf)
            .map(DynamicImage::ImageRgba8)
            .ok_or_else(|| "Failed to construct RgbaImage".into())
    } else {
        image::RgbImage::from_raw(w, h, buf)
            .map(DynamicImage::ImageRgb8)
            .ok_or_else(|| "Failed to construct RgbImage".into())
    }
}

fn is_blank_frame(buf: &[u8], bpp: usize) -> bool {
    let total_px = buf.len() / bpp;
    if total_px == 0 {
        return true;
    }
    let step = (total_px / 16384).max(1);
    let first = &buf[0..bpp];
    let mut all_transparent = bpp == 4;
    let mut all_same = true;

    let mut idx = 0;
    while idx < total_px {
        let px = &buf[idx * bpp..(idx + 1) * bpp];
        if bpp == 4 && px[3] != 0 {
            all_transparent = false;
        }
        if px != first {
            all_same = false;
        }
        if !all_transparent && !all_same {
            return false;
        }
        idx += step;
    }
    true
}

// ---------------------------------------------------------------------------
// ACES Filmic HDR-to-SDR Tone Mapping + IEC 61966-2-1 Linear-to-sRGB OETF
// ---------------------------------------------------------------------------

#[inline]
fn sanitize_hdr_sample(v: f32) -> f32 {
    if v.is_finite() && v > 0.0 {
        v
    } else {
        0.0
    }
}

#[inline]
fn aces_filmic(x: f32) -> f32 {
    // Narkowicz 2015 ACES Filmic Tone Mapping Curve
    let a = 2.51f32;
    let b = 0.03f32;
    let c = 2.43f32;
    let d = 0.59f32;
    let e = 0.14f32;
    ((x * (a * x + b)) / (x * (c * x + d) + e)).clamp(0.0, 1.0)
}

#[inline]
fn linear_to_srgb_u8(v: f32) -> u8 {
    // IEC 61966-2-1 standard sRGB transfer function
    let c = v.clamp(0.0, 1.0);
    let srgb = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (srgb.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Converts a linear floating-point HDR image (`ImageRgb32F` or `ImageRgba32F`) into an
/// 8-bit sRGB `RgbaImage` using automatic log-average luminance exposure + ACES Filmic tone mapping.
pub fn tone_map_hdr_to_rgba8(img: &DynamicImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    let rgba32 = img.to_rgba32f();
    let raw = rgba32.as_raw();

    // Pass 1: compute log-average luminance (Reinhard key estimation) & max luminance
    let mut log_lum_sum = 0.0f64;
    let mut lum_count = 0usize;
    let mut max_lum = 0.0f32;

    for px in raw.chunks_exact(4) {
        let r = sanitize_hdr_sample(px[0]);
        let g = sanitize_hdr_sample(px[1]);
        let b = sanitize_hdr_sample(px[2]);
        // Rec. 709 / sRGB luminance
        let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        if lum > max_lum {
            max_lum = lum;
        }
        if lum > 1e-4 {
            log_lum_sum += (lum as f64).ln();
            lum_count += 1;
        }
    }

    // Determine exposure scale: if peak luminance > 1.0 (true scene-referred HDR),
    // map geometric mean luminance toward mid-grey (0.18), clamped to a sensible range.
    let exposure = if max_lum > 1.05 && lum_count > 0 {
        let avg_lum = (log_lum_sum / lum_count as f64).exp() as f32;
        (0.18 / avg_lum.max(1e-4)).clamp(0.05, 10.0)
    } else {
        1.0
    };

    // Pass 2: apply exposure + ACES Filmic curve + linear-to-sRGB OETF
    let mut out = Vec::with_capacity((w as usize) * (h as usize) * 4);
    for px in raw.chunks_exact(4) {
        let r = aces_filmic(sanitize_hdr_sample(px[0]) * exposure);
        let g = aces_filmic(sanitize_hdr_sample(px[1]) * exposure);
        let b = aces_filmic(sanitize_hdr_sample(px[2]) * exposure);
        let a_f = if px[3].is_finite() {
            px[3].clamp(0.0, 1.0)
        } else {
            1.0
        };
        out.push(linear_to_srgb_u8(r));
        out.push(linear_to_srgb_u8(g));
        out.push(linear_to_srgb_u8(b));
        out.push((a_f * 255.0 + 0.5) as u8);
    }

    RgbaImage::from_raw(w, h, out).unwrap_or_else(|| RgbaImage::new(w, h))
}

/// Creates a top-down 32bpp BGRA `HBITMAP` suitable for `IThumbnailProvider::GetThumbnail`.
pub fn create_hbitmap_from_bgra(width: u32, height: u32, bgra: &[u8]) -> Result<HBITMAP, String> {
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| "Bitmap size overflow".to_string())?;
    if bgra.len() < expected {
        return Err("BGRA buffer shorter than expected".into());
    }

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32), // negative = top-down DIB
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: bmi is initialized and bits receives the section pointer.
    let hbmp = unsafe {
        CreateDIBSection(
            HDC::default(),
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            None,
            0,
        )
    }
    .map_err(|e| e.to_string())?;

    if hbmp.is_invalid() || bits.is_null() {
        return Err("CreateDIBSection returned null".into());
    }

    // SAFETY: CreateDIBSection allocated `expected` bytes at `bits`.
    unsafe {
        std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits.cast::<u8>(), expected);
    }

    Ok(hbmp)
}
