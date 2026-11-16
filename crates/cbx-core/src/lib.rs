pub mod archive;
pub mod badge;
pub mod encoding;
pub mod epub;
pub mod i18n;
pub mod icon;
pub mod image_dec;
pub mod ebook;
pub mod media;
pub mod other_doc;
pub mod redact;
pub mod monitor;
pub mod rar_pure;
pub mod registry;
pub mod settings;

#[cfg(test)]
mod adversarial_tests;

use archive::{archive_badge_label, count_entries, find_cover, CoverOptions};
use image_dec::{create_hbitmap_from_bgra, decode_to_thumbnail, DecodeInfo, DecodeOptions};
use settings::{find_file_type_by_ext, Settings};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;
use windows::Win32::Graphics::Gdi::HBITMAP;
use windows::Win32::System::Com::{IStream, STREAM_SEEK_CUR, STREAM_SEEK_END, STREAM_SEEK_SET};

const MAX_DECODE_ATTEMPTS: usize = 10;

#[derive(Debug, Clone)]
pub struct ThumbnailReport {
    pub entry_name: String,
    pub decode: DecodeInfo,
    pub attempts: usize,
    pub from_epub_metadata: bool,
    pub is_epub: bool,
    pub has_risk: bool,
}

/// Generates a thumbnail (`DecodedThumbnail` + `ThumbnailReport`) from any `Read + Seek` archive stream.
pub fn generate_thumbnail_bytes<R: Read + Seek>(
    reader: &mut R,
    cx: u32,
    settings: &Settings,
) -> Result<(Vec<u8>, ThumbnailReport), String> {
    generate_thumbnail_bytes_with_ext(reader, cx, settings, None)
}

/// Generates a thumbnail (`DecodedThumbnail` + `ThumbnailReport`) from any `Read + Seek` archive stream,
/// using `ext_or_name` (if provided, e.g. `"comic.cbz"` or `".cbz"`) to display the exact archive extension
/// (`CBZ`, `ZIP`, `CBR`, `RAR`, `CB7`, `7Z`, `CBT`, `TAR`, `EPUB`) on the corner badge.
pub fn generate_thumbnail_bytes_with_ext<R: Read + Seek>(
    reader: &mut R,
    cx: u32,
    settings: &Settings,
    ext_or_name: Option<&str>,
) -> Result<(Vec<u8>, ThumbnailReport), String> {
    let cover_opt = CoverOptions {
        sort: settings.sort_alphabetically,
        prefer_continuous_sequence: settings.prefer_continuous_sequence,
        max_image_bytes: settings.max_image_bytes(),
        scan_timeout: Duration::from_secs(settings.max_scan_seconds as u64),
        anomaly_detection: settings.anomaly_detection && settings.badge_risk,
        risk_rules: settings.risk_rules(),
    };

    let is_ext_epub = ext_or_name
        .map(|s| {
            let l = s.trim().to_ascii_lowercase();
            l == "epub" || l.ends_with(".epub")
        })
        .unwrap_or(false);

    let mut exclude: Vec<String> = Vec::new();
    let mut last_err = String::from("No cover candidate found");

    for attempt in 1..=MAX_DECODE_ATTEMPTS {
        let cand = match find_cover(reader, &cover_opt, &exclude) {
            Ok(c) => c,
            Err(e) => {
                return Err(if attempt == 1 { e } else { last_err });
            }
        };
        let is_epub = cand.is_epub || is_ext_epub;
        let arc_label = archive_badge_label(ext_or_name, cand.container, is_epub);
        let dec_opt = DecodeOptions {
            max_side: cx,
            crop_tall: settings.crop_tall_strips,
            is_epub,
            badge_epub: settings.badge_epub,
            badge_animated: settings.badge_animated,
            badge_static: settings.badge_static,
            archive_badge_label: arc_label,
            has_risk: cand.has_risk && settings.badge_risk,
        };
        match decode_to_thumbnail(&cand.data, dec_opt) {
            Ok(decoded) => {
                if settings.filter_plain_covers && media::smart_render::is_image_too_simple(&decoded.bgra, true) {
                    last_err = format!("Candidate '{}' was skipped because it is plain or monochromatic", cand.name);
                    exclude.push(cand.name);
                    continue;
                }
                let is_risk = cand.has_risk || decoded.info.has_risk_badge;
                let report = ThumbnailReport {
                    entry_name: cand.name,
                    decode: decoded.info,
                    attempts: attempt,
                    from_epub_metadata: cand.from_epub_metadata,
                    is_epub,
                    has_risk: is_risk,
                };
                return Ok((decoded.bgra, report));
            }
            Err(e) => {
                last_err = format!("Decode failed for '{}': {e}", cand.name);
                exclude.push(cand.name);
            }
        }
    }

    Err(last_err)
}

/// Generates a Windows `HBITMAP` (top-down 32bpp BGRA) from any `Read + Seek` archive stream.
pub fn create_thumbnail<R: Read + Seek>(
    reader: &mut R,
    cx: u32,
    settings: &Settings,
) -> Result<(HBITMAP, ThumbnailReport), String> {
    create_thumbnail_with_ext(reader, cx, settings, None)
}

/// Generates a Windows `HBITMAP` (top-down 32bpp BGRA) with an optional filename/extension hint.
pub fn create_thumbnail_with_ext<R: Read + Seek>(
    reader: &mut R,
    cx: u32,
    settings: &Settings,
    ext_or_name: Option<&str>,
) -> Result<(HBITMAP, ThumbnailReport), String> {
    let (bgra, report) = generate_thumbnail_bytes_with_ext(reader, cx, settings, ext_or_name)?;
    let mut out_w = report.decode.out_width;
    let mut out_h = report.decode.out_height;
    let mut final_bgra = bgra;
    if settings.border_archive != 0 {
        if let Some(rgba) = image::RgbaImage::from_raw(out_w, out_h, bgra_to_rgba_vec(&final_bgra)) {
            let bordered = badge::apply_border_decoration(&rgba, settings.border_archive_enum());
            out_w = bordered.width();
            out_h = bordered.height();
            final_bgra = rgba_to_bgra(&bordered);
        }
    }
    let hbmp = create_hbitmap_from_bgra(out_w, out_h, &final_bgra)?;
    Ok((hbmp, report))
}

/// Converts an `RgbaImage` to 32bpp BGRA byte vector.
pub fn rgba_to_bgra(img: &image::RgbaImage) -> Vec<u8> {
    let mut bgra = Vec::with_capacity((img.width() * img.height() * 4) as usize);
    for pixel in img.pixels() {
        let [r, g, b, a] = pixel.0;
        bgra.push(b);
        bgra.push(g);
        bgra.push(r);
        bgra.push(a);
    }
    bgra
}

fn bgra_to_rgba_vec(bgra: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(bgra.len());
    let mut i = 0;
    while i + 3 < bgra.len() {
        let b = bgra[i];
        let g = bgra[i + 1];
        let r = bgra[i + 2];
        let a = bgra[i + 3];
        rgba.push(r);
        rgba.push(g);
        rgba.push(b);
        rgba.push(a);
        i += 4;
    }
    rgba
}

pub fn static_ext_label(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        ".mp4" | "mp4" => "MP4",
        ".m4v" | "m4v" => "M4V",
        ".mkv" | "mkv" => "MKV",
        ".webm" | "webm" => "WEBM",
        ".avi" | "avi" => "AVI",
        ".mov" | "mov" => "MOV",
        ".wmv" | "wmv" => "WMV",
        ".asf" | "asf" => "ASF",
        ".flv" | "flv" => "FLV",
        ".f4v" | "f4v" => "F4V",
        ".ts" | "ts" => "TS",
        ".m2ts" | "m2ts" => "M2TS",
        ".mts" | "mts" => "MTS",
        ".vob" | "vob" => "VOB",
        ".3gp" | "3gp" => "3GP",
        ".ogv" | "ogv" => "OGV",
        ".rm" | "rm" => "RM",
        ".rmvb" | "rmvb" => "RMVB",
        ".mp3" | "mp3" => "MP3",
        ".flac" | "flac" => "FLAC",
        ".m4a" | "m4a" => "M4A",
        ".aac" | "aac" => "AAC",
        ".ogg" | "ogg" => "OGG",
        ".opus" | "opus" => "OPUS",
        ".wav" | "wav" => "WAV",
        ".wma" | "wma" => "WMA",
        ".ape" | "ape" => "APE",
        ".epub" | "epub" => "EPUB",
        ".mobi" | "mobi" => "MOBI",
        ".iba" | "iba" => "IBA",
        ".azw" | "azw" => "AZW",
        ".azw3" | "azw3" => "AZW3",
        ".pdf" | "pdf" => "PDF",
        ".docx" | "docx" => "DOCX",
        ".doc" | "doc" => "DOC",
        ".xlsx" | "xlsx" => "XLSX",
        ".xls" | "xls" => "XLS",
        ".psd" | "psd" => "PSD",
        ".txt" | "txt" => "TXT",
        ".md" | "md" => "MD",
        _ => "MEDIA",
    }
}

fn extract_normalized_ext(ext_or_name: Option<&str>) -> String {
    let Some(raw) = ext_or_name else {
        return String::new();
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some(ext) = Path::new(trimmed).extension().and_then(|e| e.to_str()) {
        return format!(".{}", ext.to_ascii_lowercase());
    }
    if trimmed.starts_with('.') && !trimmed.contains('\\') && !trimmed.contains('/') {
        return trimmed.to_ascii_lowercase();
    }
    if !trimmed.contains('.') && !trimmed.contains('\\') && !trimmed.contains('/') {
        return format!(".{}", trimmed.to_ascii_lowercase());
    }
    String::new()
}

/// Unified stream/file thumbnail generator for all non-video formats
/// (E-Books, Audio, Documents/Other + custom text extensions, and Image Archives).
/// Returns `(HBITMAP, summary_details)`.
pub fn create_thumbnail_from_reader_unified<R: Read + Seek>(
    reader: &mut R,
    cx: u32,
    settings: &Settings,
    ext_or_name: Option<&str>,
) -> Result<(HBITMAP, String), String> {
    let ext = extract_normalized_ext(ext_or_name);
    let cover_opt = CoverOptions {
        sort: settings.sort_alphabetically,
        prefer_continuous_sequence: settings.prefer_continuous_sequence,
        max_image_bytes: settings.max_image_bytes(),
        scan_timeout: Duration::from_secs(settings.max_scan_seconds as u64),
        anomaly_detection: settings.anomaly_detection && settings.badge_risk,
        risk_rules: settings.risk_rules(),
    };

    if media::is_ebook_extension(&ext) {
        let mut rgba = ebook::extract_ebook_cover_image(reader, &ext, cx, &cover_opt)?;
        rgba = badge::apply_border_decoration(&rgba, settings.border_ebook_enum());
        other_doc::overlay_associated_app_icon(&mut rgba, &ext);
        let w = rgba.width();
        let h = rgba.height();
        let bgra = rgba_to_bgra(&rgba);
        let hbmp = create_hbitmap_from_bgra(w, h, &bgra)?;
        let label = static_ext_label(&ext);
        return Ok((
            hbmp,
            i18n::t_fmt("details.ebook", &[("w", &w.to_string()), ("h", &h.to_string()), ("label", label)]),
        ));
    }

    if media::is_audio_extension(&ext) {
        if settings.cover_art_enabled {
            let _ = reader.seek(std::io::SeekFrom::Start(0));
            if let Some(cover_bytes) = media::extract_embedded_cover(reader) {
                let dec_opt = DecodeOptions {
                    max_side: cx,
                    crop_tall: false,
                    is_epub: false,
                    badge_epub: false,
                    badge_animated: false,
                    badge_static: false,
                    archive_badge_label: "",
                    has_risk: false,
                };
                if let Ok(decoded) = decode_to_thumbnail(&cover_bytes, dec_opt) {
                    if let Some(mut rgba) = image::RgbaImage::from_raw(
                        decoded.info.out_width,
                        decoded.info.out_height,
                        bgra_to_rgba_vec(&decoded.bgra),
                    ) {
                        rgba = badge::apply_border_decoration(&rgba, settings.border_audio_enum());
                        if settings.badge_audio {
                            let label = static_ext_label(&ext);
                            badge::draw_media_corner_badge(&mut rgba, label, false);
                        }
                        let w = rgba.width();
                        let h = rgba.height();
                        let bgra = rgba_to_bgra(&rgba);
                        let hbmp = create_hbitmap_from_bgra(w, h, &bgra)?;
                        return Ok((
                            hbmp,
                            i18n::t_fmt("details.audio_cover", &[("w", &w.to_string()), ("h", &h.to_string())]),
                        ));
                    }
                }
            }
        }
        let label = static_ext_label(&ext);
        let mut card = generate_procedural_audio_card(cx, label);
        card = badge::apply_border_decoration(&card, settings.border_audio_enum());
        let w = card.width();
        let h = card.height();
        let bgra = rgba_to_bgra(&card);
        let hbmp = create_hbitmap_from_bgra(w, h, &bgra)?;
        return Ok((
            hbmp,
            i18n::t_fmt("details.audio_default", &[("w", &w.to_string()), ("h", &h.to_string())]),
        ));
    }

    let custom_exts = settings.parsed_custom_text_extensions();
    if media::is_other_or_custom_text_extension(&ext, &custom_exts) {
        let rgba = other_doc::create_other_thumbnail_rgba(reader, &ext, cx)?;
        let w = rgba.width();
        let h = rgba.height();
        let bgra = rgba_to_bgra(&rgba);
        let hbmp = create_hbitmap_from_bgra(w, h, &bgra)?;
        let ext_tag = ext.trim_start_matches('.').to_ascii_uppercase();
        return Ok((
            hbmp,
            i18n::t_fmt("details.document", &[("w", &w.to_string()), ("h", &h.to_string()), ("ext", &ext_tag)]),
        ));
    }

    match create_thumbnail_with_ext(reader, cx, settings, ext_or_name) {
        Ok((hbmp, rep)) => Ok((
            hbmp,
            i18n::t_fmt(
                "details.archive_page",
                &[
                    ("w", &rep.decode.out_width.to_string()),
                    ("h", &rep.decode.out_height.to_string()),
                    ("entry", &rep.entry_name),
                ],
            ),
        )),
        Err(archive_err) => {
            if other_doc::is_likely_plain_text_stream(reader) {
                let fallback_ext = if ext.is_empty() { ".txt" } else { ext.as_str() };
                let rgba = other_doc::create_other_thumbnail_rgba(reader, fallback_ext, cx)?;
                let w = rgba.width();
                let h = rgba.height();
                let bgra = rgba_to_bgra(&rgba);
                let hbmp = create_hbitmap_from_bgra(w, h, &bgra)?;
                let ext_tag = fallback_ext.trim_start_matches('.').to_ascii_uppercase();
                Ok((
                    hbmp,
                    i18n::t_fmt("details.plain_text", &[("w", &w.to_string()), ("h", &h.to_string()), ("ext", &ext_tag)]),
                ))
            } else {
                Err(archive_err)
            }
        }
    }
}

/// Simple standalone image decoder that resamples raw bytes to BGRA thumbnail.
pub fn decode_simple_image(bytes: &[u8], max_side: u32) -> Result<(Vec<u8>, u32, u32), String> {
    let dec_opt = image_dec::DecodeOptions {
        max_side,
        crop_tall: false,
        is_epub: false,
        badge_epub: false,
        badge_animated: false,
        badge_static: false,
        archive_badge_label: "",
        has_risk: false,
    };
    let res = image_dec::decode_to_thumbnail(bytes, dec_opt)?;
    Ok((res.bgra, res.info.out_width, res.info.out_height))
}

/// Generates a procedural high-DPI music album card when an audio file has no embedded cover art.
pub fn generate_procedural_audio_card(cx: u32, ext_label: &'static str) -> image::RgbaImage {
    let side = cx.clamp(48, 1024);
    let mut img = image::RgbaImage::from_pixel(side, side, image::Rgba([28, 28, 34, 255]));

    let center = side as f32 / 2.0;
    let radius = center * 0.85;
    let r_sq = radius * radius;
    let center_hole_sq = (radius * 0.22) * (radius * 0.22);

    for y in 0..side {
        for x in 0..side {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let d_sq = dx * dx + dy * dy;
            if d_sq <= r_sq {
                if d_sq < center_hole_sq {
                    img.put_pixel(x, y, image::Rgba([220, 60, 60, 255]));
                } else {
                    let dist = d_sq.sqrt();
                    let groove = ((dist * 1.5).sin() * 15.0) as i32;
                    let val = (24 + groove).clamp(10, 50) as u8;
                    img.put_pixel(x, y, image::Rgba([val, val, val + 2, 255]));
                }
            }
        }
    }

    badge::draw_media_corner_badge(&mut img, ext_label, false);
    img
}

/// Generates a procedural modern video card for corrupted, missing-codec, or unreadable video files.
pub fn generate_procedural_video_card(cx: u32, ext_label: &'static str, is_corrupt: bool) -> image::RgbaImage {
    let side = cx.clamp(48, 1024);
    let mut img = image::RgbaImage::from_pixel(side, side, image::Rgba([22, 24, 30, 255]));

    let center = side as f32 / 2.0;

    // Draw dark rounded video slate in center
    let slate_w = (side as f32 * 0.72) as i32;
    let slate_h = (side as f32 * 0.52) as i32;
    let x0 = (center - slate_w as f32 / 2.0) as i32;
    let y0 = (center - slate_h as f32 / 2.0) as i32;

    for y in y0..(y0 + slate_h) {
        for x in x0..(x0 + slate_w) {
            if x >= 0 && x < side as i32 && y >= 0 && y < side as i32 {
                let is_top_bar = (y - y0) < (slate_h / 4);
                if is_top_bar {
                    let stripe = ((x - x0) + (y - y0)) / ((slate_h / 4).max(1) / 2);
                    if stripe % 2 == 0 {
                        img.put_pixel(x as u32, y as u32, image::Rgba([180, 185, 200, 255]));
                    } else {
                        img.put_pixel(x as u32, y as u32, image::Rgba([45, 48, 56, 255]));
                    }
                } else {
                    img.put_pixel(x as u32, y as u32, image::Rgba([35, 38, 46, 255]));
                }
            }
        }
    }

    // Draw central play triangle or warning glyph
    let tri_size = (side as f32 * 0.14) as i32;
    let tri_cx = center as i32;
    let tri_cy = (center + slate_h as f32 * 0.10) as i32;

    if is_corrupt {
        // Soft amber warning glyph
        for dy in -tri_size..=tri_size {
            let row_w = ((tri_size - dy) * 2) / 3;
            for dx in -row_w..=row_w {
                let px = tri_cx + dx;
                let py = tri_cy + dy;
                if px >= 0 && px < side as i32 && py >= 0 && py < side as i32 {
                    if dy > tri_size / 2 {
                        img.put_pixel(px as u32, py as u32, image::Rgba([240, 170, 45, 255]));
                    } else if dy < tri_size / 4 && dx.abs() <= 2 {
                        img.put_pixel(px as u32, py as u32, image::Rgba([240, 170, 45, 255]));
                    }
                }
            }
        }
    } else {
        // Play triangle
        for dy in -tri_size..=tri_size {
            let max_dx = tri_size - dy.abs();
            for dx in 0..max_dx {
                let px = tri_cx - (tri_size / 3) + dx;
                let py = tri_cy + dy;
                if px >= 0 && px < side as i32 && py >= 0 && py < side as i32 {
                    img.put_pixel(px as u32, py as u32, image::Rgba([210, 220, 235, 240]));
                }
            }
        }
    }

    badge::draw_media_corner_badge(&mut img, ext_label, true);
    img
}

/// Sniffs container format (.mp4, .mkv, .avi, .wmv, .flv, .mp3, .flac, .ogg, .cbz, etc.) from the first bytes of a stream.
pub fn sniff_media_type_from_stream(stream: &IStream) -> &'static str {
    unsafe {
        let mut buf = [0u8; 512];
        let mut read: u32 = 0;
        let mut current_pos: u64 = 0;
        let _ = stream.Seek(0, STREAM_SEEK_CUR, Some(&mut current_pos));
        let _ = stream.Seek(0, STREAM_SEEK_SET, None);
        let hr = stream.Read(buf.as_mut_ptr().cast(), buf.len() as u32, Some(&mut read));
        let _ = stream.Seek(current_pos as i64, STREAM_SEEK_SET, None);
        if hr.is_err() || read < 1 {
            return "";
        }
        let n = read as usize;
        // MP4 / M4V / MOV / 3GP
        if n >= 8 && (&buf[4..8] == b"ftyp" || &buf[4..8] == b"moov") {
            return ".mp4";
        }
        // Matroska / WebM
        if n >= 4 && buf[0..4] == [0x1A, 0x45, 0xDF, 0xA3] {
            return ".mkv";
        }
        // AVI
        if n >= 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"AVI " {
            return ".avi";
        }
        // WMV / ASF
        if n >= 16 && buf[0..4] == [0x30, 0x26, 0xB2, 0x75] {
            return ".wmv";
        }
        // FLV
        if n >= 3 && &buf[0..3] == b"FLV" {
            return ".flv";
        }
        // MP3
        if n >= 3 && &buf[0..3] == b"ID3" {
            return ".mp3";
        }
        if n >= 2 && buf[0] == 0xFF && (buf[1] & 0xE0) == 0xE0 {
            return ".mp3";
        }
        // FLAC
        if n >= 4 && &buf[0..4] == b"fLaC" {
            return ".flac";
        }
        // OGG
        if n >= 4 && &buf[0..4] == b"OggS" {
            return ".ogg";
        }
        // ZIP / CBZ / EPUB
        if n >= 4 && &buf[0..4] == b"PK\x03\x04" {
            return ".cbz";
        }
        // RAR / CBR
        if n >= 4 && (&buf[0..4] == b"Rar!" || (n >= 7 && &buf[0..7] == b"Rar!\x1A\x07\x00") || (n >= 8 && &buf[0..8] == b"Rar!\x1A\x07\x01\x00")) {
            return ".cbr";
        }
        // 7Z / CB7
        if n >= 6 && &buf[0..6] == b"7z\xBC\xAF\x27\x1C" {
            return ".cb7";
        }
        // PDF
        if n >= 4 && &buf[0..4] == b"%PDF" {
            return ".pdf";
        }
        // Adobe Photoshop PSD
        if n >= 4 && &buf[0..4] == b"8BPS" {
            return ".psd";
        }
        // UTF-8 BOM / UTF-16 BOM / Plain text heuristic
        let sample = &buf[..n];
        if (n >= 3 && sample[0..3] == [0xEF, 0xBB, 0xBF])
            || (n >= 2 && (sample[0..2] == [0xFF, 0xFE] || sample[0..2] == [0xFE, 0xFF]))
        {
            return ".txt";
        }
        if !sample.contains(&0x00) {
            let valid_text_bytes = sample
                .iter()
                .filter(|&&b| b == b'\t' || b == b'\n' || b == b'\r' || b >= 0x20)
                .count();
            if valid_text_bytes * 100 >= sample.len() * 85 {
                return ".txt";
            }
        }
        ""
    }
}

/// Universal Thumbnail Generator:
/// Dispatches transparently to:
/// 1. Windows Media Foundation Smart Video Rendering (MP4, MKV, WebM, AVI, etc.)
/// 2. Audio Embedded Album Cover Art (MP3, FLAC, M4A, OGG, OPUS, etc.)
/// 3. Image Archives (CBZ, CBR, CB7, CBT, EPUB, ZIP, RAR, 7Z, TAR)
pub fn create_universal_thumbnail(
    stream: &IStream,
    cx: u32,
    settings: &Settings,
    ext_hint: Option<&str>,
) -> Result<HBITMAP, String> {
    let mut ext = extract_normalized_ext(ext_hint);

    if ext.is_empty() {
        let sniffed = sniff_media_type_from_stream(stream);
        if !sniffed.is_empty() {
            ext = sniffed.to_string();
        }
    }

    if media::is_video_extension(&ext) {
        // Direct video frame decode using Windows Media Foundation & Smart Fast Rendering.
        // Videos decode video frames directly — no embedded audio cover scanning.
        let res = if let Some(path_str) = ext_hint {
            let p = Path::new(path_str);
            // Only trust an absolute, local, non-device path; a bare stream name must never be
            // resolved against the host process's current directory (confused-deputy read).
            if media::is_trusted_local_abs_path(p) && p.is_file() {
                media::video_mf::extract_video_thumbnail_from_path(p, cx, &settings.smart_render_config())
            } else {
                media::video_mf::extract_video_thumbnail_from_stream(stream, cx, &settings.smart_render_config())
            }
        } else {
            media::video_mf::extract_video_thumbnail_from_stream(stream, cx, &settings.smart_render_config())
        };

        match res {
            Ok((mut rgba, _report)) => {
                rgba = badge::apply_border_decoration(&rgba, settings.border_video_enum());
                let bgra = rgba_to_bgra(&rgba);
                create_hbitmap_from_bgra(rgba.width(), rgba.height(), &bgra)
            }
            Err(_) => {
                // Windows 11 Explorer Fast Recovery:
                // When video decoding fails (e.g. a truncated or corrupted container),
                // DO NOT return E_FAIL! Returning E_FAIL causes Windows Explorer to stall
                // trying legacy/MPC-BE DirectShow decoders for 30 seconds!
                // Instead, generate a clean procedural video card with format badge.
                let label = static_ext_label(&ext);
                let mut card = generate_procedural_video_card(cx, label, true);
                card = badge::apply_border_decoration(&card, settings.border_video_enum());
                let bgra = rgba_to_bgra(&card);
                create_hbitmap_from_bgra(card.width(), card.height(), &bgra)
            }
        }
    } else {
        let mut reader = ComStreamReader::new(stream.clone());
        let effective_hint = ext_hint.filter(|s| !s.trim().is_empty()).unwrap_or(&ext);
        let hint_opt = if effective_hint.is_empty() {
            None
        } else {
            Some(effective_hint)
        };
        let (hbmp, _detail) = create_thumbnail_from_reader_unified(&mut reader, cx, settings, hint_opt)?;
        Ok(hbmp)
    }
}

/// Builds the Explorer hover info-tip string for an archive file on disk.
/// Skips opening cloud placeholder files (`FILE_ATTRIBUTE_OFFLINE` / `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`)
/// so hovering in Explorer never triggers an unwanted cloud download.
pub fn build_infotip(path: &Path) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    let file_size = meta.len();

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let desc = find_file_type_by_ext(&ext)
        .map(|ft| ft.description)
        .unwrap_or("Archive");

    let size_str = format_byte_size(file_size);

    #[cfg(windows)]
    let is_cloud_placeholder = {
        use std::os::windows::fs::MetadataExt;
        const OFFLINE_MASK: u32 = 0x0000_1000 | 0x0040_0000 | 0x0004_0000;
        (meta.file_attributes() & OFFLINE_MASK) != 0
    };
    #[cfg(not(windows))]
    let is_cloud_placeholder = false;

    if file_size == 0 || is_cloud_placeholder {
        return Ok(format!("{desc}\nSize: {size_str}"));
    }

    if let Ok(mut f) = File::open(path) {
        let settings = Settings::load();
        if let Ok(stats) = count_entries(&mut f, Duration::from_secs(3)) {
            let plus = if stats.complete { "" } else { "+" };
            let mut tip = format!(
                "{desc}\n{}{plus} Images\n{}{plus} Files\nSize: {size_str}",
                stats.images, stats.files
            );
            if settings.anomaly_detection && stats.has_risk {
                let alert_msg = i18n::t_for(settings::UiLang::load_or_detect(), "tooltip.security_warning");
                tip.push_str(&alert_msg);
            }
            return Ok(tip);
        }
    }

    Ok(format!("{desc}\nSize: {size_str}"))
}

pub fn format_byte_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["bytes", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut val = bytes as f64;
    let mut unit_idx = 0;
    while val >= 1024.0 && unit_idx + 1 < UNITS.len() {
        val /= 1024.0;
        unit_idx += 1;
    }
    format!("{val:.2} {}", UNITS[unit_idx])
}

/// Adapter implementing `std::io::Read + std::io::Seek` over a Windows COM `IStream`.
pub struct ComStreamReader {
    stream: IStream,
}

impl ComStreamReader {
    pub fn new(stream: IStream) -> Self {
        Self { stream }
    }
}

impl Read for ComStreamReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let to_read = buf.len().min(u32::MAX as usize) as u32;
        let mut read_bytes: u32 = 0;
        // SAFETY: buf is valid for `to_read` bytes and `read_bytes` is a valid out-pointer.
        let hr = unsafe {
            self.stream.Read(
                buf.as_mut_ptr().cast(),
                to_read,
                Some(&mut read_bytes),
            )
        };
        if hr.is_err() {
            return Err(std::io::Error::from_raw_os_error(hr.0));
        }
        // A buggy or hostile third-party IStream (shell namespace extension, cloud provider)
        // could report more bytes than were requested. Never propagate a count larger than
        // the caller's buffer: downstream slicing/`read_to_end` would panic or misbehave.
        if read_bytes > to_read {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "IStream::Read reported more bytes than requested",
            ));
        }
        Ok(read_bytes as usize)
    }
}

impl Seek for ComStreamReader {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let (offset, origin) = match pos {
            SeekFrom::Start(o) => (o as i64, STREAM_SEEK_SET),
            SeekFrom::Current(o) => (o, STREAM_SEEK_CUR),
            SeekFrom::End(o) => (o, STREAM_SEEK_END),
        };
        let mut new_pos: u64 = 0;
        // SAFETY: `new_pos` is a valid out-pointer.
        unsafe {
            self.stream
                .Seek(offset, origin, Some(&mut new_pos))
                .map_err(|e| std::io::Error::from_raw_os_error(e.code().0))?;
        }
        Ok(new_pos)
    }
}

#[cfg(test)]
mod security_adversarial_tests {
    use super::*;
    use std::io::{Cursor, Write};

    fn make_valid_png_1x1() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(4, 4, image::Rgba([20, 120, 220, 255]));
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .expect("png encode");
        out
    }

    fn build_test_zip(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut cur = Cursor::new(Vec::new());
        {
            let mut zw = zip::ZipWriter::new(&mut cur);
            for (name, data, compress) in entries {
                let method = if *compress {
                    zip::CompressionMethod::Deflated
                } else {
                    zip::CompressionMethod::Stored
                };
                let opts = zip::write::SimpleFileOptions::default().compression_method(method);
                let _ = zw.start_file(*name, opts);
                let _ = zw.write_all(data);
            }
            let _ = zw.finish();
        }
        cur.into_inner()
    }

    #[test]
    fn test_risk_rules_and_zero_width_evasion() {
        use crate::archive::{inspect_entry_security_with_rules, sanitize_archive_path, RiskRules};

        // Documented dangerous scripts including .js
        for name in ["payload.exe", "dropper.js", "macro.ps1", "shortcut.lnk", "link.url"] {
            assert!(
                inspect_entry_security_with_rules(name, 100, 50, false, None).is_some(),
                "expected {name} to be flagged"
            );
        }

        // Granular rule toggle disables executable detection when turned off
        let mut no_exe = RiskRules::default();
        no_exe.executables = false;
        assert!(inspect_entry_security_with_rules("dropper.js", 100, 50, false, Some(&no_exe)).is_none());

        // Zero-width / BiDi spoofing in both inspection and path sanitization
        for spoof in [
            "cover.jpg\u{202E}gnp.exe",
            "cover\u{200B}.png",
            "\u{FEFF}001.png",
            "sub/CO\u{2060}N.png",
        ] {
            assert!(
                inspect_entry_security_with_rules(spoof, 100, 50, false, None).is_some(),
                "expected spoofed name to be flagged"
            );
            assert!(
                sanitize_archive_path(spoof).is_none(),
                "expected spoofed name to be rejected by sanitizer"
            );
        }

        // Forged 0-byte compressed size with >8 MiB uncompressed claim
        assert!(inspect_entry_security_with_rules("bomb.png", 9 * 1024 * 1024, 0, false, None).is_some());
    }

    #[test]
    fn test_settings_sanitize_defends_against_tampering() {
        let mut s = Settings::default();
        s.max_image_mb = 1;
        s.max_scan_seconds = 9999;
        s.watchdog_timeout_sec = 0;
        s.luminance_lower_limit = 90;
        s.luminance_upper_limit = 10;
        s.border_archive = 99;
        s.border_ebook = 99;
        s.sanitize();
        assert_eq!(s.max_image_mb, Settings::MIN_IMAGE_MB);
        assert!(s.max_scan_seconds <= 120);
        assert_eq!(s.watchdog_timeout_sec, 3);
        assert!(s.luminance_lower_limit < s.luminance_upper_limit);
        assert_eq!(s.border_archive, 5);
        assert_eq!(s.border_ebook, 5);

        // Reject UNC, ADS, relative paths, and shell metacharacters in custom_ffmpeg_path
        for bad in [
            r"\\attacker\share\ffmpeg.exe",
            r"C:\tools\note.txt:ffmpeg.exe",
            r"ffmpeg.exe",
            r"C:\tools\ffmpeg.exe | calc.exe",
            r"C:\tools\ffmpeg.exe > out.txt",
            r"C:\tools\ffmpeg%TEMP%.exe",
        ] {
            let mut s2 = Settings::default();
            s2.custom_ffmpeg_path = bad.to_string();
            s2.sanitize();
            assert!(s2.custom_ffmpeg_path.is_empty(), "expected rejection of {bad}");
        }
    }

    #[test]
    fn test_zip_forged_uncompressed_header_ratio_bomb() {
        // Compress 10 MiB of zeros (compresses to ~10 KiB => ~1000:1 ratio)
        let zeros = vec![0u8; 10 * 1024 * 1024];
        let mut zip_bytes = build_test_zip(&[("001_bomb.png", &zeros, true)]);
        // Corrupt/forge the central directory and local header uncompressed size to 100 bytes
        // so pre-extraction header checks see 100 bytes while actual inflate is 10 MiB.
        let uncomp_le = (zeros.len() as u32).to_le_bytes();
        let forged_le = 100u32.to_le_bytes();
        for i in 0..zip_bytes.len().saturating_sub(4) {
            if zip_bytes[i..i + 4] == uncomp_le {
                zip_bytes[i..i + 4].copy_from_slice(&forged_le);
            }
        }
        let mut cur = Cursor::new(zip_bytes);
        let settings = Settings::default();
        let res = generate_thumbnail_bytes(&mut cur, 128, &settings);
        assert!(res.is_err(), "forged-size ratio bomb must be rejected");
    }

    #[test]
    fn test_audio_cover_loop_and_oom_defenses() {
        // Crafted MP4 with zero-size `data` atom inside `covr` (previously infinite loop)
        let mut mp4 = Vec::new();
        mp4.extend_from_slice(&40u32.to_be_bytes());
        mp4.extend_from_slice(b"ftypisom");
        mp4.extend_from_slice(&[0u8; 24]);
        // `covr` atom containing a 0-size `data` atom
        mp4.extend_from_slice(&24u32.to_be_bytes());
        mp4.extend_from_slice(b"covr");
        mp4.extend_from_slice(&0u32.to_be_bytes()); // size = 0!
        mp4.extend_from_slice(b"data");
        mp4.extend_from_slice(&[0u8; 8]);
        assert!(media::extract_embedded_cover(&mut Cursor::new(mp4)).is_none());

        // Crafted FLAC picture block with forged 2 GiB mime_len (must not OOM)
        let mut flac = Vec::new();
        flac.extend_from_slice(b"fLaC");
        flac.push(0x86); // last=1, type=6 (PICTURE)
        flac.extend_from_slice(&[0x00, 0x00, 0x30]); // blk_len = 48
        flac.extend_from_slice(&3u32.to_be_bytes()); // pic type
        flac.extend_from_slice(&0x7FFF_FFFFu32.to_be_bytes()); // mime_len = 2 GiB
        flac.extend_from_slice(&[0u8; 40]);
        assert!(media::extract_embedded_cover(&mut Cursor::new(flac)).is_none());
    }

    #[test]
    fn test_psd_allocation_bomb_and_png_dimension_bomb() {
        // Crafted PSD with channels=65535, height=100000, width=1, compression=1 (PackBits)
        let mut psd = Vec::new();
        psd.extend_from_slice(b"8BPS");
        psd.extend_from_slice(&2u16.to_be_bytes()); // PSB v2
        psd.extend_from_slice(&[0u8; 6]);
        psd.extend_from_slice(&65535u16.to_be_bytes()); // channels
        psd.extend_from_slice(&100_000u32.to_be_bytes()); // height
        psd.extend_from_slice(&1u32.to_be_bytes()); // width
        psd.extend_from_slice(&8u16.to_be_bytes()); // depth
        psd.extend_from_slice(&3u16.to_be_bytes()); // RGB
        psd.extend_from_slice(&0u32.to_be_bytes()); // color mode len
        psd.extend_from_slice(&0u32.to_be_bytes()); // image resources len
        psd.extend_from_slice(&0u64.to_be_bytes()); // layer/mask len (v2 = 8 bytes)
        psd.extend_from_slice(&1u16.to_be_bytes()); // compression = 1 (PackBits)
        let psd_res = other_doc::create_other_thumbnail_rgba(&mut Cursor::new(psd), ".psd", 128);
        // Must not OOM or panic; either errors or falls back to a bounded procedural card.
        if let Ok(img) = psd_res {
            assert!(img.width() <= 256 && img.height() <= 256);
        }

        // PNG header claiming 32768 x 32768 dimensions (exceeds MAX_IMAGE_DIMENSION = 16384)
        let mut png = make_valid_png_1x1();
        if png.len() > 24 {
            png[16..20].copy_from_slice(&32768u32.to_be_bytes());
            png[20..24].copy_from_slice(&32768u32.to_be_bytes());
        }
        let mut opt = image_dec::DecodeOptions::default();
        opt.max_side = 128;
        assert!(image_dec::decode_to_thumbnail(&png, opt).is_err());
    }

    #[test]
    fn test_resolve_name_blocks_unc_traversal_and_quote_injection() {
        assert!(media::resolve_file_from_name_with_source(r"\\attacker\share\clip.mp4", 1024).is_none());
        assert!(media::resolve_file_from_name_with_source(r"..\..\clip.mp4", 1024).is_none());
        assert!(media::resolve_via_installed_everything(r#"clip" OR ".mp4"#, 1024).is_none());
    }

    #[test]
    fn test_text_rendering_strips_bidi_and_control_chars() {
        let evil_txt = b"normal\x00\x07\x1B[31m\xE2\x80\xAEgnp.exe\nline2\r\n";
        let res = other_doc::create_other_thumbnail_rgba(&mut Cursor::new(evil_txt.as_slice()), ".txt", 128);
        assert!(res.is_ok(), "text card rendering must tolerate control/BiDi bytes");
    }

    #[test]
    fn test_mutation_fuzz_no_panic_no_oom() {
        // Deterministic PRNG (xorshift64) mutating real container & image seeds
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next_u64 = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };

        let png = make_valid_png_1x1();
        let zip_seed = build_test_zip(&[("001.png", &png, true)]);
        let mut tar_cur = Cursor::new(Vec::new());
        {
            let mut tb = tar::Builder::new(&mut tar_cur);
            let mut hdr = tar::Header::new_gnu();
            hdr.set_size(png.len() as u64);
            let _ = hdr.set_path("001.png");
            hdr.set_cksum();
            let _ = tb.append(&hdr, Cursor::new(&png));
            let _ = tb.finish();
        }
        let tar_seed = tar_cur.into_inner();
        let sz_seed = b"7z\xBC\xAF\x27\x1C\x00\x04\x00\x00\x00\x00\x00\x00\x00\x00".to_vec();
        let rar_seed = b"Rar!\x1A\x07\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec();
        let psd_seed = b"8BPS\x00\x01\x00\x00\x00\x00\x00\x00\x00\x03\x00\x00\x00\x10\x00\x00\x00\x10\x00\x08\x00\x03".to_vec();
        let mp3_seed = b"ID3\x03\x00\x00\x00\x00\x00\x20APIC\x00\x00\x00\x10\x00\x00\x00image/png\x00\x03\x00\x89PNG".to_vec();

        let seeds: &[(&str, &[u8])] = &[
            (".cbz", &zip_seed),
            (".cbt", &tar_seed),
            (".cb7", &sz_seed),
            (".cbr", &rar_seed),
            (".psd", &psd_seed),
            (".mp3", &mp3_seed),
            (".epub", &zip_seed),
            (".pdf", b"%PDF-1.7\n1 0 obj\n<<>>\nendobj\n"),
        ];

        let mut settings = Settings::default();
        settings.max_scan_seconds = 1;
        settings.max_image_mb = 8;

        for iter in 0..1200 {
            let (ext, base) = seeds[iter % seeds.len()];
            let mut buf = base.to_vec();
            let muts = (next_u64() % 6) + 1;
            for _ in 0..muts {
                if buf.is_empty() {
                    break;
                }
                let idx = (next_u64() as usize) % buf.len();
                match next_u64() % 4 {
                    0 => buf[idx] ^= (next_u64() & 0xFF) as u8,
                    1 => buf[idx] = 0xFF,
                    2 => buf[idx] = 0x00,
                    _ => {
                        let new_len = ((next_u64() as usize) % (base.len() + 32)).max(1);
                        buf.resize(new_len, (next_u64() & 0xFF) as u8);
                    }
                }
            }

            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut cur = Cursor::new(&buf);
                let _ = create_thumbnail_from_reader_unified(&mut cur, 128, &settings, Some(ext));
                let _ = media::extract_embedded_cover(&mut Cursor::new(&buf));
                let _ = image_dec::decode_to_thumbnail(&buf, image_dec::DecodeOptions::default());
            }));
            assert!(res.is_ok(), "panic on mutated {ext} input at iter {iter}");
        }
    }
}
