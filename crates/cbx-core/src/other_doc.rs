use std::io::{Read, Seek, SeekFrom};

use image::{imageops::FilterType, Rgba, RgbaImage};
use quick_xml::events::Event;
use quick_xml::Reader as XmlReader;
use zip::ZipArchive;

/// Trait alias for readable & seekable streams for "Other" document/graphics files.
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// Kind of file inside the "Other" category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtherFileKind {
    Word,
    Excel,
    Psd,
    Torrent,
    PlainText,
}

impl OtherFileKind {
    pub fn from_ext(ext: &str) -> Self {
        let clean = ext.trim().trim_start_matches('.').to_ascii_lowercase();
        match clean.as_str() {
            "docx" | "doc" => Self::Word,
            "xlsx" | "xls" => Self::Excel,
            "psd" => Self::Psd,
            "torrent" => Self::Torrent,
            _ => Self::PlainText,
        }
    }

    pub fn header_badge_text(self, ext: &str) -> String {
        let clean = ext.trim().trim_start_matches('.').to_ascii_uppercase();
        if clean.is_empty() {
            match self {
                Self::Word => "DOC".to_string(),
                Self::Excel => "XLS".to_string(),
                Self::Psd => "PSD".to_string(),
                Self::Torrent => "TORRENT".to_string(),
                Self::PlainText => "TXT".to_string(),
            }
        } else {
            clean.chars().take(6).collect()
        }
    }
}

/// Generates a stylized thumbnail for Word / Excel / PSD / Torrent / Plain Text files,
/// complete with program-specific frame styling and the system default application icon in the bottom-right.
pub fn create_other_thumbnail_rgba(
    stream: &mut dyn ReadSeek,
    ext: &str,
    target_size: u32,
) -> Result<RgbaImage, String> {
    let size = target_size.clamp(64, 512);
    let kind = OtherFileKind::from_ext(ext);
    let clean_ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();

    let mut img = match kind {
        OtherFileKind::Psd => render_psd_card(stream, &clean_ext, size)?,
        OtherFileKind::Word => render_word_card(stream, &clean_ext, size),
        OtherFileKind::Excel => render_excel_card(stream, &clean_ext, size),
        OtherFileKind::Torrent => render_torrent_card(stream, size),
        OtherFileKind::PlainText => render_text_card(stream, &clean_ext, size),
    };

    // Overlay Windows default opening application icon in bottom-right corner
    let dot_ext = if clean_ext.is_empty() {
        ".txt".to_string()
    } else {
        format!(".{}", clean_ext)
    };
    overlay_associated_app_icon(&mut img, &dot_ext);

    Ok(img)
}

// ---------------------------------------------------------------------------
// 1. Word (.docx / .doc)
// ---------------------------------------------------------------------------

fn render_word_card(stream: &mut dyn ReadSeek, clean_ext: &str, size: u32) -> RgbaImage {
    let _ = stream.seek(SeekFrom::Start(0));
    let frame_bg = Rgba([24, 90, 189, 255]);    // Word Blue #185ABD
    let accent_line = Rgba([40, 136, 238, 255]); // Lighter blue accent

    // If OOXML .docx contains embedded thumbnail (docProps/thumbnail.*), use it inside Word frame!
    if clean_ext == "docx" {
        if let Some(embedded) = try_extract_ooxml_embedded_thumb(stream, size) {
            return apply_program_frame(&embedded, size, frame_bg, accent_line, FoldCorner::None);
        }
    }

    let preview_lines = if clean_ext == "docx" {
        extract_docx_preview_lines(stream)
    } else {
        extract_binary_doc_preview_lines(stream)
    };

    let (avail_w, avail_h) = program_frame_inner_bounds(size);
    let sheet_w = ((avail_w as f32 * 0.80).round() as u32).clamp(32, avail_w);
    let sheet = render_document_sheet_card(
        sheet_w,
        avail_h,
        frame_bg,
        Rgba([250, 251, 253, 255]),
        Rgba([36, 41, 47, 255]),
        &preview_lines,
        false,
    );
    apply_program_frame(&sheet, size, frame_bg, accent_line, FoldCorner::None)
}

fn try_extract_ooxml_embedded_thumb(stream: &mut dyn ReadSeek, size: u32) -> Option<RgbaImage> {
    let _ = stream.seek(SeekFrom::Start(0));
    crate::archive::verify_zip_eocd_tail(stream).ok()?;
    let mut archive = ZipArchive::new(stream).ok()?;
    let candidates = [
        "docProps/thumbnail.jpeg",
        "docProps/thumbnail.jpg",
        "docProps/thumbnail.png",
    ];
    for idx in 0..archive.len().min(512) {
        if let Ok(mut entry) = archive.by_index(idx) {
            let name = entry.name().to_ascii_lowercase();
            if candidates.iter().any(|c| name == c.to_ascii_lowercase()) {
                if entry.size() > 0 && entry.size() <= 16 * 1024 * 1024 {
                    let mut buf = Vec::with_capacity((entry.size() as usize).min(1024 * 1024));
                    if (&mut entry).take(16 * 1024 * 1024 + 1).read_to_end(&mut buf).is_ok()
                        && buf.len() <= 16 * 1024 * 1024
                    {
                        let mut reader = image::ImageReader::new(std::io::Cursor::new(&buf));
                        if let Ok(guessed) = reader.with_guessed_format() {
                            let mut lim = image::Limits::default();
                            lim.max_image_width = Some(crate::image_dec::MAX_IMAGE_DIMENSION);
                            lim.max_image_height = Some(crate::image_dec::MAX_IMAGE_DIMENSION);
                            lim.max_alloc = Some(crate::image_dec::MAX_DECODER_ALLOC_BYTES);
                            reader = guessed;
                            reader.limits(lim);
                            if let Ok(dyn_img) = reader.decode() {
                                let inner = size.max(64);
                                let resized = dyn_img.resize(inner, inner, FilterType::Lanczos3);
                                return Some(resized.to_rgba8());
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

fn extract_docx_preview_lines(stream: &mut dyn ReadSeek) -> Vec<String> {
    let _ = stream.seek(SeekFrom::Start(0));
    if crate::archive::verify_zip_eocd_tail(stream).is_err() {
        return vec!["Microsoft Word Document".to_string()];
    }
    let Ok(mut archive) = ZipArchive::new(stream) else {
        return vec!["Microsoft Word Document".to_string()];
    };
    let Ok(mut doc_xml) = archive.by_name("word/document.xml") else {
        return vec!["Word Document (.docx)".to_string()];
    };
    let mut xml_buf = Vec::new();
    let _ = (&mut doc_xml).take(512 * 1024).read_to_end(&mut xml_buf);

    let mut reader = XmlReader::from_reader(xml_buf.as_slice());
    let mut buf = Vec::new();
    let mut lines: Vec<String> = Vec::new();
    let mut current_line = String::new();
    let mut in_text = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                if e.local_name().as_ref() == b"t" {
                    in_text = true;
                }
            }
            Ok(Event::Text(t)) if in_text => {
                if let Ok(s) = t.unescape() {
                    current_line.push_str(&s);
                }
            }
            Ok(Event::End(e)) => {
                let ln = e.local_name();
                if ln.as_ref() == b"t" {
                    in_text = false;
                } else if ln.as_ref() == b"p" {
                    let trimmed = current_line.trim().to_string();
                    if !trimmed.is_empty() {
                        lines.push(trimmed);
                        if lines.len() >= 18 {
                            break;
                        }
                    }
                    current_line.clear();
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    let mut lines = crate::redact::redact_lines(lines);
    if lines.is_empty() {
        lines.push(crate::i18n::t("doc.word_no_preview"));
    }
    lines
}

fn extract_binary_doc_preview_lines(stream: &mut dyn ReadSeek) -> Vec<String> {
    let _ = stream.seek(SeekFrom::Start(0));
    let mut buf = Vec::new();
    let _ = stream.take(256 * 1024).read_to_end(&mut buf);

    // Extract longest clean ASCII/Unicode paragraph fragments as best-effort preview
    let mut runs: Vec<String> = Vec::new();
    let mut cur = String::new();
    for &b in &buf {
        if (0x20..=0x7E).contains(&b) {
            cur.push(b as char);
        } else {
            if cur.len() >= 12
                && !cur.contains("Root Entry")
                && !cur.contains("WordDocument")
                && !cur.contains("Microsoft")
                && !cur.contains("Times New Roman")
            {
                runs.push(cur.trim().to_string());
                if runs.len() >= 14 {
                    break;
                }
            }
            cur.clear();
        }
    }
    let runs = crate::redact::redact_lines(runs);
    if runs.is_empty() {
        vec!["Microsoft Word 97-2003 (.doc)".to_string()]
    } else {
        runs
    }
}

// ---------------------------------------------------------------------------
// 2. Excel (.xlsx / .xls)
// ---------------------------------------------------------------------------

fn render_excel_card(stream: &mut dyn ReadSeek, clean_ext: &str, size: u32) -> RgbaImage {
    let _ = stream.seek(SeekFrom::Start(0));
    let frame_bg = Rgba([16, 124, 65, 255]);    // Excel Green #107C41
    let accent_line = Rgba([51, 170, 102, 255]); // Lighter green accent

    if clean_ext == "xlsx" {
        if let Some(embedded) = try_extract_ooxml_embedded_thumb(stream, size) {
            return apply_program_frame(&embedded, size, frame_bg, accent_line, FoldCorner::None);
        }
    }

    let rows = if clean_ext == "xlsx" {
        extract_xlsx_preview_rows(stream)
    } else {
        vec![vec!["Excel".to_string(), "Workbook".to_string(), ".XLS".to_string()]]
    };

    let (avail_w, avail_h) = program_frame_inner_bounds(size);
    let sheet_w = ((avail_w as f32 * 0.90).round() as u32).clamp(32, avail_w);
    let sheet = render_excel_grid_card(sheet_w, avail_h, &rows);
    apply_program_frame(&sheet, size, frame_bg, accent_line, FoldCorner::None)
}

fn extract_xlsx_preview_rows(stream: &mut dyn ReadSeek) -> Vec<Vec<String>> {
    let _ = stream.seek(SeekFrom::Start(0));
    if crate::archive::verify_zip_eocd_tail(stream).is_err() {
        return Vec::new();
    }
    let Ok(mut archive) = ZipArchive::new(stream) else {
        return Vec::new();
    };

    // Parse sharedStrings.xml first
    let mut shared: Vec<String> = Vec::new();
    if let Ok(mut ss_file) = archive.by_name("xl/sharedStrings.xml") {
        let mut ss_buf = Vec::new();
        let _ = (&mut ss_file).take(1024 * 1024).read_to_end(&mut ss_buf);
        let mut r = XmlReader::from_reader(ss_buf.as_slice());
        let mut b = Vec::new();
        let mut in_si = false;
        let mut in_t = false;
        let mut cur = String::new();
        loop {
            match r.read_event_into(&mut b) {
                Ok(Event::Start(e)) => {
                    if e.local_name().as_ref() == b"si" {
                        in_si = true;
                        cur.clear();
                    } else if in_si && e.local_name().as_ref() == b"t" {
                        in_t = true;
                    }
                }
                Ok(Event::Text(t)) if in_si && in_t => {
                    if let Ok(s) = t.unescape() {
                        cur.push_str(&s);
                    }
                }
                Ok(Event::End(e)) => {
                    if e.local_name().as_ref() == b"t" {
                        in_t = false;
                    } else if e.local_name().as_ref() == b"si" {
                        in_si = false;
                        shared.push(cur.clone());
                        if shared.len() >= 512 {
                            break;
                        }
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            b.clear();
        }
    }

    // Parse xl/worksheets/sheet1.xml (first available sheet)
    let sheet_name = (0..archive.len().min(256))
        .filter_map(|i| archive.by_index_raw(i).ok().map(|f| f.name().to_string()))
        .find(|n| n.to_ascii_lowercase().starts_with("xl/worksheets/sheet") && n.to_ascii_lowercase().ends_with(".xml"))
        .unwrap_or_else(|| "xl/worksheets/sheet1.xml".to_string());

    let Ok(mut ws_file) = archive.by_name(&sheet_name) else {
        return Vec::new();
    };
    let mut ws_buf = Vec::new();
    let _ = (&mut ws_file).take(512 * 1024).read_to_end(&mut ws_buf);

    let mut r = XmlReader::from_reader(ws_buf.as_slice());
    let mut b = Vec::new();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut cur_row: Vec<String> = Vec::new();
    let mut cell_type = String::new();
    let mut in_v = false;
    let mut in_is_t = false;
    let mut val_buf = String::new();

    loop {
        match r.read_event_into(&mut b) {
            Ok(Event::Start(e)) => {
                let ln = e.local_name();
                if ln.as_ref() == b"row" {
                    cur_row.clear();
                } else if ln.as_ref() == b"c" {
                    cell_type.clear();
                    for attr in e.attributes().flatten() {
                        if attr.key.local_name().as_ref() == b"t" {
                            cell_type = attr.unescape_value().map(|s| s.into_owned()).unwrap_or_default();
                        }
                    }
                } else if ln.as_ref() == b"v" {
                    in_v = true;
                    val_buf.clear();
                } else if ln.as_ref() == b"t" {
                    in_is_t = true;
                    val_buf.clear();
                }
            }
            Ok(Event::Text(t)) if in_v || in_is_t => {
                if let Ok(s) = t.unescape() {
                    val_buf.push_str(&s);
                }
            }
            Ok(Event::End(e)) => {
                let ln = e.local_name();
                if ln.as_ref() == b"v" || ln.as_ref() == b"t" {
                    let display = if cell_type == "s" && in_v {
                        val_buf
                            .trim()
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| shared.get(i).cloned())
                            .unwrap_or_else(|| val_buf.clone())
                    } else {
                        val_buf.clone()
                    };
                    if cur_row.len() < 4 {
                        let cell = crate::redact::sanitize_label(display.trim());
                        cur_row.push(crate::redact::redact_line(&cell));
                    }
                    in_v = false;
                    in_is_t = false;
                } else if ln.as_ref() == b"row" {
                    if !cur_row.is_empty() && cur_row.iter().any(|s| !s.is_empty()) {
                        rows.push(cur_row.clone());
                        if rows.len() >= 10 {
                            break;
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        b.clear();
    }

    rows
}

// ---------------------------------------------------------------------------
// 3. Adobe Photoshop (.psd)
// ---------------------------------------------------------------------------

fn be_u16_at(b: &[u8], off: usize) -> Option<u16> {
    b.get(off..off + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}

fn be_u32_at(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn render_psd_card(stream: &mut dyn ReadSeek, _clean_ext: &str, size: u32) -> Result<RgbaImage, String> {
    let _ = stream.seek(SeekFrom::Start(0));
    let mut hdr = [0u8; 26];
    stream
        .read_exact(&mut hdr)
        .map_err(|e| format!("PSD header read failed: {}", e))?;
    if &hdr[0..4] != b"8BPS" {
        return Err("Not a valid PSD 8BPS file".to_string());
    }

    let version = be_u16_at(&hdr, 4).unwrap_or(0);
    if version != 1 && version != 2 {
        return Err(format!("Unsupported PSD version: {}", version));
    }

    let channels = be_u16_at(&hdr, 12).unwrap_or(0);
    let height = be_u32_at(&hdr, 14).unwrap_or(0);
    let width = be_u32_at(&hdr, 18).unwrap_or(0);
    let depth = be_u16_at(&hdr, 22).unwrap_or(0);
    let color_mode = be_u16_at(&hdr, 24).unwrap_or(0);

    // 1. Color Mode Data Section
    let mut len4 = [0u8; 4];
    stream
        .read_exact(&mut len4)
        .map_err(|e| format!("PSD color mode read failed: {}", e))?;
    let color_mode_len = u32::from_be_bytes(len4) as u64;
    stream
        .seek(SeekFrom::Current(color_mode_len as i64))
        .map_err(|e| format!("PSD seek past color mode failed: {}", e))?;

    // 2. Image Resources Section (often contains pre-rendered JPEG Thumbnail resource ID 1036 / 1033!)
    stream
        .read_exact(&mut len4)
        .map_err(|e| format!("PSD image resources len failed: {}", e))?;
    let res_len = u32::from_be_bytes(len4) as usize;

    let mut extracted_thumb: Option<RgbaImage> = None;
    if res_len > 0 && res_len <= 32 * 1024 * 1024 {
        let mut res_buf = vec![0u8; res_len];
        if stream.read_exact(&mut res_buf).is_ok() {
            extracted_thumb = extract_psd_resource_thumbnail(&res_buf, size);
        }
    } else if res_len > 0 {
        let _ = stream.seek(SeekFrom::Current(res_len as i64));
    }

    // 3. Fallback: try decoding merged composite image data when PSD thumbnail resource is missing
    if extracted_thumb.is_none() && height > 0 && width > 0 && height <= 12000 && width <= 12000 {
        extracted_thumb = try_decode_psd_composite(
            stream,
            version,
            channels,
            height,
            width,
            depth,
            color_mode,
            size,
        );
    }

    let (avail_w, avail_h) = program_frame_inner_bounds(size);
    let inner = extracted_thumb.unwrap_or_else(|| {
        let dim_line = format!("{} x {} px ({}-bit)", width, height, depth);
        let sheet_w = ((avail_w as f32 * 0.84).round() as u32).clamp(32, avail_w);
        let sheet_h = ((avail_h as f32 * 0.90).round() as u32).clamp(32, avail_h);
        render_document_sheet_card(
            sheet_w,
            sheet_h,
            Rgba([56, 168, 248, 255]),
            Rgba([24, 34, 48, 255]),
            Rgba([220, 240, 255, 255]),
            &[
                "Photoshop Artwork".to_string(),
                dim_line,
                format!("Channels: {}", channels),
            ],
            true,
        )
    });

    Ok(apply_program_frame(
        &inner,
        size,
        Rgba([8, 34, 62, 255]),     // Photoshop dark navy matching UI frame
        Rgba([56, 168, 248, 255]),  // Photoshop cyan #38A8F8
        FoldCorner::None,
    ))
}

fn extract_psd_resource_thumbnail(res_buf: &[u8], target_size: u32) -> Option<RgbaImage> {
    let mut pos = 0usize;
    while pos + 12 <= res_buf.len() {
        if &res_buf[pos..pos + 4] != b"8BIM" {
            break;
        }
        let res_id = be_u16_at(res_buf, pos + 4)?;
        let name_len = *res_buf.get(pos + 6)? as usize;
        // Pascal string padded to even length (1 + name_len padded to 2-byte boundary)
        let pascal_total = (1 + name_len + 1) & !1;
        let size_off = pos + 6 + pascal_total;
        let data_size = be_u32_at(res_buf, size_off)? as usize;
        let data_off = size_off + 4;
        if data_off + data_size > res_buf.len() {
            break;
        }

        // Resource 1036 (Photoshop 5.0+ JPEG thumbnail) or 1033 (4.0 BGR-JFIF thumbnail)
        if (res_id == 1036 || res_id == 1033) && data_size > 28 {
            // 28-byte thumbnail resource header followed directly by JPEG stream
            let jpeg_bytes = &res_buf[data_off + 28..data_off + data_size];
            if jpeg_bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
                if let Ok(dyn_img) = crate::image_dec::decode_bytes_strict(jpeg_bytes) {
                    let inner = target_size.max(64);
                    let mut rgba = dyn_img.resize(inner, inner, FilterType::Lanczos3).to_rgba8();
                    // Note: PSD resource 1033 stores BGR instead of RGB inside JPEG
                    if res_id == 1033 {
                        for px in rgba.pixels_mut() {
                            px.0.swap(0, 2);
                        }
                    }
                    return Some(rgba);
                }
            }
        }

        let padded_data = (data_size + 1) & !1;
        pos = data_off + padded_data;
    }
    None
}

fn try_decode_psd_composite(
    stream: &mut dyn ReadSeek,
    version: u16,
    channels: u16,
    height: u32,
    width: u32,
    depth: u16,
    color_mode: u16,
    target_size: u32,
) -> Option<RgbaImage> {
    // Only support standard 8-bit Grayscale (1) or RGB (3) for lightweight composite fallback
    if depth != 8 || (color_mode != 1 && color_mode != 3) {
        return None;
    }
    let ch_needed = if color_mode == 1 { 1 } else { 3 };
    if channels < ch_needed as u16 {
        return None;
    }

    // Skip Layer and Mask Information section
    let lmi_len: u64 = if version == 2 {
        let mut b8 = [0u8; 8];
        stream.read_exact(&mut b8).ok()?;
        u64::from_be_bytes(b8)
    } else {
        let mut b4 = [0u8; 4];
        stream.read_exact(&mut b4).ok()?;
        u32::from_be_bytes(b4) as u64
    };
    if lmi_len > 512 * 1024 * 1024 {
        return None;
    }
    stream.seek(SeekFrom::Current(lmi_len as i64)).ok()?;

    let mut comp_buf = [0u8; 2];
    stream.read_exact(&mut comp_buf).ok()?;
    let compression = u16::from_be_bytes(comp_buf);

    if width == 0 || height == 0 || width > 16_384 || height > 16_384 || channels > 64 {
        return None;
    }
    let pixel_count = (width as usize).checked_mul(height as usize)?;
    if pixel_count == 0 || pixel_count > 64 * 1024 * 1024 {
        return None;
    }

    let mut planes: Vec<Vec<u8>> = Vec::with_capacity(ch_needed);
    if compression == 0 {
        // Raw planar image data
        for _ in 0..ch_needed {
            let mut plane = vec![0u8; pixel_count];
            stream.read_exact(&mut plane).ok()?;
            planes.push(plane);
        }
    } else if compression == 1 {
        // PackBits RLE planar: first comes byte-counts per scanline across all channels
        let total_lines = (height as usize).checked_mul(channels as usize)?;
        let entry_bytes = if version == 2 { 4 } else { 2 };
        let counts_len = total_lines.checked_mul(entry_bytes)?;
        if counts_len > 16 * 1024 * 1024 {
            return None;
        }
        let mut counts_buf = vec![0u8; counts_len];
        stream.read_exact(&mut counts_buf).ok()?;

        let mut scanline_lens = Vec::with_capacity(total_lines);
        for i in 0..total_lines {
            let off = i * entry_bytes;
            let l = if version == 2 {
                be_u32_at(&counts_buf, off)? as usize
            } else {
                be_u16_at(&counts_buf, off)? as usize
            };
            scanline_lens.push(l);
        }

        for c in 0..ch_needed {
            let mut plane = vec![0u8; pixel_count];
            for y in 0..height as usize {
                let comp_len = scanline_lens[c * (height as usize) + y];
                if comp_len > (width as usize) * 4 + 64 {
                    return None;
                }
                let mut rle = vec![0u8; comp_len];
                stream.read_exact(&mut rle).ok()?;
                decode_packbits_line(&rle, &mut plane[y * (width as usize)..(y + 1) * (width as usize)]);
            }
            planes.push(plane);
        }
    } else {
        return None;
    }

    let mut full = RgbaImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let idx = (y as usize) * (width as usize) + (x as usize);
            let (r, g, b) = if color_mode == 1 {
                let v = planes[0][idx];
                (v, v, v)
            } else {
                (planes[0][idx], planes[1][idx], planes[2][idx])
            };
            full.put_pixel(x, y, Rgba([r, g, b, 255]));
        }
    }

    let inner = target_size.max(64);
    let dyn_img = image::DynamicImage::ImageRgba8(full);
    Some(dyn_img.resize(inner, inner, FilterType::Lanczos3).to_rgba8())
}

fn decode_packbits_line(src: &[u8], dst: &mut [u8]) {
    let mut s = 0usize;
    let mut d = 0usize;
    while s < src.len() && d < dst.len() {
        let n = src[s] as i8;
        s += 1;
        if n >= 0 {
            let count = (n as usize) + 1;
            let take = count.min(src.len() - s).min(dst.len() - d);
            dst[d..d + take].copy_from_slice(&src[s..s + take]);
            s += take;
            d += take;
        } else if n != -128 {
            let count = ((-n) as usize) + 1;
            if s < src.len() {
                let val = src[s];
                s += 1;
                let take = count.min(dst.len() - d);
                for slot in &mut dst[d..d + take] {
                    *slot = val;
                }
                d += take;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 4. Plain Text (.txt, .md, syntax-highlighted text/code, and custom extensions)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyntaxFamily {
    Plain,
    Markdown,
    Json,
    Csv,
    Markup,
    Config,
    Log,
    Code,
}

#[derive(Debug, Clone)]
struct TextSpan {
    text: String,
    color: Rgba<u8>,
    bold: bool,
}

const COL_TEXT_DEFAULT: Rgba<u8> = Rgba([38, 43, 50, 255]);
const COL_TEXT_COMMENT: Rgba<u8> = Rgba([16, 128, 48, 255]);
const COL_TEXT_KEYWORD: Rgba<u8> = Rgba([138, 12, 118, 255]);
const COL_TEXT_KEY: Rgba<u8> = Rgba([4, 81, 165, 255]);
const COL_TEXT_STRING: Rgba<u8> = Rgba([178, 28, 28, 255]);
const COL_TEXT_NUMBER: Rgba<u8> = Rgba([9, 134, 88, 255]);
const COL_TEXT_VAR: Rgba<u8> = Rgba([0, 118, 148, 255]);
const COL_TEXT_PUNCT: Rgba<u8> = Rgba([104, 112, 124, 255]);
const COL_TEXT_TAG: Rgba<u8> = Rgba([16, 86, 168, 255]);
const COL_TEXT_ATTR: Rgba<u8> = Rgba([188, 36, 48, 255]);
const COL_TEXT_HEADING: Rgba<u8> = Rgba([20, 86, 184, 255]);
const COL_TEXT_ACCENT: Rgba<u8> = Rgba([196, 92, 16, 255]);
const COL_TEXT_ERROR: Rgba<u8> = Rgba([204, 24, 36, 255]);

fn push_span(out: &mut Vec<TextSpan>, text: String, color: Rgba<u8>, bold: bool) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = out.last_mut() {
        if last.color == color && last.bold == bold {
            last.text.push_str(&text);
            return;
        }
    }
    out.push(TextSpan { text, color, bold });
}

fn detect_syntax_family(clean_ext: &str, text: &str) -> SyntaxFamily {
    match clean_ext {
        "md" | "markdown" | "rst" | "adoc" => SyntaxFamily::Markdown,
        "json" | "jsonc" | "jsonl" => SyntaxFamily::Json,
        "csv" | "tsv" => SyntaxFamily::Csv,
        "html" | "htm" | "xhtml" | "xml" | "svg" | "vue" | "svelte" => SyntaxFamily::Markup,
        "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "env" | "properties" | "reg"
        | "inf" | "editorconfig" => SyntaxFamily::Config,
        "log" => SyntaxFamily::Log,
        "ps1" | "psm1" | "psd1" | "cmd" | "bat" | "sh" | "bash" | "zsh" | "sql" | "py"
        | "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" | "css" | "scss" | "less" | "rs"
        | "c" | "cpp" | "cc" | "cxx" | "h" | "hpp" | "cs" | "java" | "go" | "rb" | "php"
        | "lua" | "r" | "pl" | "swift" | "kt" | "scala" => SyntaxFamily::Code,
        "txt" | "nfo" | "diz" | "srt" | "vtt" | "lrc" => SyntaxFamily::Plain,
        _ => {
            let trimmed = text.trim_start();
            let head_lower: String = trimmed.chars().take(48).collect::<String>().to_ascii_lowercase();
            if (trimmed.starts_with('{') || trimmed.starts_with('[')) && text.contains("\":") {
                SyntaxFamily::Json
            } else if head_lower.starts_with("<!doctype")
                || head_lower.starts_with("<html")
                || head_lower.starts_with("<?xml")
                || head_lower.starts_with("<svg")
            {
                SyntaxFamily::Markup
            } else {
                SyntaxFamily::Plain
            }
        }
    }
}

fn collect_slice(chars: &[char], start: usize, end: usize) -> String {
    chars[start..end].iter().collect()
}

fn tokenize_markdown_inline(chars: &[char], start: usize, out: &mut Vec<TextSpan>) {
    let mut i = start;
    let mut plain_start = start;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '`' {
            if i > plain_start {
                push_span(out, collect_slice(chars, plain_start, i), COL_TEXT_DEFAULT, false);
            }
            if let Some(rel_end) = chars[i + 1..].iter().position(|&c| c == '`') {
                let end = i + 1 + rel_end + 1;
                push_span(out, collect_slice(chars, i, end), COL_TEXT_ATTR, false);
                i = end;
                plain_start = i;
                continue;
            }
        } else if ch == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            if let Some(rel_end) = chars[i + 2..].windows(2).position(|w| w == ['*', '*']) {
                if i > plain_start {
                    push_span(out, collect_slice(chars, plain_start, i), COL_TEXT_DEFAULT, false);
                }
                let end = i + 2 + rel_end + 2;
                push_span(out, collect_slice(chars, i, end), Rgba([28, 56, 118, 255]), true);
                i = end;
                plain_start = i;
                continue;
            }
        } else if ch == '[' {
            if let Some(rel_rb) = chars[i + 1..].iter().position(|&c| c == ']') {
                let rb = i + 1 + rel_rb;
                if rb + 1 < chars.len() && chars[rb + 1] == '(' {
                    if let Some(rel_rp) = chars[rb + 2..].iter().position(|&c| c == ')') {
                        if i > plain_start {
                            push_span(out, collect_slice(chars, plain_start, i), COL_TEXT_DEFAULT, false);
                        }
                        let rp = rb + 2 + rel_rp;
                        push_span(out, collect_slice(chars, i, rb + 1), COL_TEXT_KEY, true);
                        push_span(out, collect_slice(chars, rb + 1, rp + 1), COL_TEXT_NUMBER, false);
                        i = rp + 1;
                        plain_start = i;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    if plain_start < chars.len() {
        push_span(out, collect_slice(chars, plain_start, chars.len()), COL_TEXT_DEFAULT, false);
    }
}

fn tokenize_markdown_line(chars: &[char], out: &mut Vec<TextSpan>) {
    let indent = chars.iter().take_while(|c| c.is_whitespace()).count();
    if indent > 0 {
        push_span(out, collect_slice(chars, 0, indent), COL_TEXT_DEFAULT, false);
    }
    if indent >= chars.len() {
        return;
    }
    let rest = &chars[indent..];
    if rest.starts_with(&['`', '`', '`']) {
        push_span(out, collect_slice(chars, indent, chars.len()), COL_TEXT_KEYWORD, true);
        return;
    }
    if rest[0] == '#' {
        let hashes = rest.iter().take_while(|&&c| c == '#').count();
        if hashes <= 6 && (hashes == rest.len() || rest[hashes].is_whitespace()) {
            push_span(out, collect_slice(chars, indent, indent + hashes), COL_TEXT_ACCENT, true);
            if indent + hashes < chars.len() {
                push_span(out, collect_slice(chars, indent + hashes, chars.len()), COL_TEXT_HEADING, true);
            }
            return;
        }
    }
    if rest[0] == '>' && (rest.len() == 1 || rest[1].is_whitespace()) {
        push_span(out, ">".to_string(), COL_TEXT_ACCENT, true);
        push_span(out, collect_slice(chars, indent + 1, chars.len()), COL_TEXT_VAR, false);
        return;
    }
    if matches!(rest[0], '-' | '*' | '+') && rest.len() >= 2 && rest[1].is_whitespace() {
        push_span(out, collect_slice(chars, indent, indent + 2), COL_TEXT_ACCENT, true);
        tokenize_markdown_inline(chars, indent + 2, out);
        return;
    }
    let digits = rest.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && digits + 1 < rest.len() && rest[digits] == '.' && rest[digits + 1].is_whitespace() {
        push_span(out, collect_slice(chars, indent, indent + digits + 2), COL_TEXT_ACCENT, true);
        tokenize_markdown_inline(chars, indent + digits + 2, out);
        return;
    }
    tokenize_markdown_inline(chars, indent, out);
}

fn tokenize_json_line(chars: &[char], out: &mut Vec<TextSpan>) {
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch.is_whitespace() {
            let start = i;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            push_span(out, collect_slice(chars, start, i), COL_TEXT_DEFAULT, false);
        } else if ch == '"' {
            let start = i;
            i += 1;
            let mut esc = false;
            while i < chars.len() {
                if esc {
                    esc = false;
                } else if chars[i] == '\\' {
                    esc = true;
                } else if chars[i] == '"' {
                    i += 1;
                    break;
                }
                i += 1;
            }
            let mut j = i;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let is_key = j < chars.len() && chars[j] == ':';
            push_span(
                out,
                collect_slice(chars, start, i),
                if is_key { COL_TEXT_KEY } else { COL_TEXT_STRING },
                is_key,
            );
        } else if ch == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            push_span(out, collect_slice(chars, i, chars.len()), COL_TEXT_COMMENT, false);
            break;
        } else if ch.is_ascii_digit()
            || (ch == '-' && i + 1 < chars.len() && chars[i + 1].is_ascii_digit())
        {
            let start = i;
            i += 1;
            while i < chars.len()
                && (chars[i].is_ascii_digit()
                    || matches!(chars[i], '.' | 'e' | 'E' | '+' | '-'))
            {
                i += 1;
            }
            push_span(out, collect_slice(chars, start, i), COL_TEXT_NUMBER, false);
        } else if ch.is_ascii_alphabetic() {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            let word = collect_slice(chars, start, i);
            let is_kw = matches!(word.as_str(), "true" | "false" | "null");
            push_span(
                out,
                word,
                if is_kw { COL_TEXT_KEYWORD } else { COL_TEXT_DEFAULT },
                is_kw,
            );
        } else if matches!(ch, '{' | '}' | '[' | ']' | ':' | ',') {
            push_span(out, ch.to_string(), COL_TEXT_PUNCT, false);
            i += 1;
        } else {
            push_span(out, ch.to_string(), COL_TEXT_DEFAULT, false);
            i += 1;
        }
    }
}

fn tokenize_csv_line(chars: &[char], line_idx: usize, out: &mut Vec<TextSpan>) {
    const CSV_PALETTE: [Rgba<u8>; 6] = [
        Rgba([20, 78, 160, 255]),
        Rgba([18, 120, 72, 255]),
        Rgba([168, 48, 32, 255]),
        Rgba([118, 42, 148, 255]),
        Rgba([165, 92, 12, 255]),
        Rgba([16, 116, 136, 255]),
    ];
    let use_comma = chars.contains(&',') || !chars.contains(&'\t');
    let delim = if use_comma { ',' } else { '\t' };
    let is_header = line_idx == 0;

    let mut i = 0;
    let mut start = 0;
    let mut in_quotes = false;
    let mut col_idx = 0usize;
    while i < chars.len() {
        if chars[i] == '"' {
            if in_quotes && i + 1 < chars.len() && chars[i + 1] == '"' {
                i += 2;
                continue;
            }
            in_quotes = !in_quotes;
        } else if chars[i] == delim && !in_quotes {
            let cell = collect_slice(chars, start, i);
            push_span(out, cell, CSV_PALETTE[col_idx % CSV_PALETTE.len()], is_header);
            push_span(
                out,
                if delim == ',' { ",".to_string() } else { "  ".to_string() },
                COL_TEXT_PUNCT,
                false,
            );
            col_idx += 1;
            start = i + 1;
        }
        i += 1;
    }
    if start < chars.len() {
        let cell = collect_slice(chars, start, chars.len());
        push_span(out, cell, CSV_PALETTE[col_idx % CSV_PALETTE.len()], is_header);
    }
}

fn tokenize_markup_line(chars: &[char], out: &mut Vec<TextSpan>) {
    let mut i = 0;
    let mut text_start = 0;
    while i < chars.len() {
        if chars[i] == '<' {
            if i > text_start {
                push_span(out, collect_slice(chars, text_start, i), COL_TEXT_DEFAULT, false);
            }
            if chars[i..].starts_with(&['<', '!', '-', '-']) {
                let end = chars[i + 4..]
                    .windows(3)
                    .position(|w| w == ['-', '-', '>'])
                    .map(|p| i + 4 + p + 3)
                    .unwrap_or(chars.len());
                push_span(out, collect_slice(chars, i, end), COL_TEXT_COMMENT, false);
                i = end;
                text_start = i;
                continue;
            }
            if i + 1 < chars.len() && matches!(chars[i + 1], '!' | '?') {
                let end = chars[i + 1..]
                    .iter()
                    .position(|&c| c == '>')
                    .map(|p| i + 1 + p + 1)
                    .unwrap_or(chars.len());
                push_span(out, collect_slice(chars, i, end), COL_TEXT_KEYWORD, true);
                i = end;
                text_start = i;
                continue;
            }

            // Opening / closing tag `<tag attr="val">`
            let open_end = if i + 1 < chars.len() && chars[i + 1] == '/' {
                i + 2
            } else {
                i + 1
            };
            push_span(out, collect_slice(chars, i, open_end), COL_TEXT_PUNCT, false);
            i = open_end;

            let name_start = i;
            while i < chars.len() && !chars[i].is_whitespace() && !matches!(chars[i], '>' | '/') {
                i += 1;
            }
            if i > name_start {
                push_span(out, collect_slice(chars, name_start, i), COL_TEXT_TAG, true);
            }

            while i < chars.len() {
                if chars[i] == '>' {
                    push_span(out, ">".to_string(), COL_TEXT_PUNCT, false);
                    i += 1;
                    break;
                }
                if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '>' {
                    push_span(out, "/>".to_string(), COL_TEXT_PUNCT, false);
                    i += 2;
                    break;
                }
                if chars[i].is_whitespace() {
                    let ws_start = i;
                    while i < chars.len() && chars[i].is_whitespace() {
                        i += 1;
                    }
                    push_span(out, collect_slice(chars, ws_start, i), COL_TEXT_DEFAULT, false);
                } else if chars[i] == '"' || chars[i] == '\'' {
                    let quote = chars[i];
                    let q_start = i;
                    i += 1;
                    while i < chars.len() && chars[i] != quote {
                        i += 1;
                    }
                    if i < chars.len() {
                        i += 1;
                    }
                    push_span(out, collect_slice(chars, q_start, i), Rgba([16, 124, 65, 255]), false);
                } else if chars[i] == '=' {
                    push_span(out, "=".to_string(), COL_TEXT_PUNCT, false);
                    i += 1;
                } else {
                    let a_start = i;
                    while i < chars.len()
                        && !chars[i].is_whitespace()
                        && !matches!(chars[i], '=' | '>' | '/' | '"' | '\'')
                    {
                        i += 1;
                    }
                    push_span(out, collect_slice(chars, a_start, i), COL_TEXT_ATTR, false);
                }
            }
            text_start = i;
            continue;
        }
        i += 1;
    }
    if text_start < chars.len() {
        push_span(out, collect_slice(chars, text_start, chars.len()), COL_TEXT_DEFAULT, false);
    }
}

fn tokenize_config_value(chars: &[char], mut i: usize, out: &mut Vec<TextSpan>) {
    while i < chars.len() {
        let ch = chars[i];
        if (ch == '#' || ch == ';') && (i == 0 || chars[i - 1].is_whitespace()) {
            push_span(out, collect_slice(chars, i, chars.len()), COL_TEXT_COMMENT, false);
            return;
        }
        if ch == '"' || ch == '\'' {
            let quote = ch;
            let start = i;
            i += 1;
            let mut esc = false;
            while i < chars.len() {
                if esc {
                    esc = false;
                } else if chars[i] == '\\' && quote == '"' {
                    esc = true;
                } else if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            push_span(out, collect_slice(chars, start, i), COL_TEXT_STRING, false);
        } else if ch.is_whitespace() {
            let start = i;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            push_span(out, collect_slice(chars, start, i), COL_TEXT_DEFAULT, false);
        } else if matches!(ch, '[' | ']' | '{' | '}' | ',' | ':') {
            push_span(out, ch.to_string(), COL_TEXT_PUNCT, false);
            i += 1;
        } else {
            let start = i;
            while i < chars.len()
                && !chars[i].is_whitespace()
                && !matches!(chars[i], '[' | ']' | '{' | '}' | ',' | '"' | '\'' | '#' | ';')
            {
                i += 1;
            }
            let tok = collect_slice(chars, start, i);
            let lower = tok.to_ascii_lowercase();
            let is_val_kw = matches!(lower.as_str(), "true" | "false" | "null" | "yes" | "no" | "on" | "off")
                || tok.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+'));
            push_span(
                out,
                tok,
                if is_val_kw { COL_TEXT_NUMBER } else { COL_TEXT_DEFAULT },
                false,
            );
        }
    }
}

fn tokenize_config_line(chars: &[char], out: &mut Vec<TextSpan>) {
    let indent = chars.iter().take_while(|c| c.is_whitespace()).count();
    if indent > 0 {
        push_span(out, collect_slice(chars, 0, indent), COL_TEXT_DEFAULT, false);
    }
    if indent >= chars.len() {
        return;
    }
    let rest = &chars[indent..];
    if rest[0] == '#' || rest[0] == ';' {
        push_span(out, collect_slice(chars, indent, chars.len()), COL_TEXT_COMMENT, false);
        return;
    }
    if rest[0] == '[' && rest.contains(&']') {
        push_span(out, collect_slice(chars, indent, chars.len()), COL_TEXT_KEYWORD, true);
        return;
    }

    let mut pos = indent;
    if chars[pos] == '-' && pos + 1 < chars.len() && chars[pos + 1].is_whitespace() {
        push_span(out, "- ".to_string(), COL_TEXT_ACCENT, true);
        pos += 2;
    }
    let eq_pos = chars[pos..].iter().position(|&c| c == '=' || c == ':').map(|p| pos + p);
    if let Some(sep) = eq_pos {
        let before_has_quote = chars[pos..sep].iter().any(|&c| matches!(c, '"' | '\''));
        if sep > pos && !before_has_quote {
            push_span(out, collect_slice(chars, pos, sep), COL_TEXT_KEY, true);
            push_span(out, chars[sep].to_string(), COL_TEXT_PUNCT, false);
            tokenize_config_value(chars, sep + 1, out);
            return;
        }
    }
    tokenize_config_value(chars, pos, out);
}

fn tokenize_log_line(chars: &[char], out: &mut Vec<TextSpan>) {
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch.is_whitespace() {
            let start = i;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            push_span(out, collect_slice(chars, start, i), COL_TEXT_DEFAULT, false);
        } else if ch == '[' {
            if let Some(rel_end) = chars[i + 1..].iter().position(|&c| c == ']') {
                let end = i + 1 + rel_end + 1;
                let inner = collect_slice(chars, i + 1, end - 1).to_ascii_uppercase();
                let (col, bold) = if inner.contains("ERR") || inner.contains("FATAL") || inner.contains("FAIL") {
                    (COL_TEXT_ERROR, true)
                } else if inner.contains("WARN") {
                    (COL_TEXT_ACCENT, true)
                } else if inner.contains("INFO") || inner.contains("OK") || inner.contains("PASS") {
                    (COL_TEXT_COMMENT, true)
                } else if inner.contains("DEBUG") || inner.contains("TRACE") {
                    (COL_TEXT_KEYWORD, true)
                } else {
                    (COL_TEXT_VAR, false)
                };
                push_span(out, collect_slice(chars, i, end), col, bold);
                i = end;
            } else {
                push_span(out, "[".to_string(), COL_TEXT_PUNCT, false);
                i += 1;
            }
        } else if ch == '"' || ch == '\'' {
            let quote = ch;
            let start = i;
            i += 1;
            while i < chars.len() && chars[i] != quote {
                i += 1;
            }
            if i < chars.len() {
                i += 1;
            }
            push_span(out, collect_slice(chars, start, i), COL_TEXT_STRING, false);
        } else if i == 0 && ch.is_ascii_digit() {
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() {
                i += 1;
            }
            push_span(out, collect_slice(chars, start, i), COL_TEXT_KEY, false);
        } else if ch.is_ascii_alphanumeric() || ch == '_' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | '-' | '.')) {
                i += 1;
            }
            let tok = collect_slice(chars, start, i);
            let upper = tok.to_ascii_uppercase();
            if matches!(upper.as_str(), "ERROR" | "FATAL" | "CRITICAL" | "FAIL" | "FAILED" | "ERR" | "EXCEPTION") {
                push_span(out, tok, COL_TEXT_ERROR, true);
            } else if matches!(upper.as_str(), "WARN" | "WARNING") {
                push_span(out, tok, COL_TEXT_ACCENT, true);
            } else if matches!(upper.as_str(), "INFO" | "OK" | "PASS" | "PASSED" | "SUCCESS") {
                push_span(out, tok, COL_TEXT_COMMENT, true);
            } else if matches!(upper.as_str(), "DEBUG" | "TRACE") {
                push_span(out, tok, COL_TEXT_KEYWORD, true);
            } else if tok.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | ':' | '-' | '/')) && tok.chars().any(|c| c.is_ascii_digit()) {
                push_span(out, tok, COL_TEXT_NUMBER, false);
            } else {
                push_span(out, tok, COL_TEXT_DEFAULT, false);
            }
        } else {
            push_span(out, ch.to_string(), COL_TEXT_DEFAULT, false);
            i += 1;
        }
    }
}

fn is_code_keyword(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "fn" | "function" | "def" | "class" | "struct" | "enum" | "impl" | "trait" | "pub"
            | "let" | "const" | "var" | "mut" | "use" | "import" | "from" | "export" | "default"
            | "return" | "if" | "else" | "elif" | "elseif" | "then" | "fi" | "for" | "foreach"
            | "in" | "while" | "loop" | "do" | "done" | "match" | "switch" | "case" | "break"
            | "continue" | "try" | "catch" | "finally" | "throw" | "async" | "await" | "yield"
            | "true" | "false" | "null" | "none" | "nil" | "self" | "this" | "super"
            | "param" | "begin" | "process" | "end" | "write-host" | "write-output"
            | "get-childitem" | "set-location" | "echo" | "set" | "goto" | "call" | "exit"
            | "select" | "where" | "insert" | "into" | "values" | "update" | "delete" | "join"
            | "left" | "right" | "inner" | "on" | "create" | "table" | "alter" | "drop"
            | "order" | "group" | "by" | "as" | "and" | "or" | "not" | "mod"
    )
}

fn tokenize_code_line(chars: &[char], clean_ext: &str, out: &mut Vec<TextSpan>) {
    let is_css = matches!(clean_ext, "css" | "scss" | "less");
    let indent = chars.iter().take_while(|c| c.is_whitespace()).count();
    if indent < chars.len() {
        let rest_str: String = chars[indent..].iter().collect();
        let rest_upper = rest_str.to_ascii_uppercase();
        if rest_upper.starts_with("REM ")
            || rest_upper == "REM"
            || rest_upper.starts_with("@REM ")
            || rest_str.starts_with("::")
            || rest_str.starts_with("<!--")
        {
            if indent > 0 {
                push_span(out, collect_slice(chars, 0, indent), COL_TEXT_DEFAULT, false);
            }
            push_span(out, rest_str, COL_TEXT_COMMENT, false);
            return;
        }
    }

    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch.is_whitespace() {
            let start = i;
            while i < chars.len() && chars[i].is_whitespace() {
                i += 1;
            }
            push_span(out, collect_slice(chars, start, i), COL_TEXT_DEFAULT, false);
        } else if (ch == '/' && i + 1 < chars.len() && chars[i + 1] == '/')
            || (ch == '-' && i + 1 < chars.len() && chars[i + 1] == '-' && (i == 0 || chars[i - 1].is_whitespace()))
        {
            push_span(out, collect_slice(chars, i, chars.len()), COL_TEXT_COMMENT, false);
            break;
        } else if ch == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            let end = chars[i + 2..]
                .windows(2)
                .position(|w| w == ['*', '/'])
                .map(|p| i + 2 + p + 2)
                .unwrap_or(chars.len());
            push_span(out, collect_slice(chars, i, end), COL_TEXT_COMMENT, false);
            i = end;
        } else if ch == '#' {
            if is_css && i + 1 < chars.len() && chars[i + 1].is_ascii_hexdigit() {
                let start = i;
                i += 1;
                while i < chars.len() && chars[i].is_ascii_hexdigit() {
                    i += 1;
                }
                push_span(out, collect_slice(chars, start, i), COL_TEXT_NUMBER, false);
            } else {
                push_span(out, collect_slice(chars, i, chars.len()), COL_TEXT_COMMENT, false);
                break;
            }
        } else if ch == '"' || ch == '\'' || ch == '`' {
            let quote = ch;
            let start = i;
            i += 1;
            let mut esc = false;
            while i < chars.len() {
                if esc {
                    esc = false;
                } else if chars[i] == '\\' || (chars[i] == '`' && matches!(clean_ext, "ps1" | "psm1" | "psd1")) {
                    esc = true;
                } else if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            push_span(out, collect_slice(chars, start, i), COL_TEXT_STRING, false);
        } else if ch == '$' || ch == '@' {
            let start = i;
            i += 1;
            if i < chars.len() && chars[i] == '{' {
                while i < chars.len() && chars[i] != '}' {
                    i += 1;
                }
                if i < chars.len() {
                    i += 1;
                }
            } else {
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | ':')) {
                    i += 1;
                }
            }
            push_span(out, collect_slice(chars, start, i), COL_TEXT_VAR, false);
        } else if ch == '%' && i + 1 < chars.len() && (chars[i + 1].is_ascii_alphabetic() || chars[i + 1] == '_') {
            if let Some(rel_end) = chars[i + 1..].iter().position(|&c| c == '%') {
                let end = i + 1 + rel_end + 1;
                push_span(out, collect_slice(chars, i, end), COL_TEXT_VAR, false);
                i = end;
            } else {
                push_span(out, "%".to_string(), COL_TEXT_PUNCT, false);
                i += 1;
            }
        } else if ch.is_ascii_digit() {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '.' | '_'))
            {
                i += 1;
            }
            push_span(out, collect_slice(chars, start, i), COL_TEXT_NUMBER, false);
        } else if ch.is_ascii_alphabetic() || ch == '_' {
            let start = i;
            let allow_hyphen = matches!(clean_ext, "ps1" | "psm1" | "psd1" | "css" | "scss" | "less");
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric()
                    || chars[i] == '_'
                    || (allow_hyphen && chars[i] == '-'))
            {
                i += 1;
            }
            let word = collect_slice(chars, start, i);
            if is_code_keyword(&word) {
                push_span(out, word, COL_TEXT_KEYWORD, true);
            } else {
                let mut look = i;
                while look < chars.len() && chars[look].is_whitespace() {
                    look += 1;
                }
                if is_css && look < chars.len() && chars[look] == ':' {
                    push_span(out, word, COL_TEXT_KEY, false);
                } else if look < chars.len() && chars[look] == '(' {
                    push_span(out, word, COL_TEXT_KEY, true);
                } else {
                    push_span(out, word, COL_TEXT_DEFAULT, false);
                }
            }
        } else if matches!(ch, '{' | '}' | '(' | ')' | '[' | ']' | ':' | ';' | ',' | '=' | '+' | '-' | '*' | '/' | '<' | '>' | '|' | '&' | '!') {
            push_span(out, ch.to_string(), COL_TEXT_PUNCT, false);
            i += 1;
        } else {
            let start = i;
            i += 1;
            while i < chars.len() && !chars[i].is_ascii() && !chars[i].is_whitespace() {
                i += 1;
            }
            push_span(out, collect_slice(chars, start, i), COL_TEXT_DEFAULT, false);
        }
    }
}

fn tokenize_syntax_line(line: &str, family: SyntaxFamily, clean_ext: &str, line_idx: usize) -> Vec<TextSpan> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::with_capacity(8);
    match family {
        SyntaxFamily::Plain => push_span(&mut out, line.to_string(), COL_TEXT_DEFAULT, false),
        SyntaxFamily::Markdown => tokenize_markdown_line(&chars, &mut out),
        SyntaxFamily::Json => tokenize_json_line(&chars, &mut out),
        SyntaxFamily::Csv => tokenize_csv_line(&chars, line_idx, &mut out),
        SyntaxFamily::Markup => tokenize_markup_line(&chars, &mut out),
        SyntaxFamily::Config => tokenize_config_line(&chars, &mut out),
        SyntaxFamily::Log => tokenize_log_line(&chars, &mut out),
        SyntaxFamily::Code => tokenize_code_line(&chars, clean_ext, &mut out),
    }
    out
}

unsafe fn draw_gdi_colored_spans(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    spans: &[TextSpan],
    start_x: i32,
    max_x: i32,
    y: i32,
    line_step: i32,
    body_font: windows::Win32::Graphics::Gdi::HFONT,
    bold_font: windows::Win32::Graphics::Gdi::HFONT,
) {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{
        DrawTextW, GetTextExtentExPointW, SelectObject, SetTextColor, DT_LEFT, DT_NOPREFIX,
        DT_SINGLELINE,
    };

    let mut cur_x = start_x;
    for span in spans {
        if cur_x >= max_x {
            break;
        }
        if span.text.is_empty() {
            continue;
        }
        let avail_w = (max_x - cur_x).max(1);
        let font = if span.bold { bold_font } else { body_font };
        SelectObject(hdc, font);
        SetTextColor(hdc, colorref(span.color));

        let mut utf16: Vec<u16> = span.text.chars().take(120).collect::<String>().encode_utf16().collect();
        if utf16.is_empty() {
            continue;
        }
        let mut fit_count: i32 = utf16.len() as i32;
        let mut sz = windows::Win32::Foundation::SIZE::default();
        let mut dx = vec![0i32; utf16.len()];
        let ok = GetTextExtentExPointW(
            hdc,
            windows::core::PCWSTR(utf16.as_ptr()),
            utf16.len() as i32,
            avail_w,
            Some(&mut fit_count),
            Some(dx.as_mut_ptr()),
            &mut sz,
        );
        let full_fit = !ok.as_bool() || (fit_count as usize) >= utf16.len();
        if !full_fit {
            let take_units = (fit_count as usize).saturating_sub(1);
            let safe_units = if take_units > 0
                && take_units < utf16.len()
                && (0xD800..=0xDBFF).contains(&utf16[take_units - 1])
            {
                take_units - 1
            } else {
                take_units
            };
            let prefix = String::from_utf16_lossy(&utf16[..safe_units]);
            let truncated = if prefix.trim().is_empty() && take_units == 0 {
                "…".to_string()
            } else {
                format!("{}…", prefix.trim_end())
            };
            utf16 = truncated.encode_utf16().collect();
        }

        if !utf16.is_empty() {
            let char_len = utf16.len();
            utf16.push(0);
            let mut rc = RECT {
                left: cur_x,
                top: y,
                right: max_x,
                bottom: y + line_step,
            };
            let _ = DrawTextW(
                hdc,
                &mut utf16[..char_len],
                &mut rc,
                DT_LEFT | DT_SINGLELINE | DT_NOPREFIX,
            );
        }

        if !full_fit {
            break;
        }
        cur_x += sz.cx.max(0);
    }
}

fn render_colorized_text_sheet_card(
    sheet_w: u32,
    sheet_h: u32,
    paper_bg: Rgba<u8>,
    lines: &[String],
    family: SyntaxFamily,
    clean_ext: &str,
) -> RgbaImage {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{
        CreateSolidBrush, DeleteObject, FillRect, SelectObject, SetBkMode, TRANSPARENT,
    };

    let drawn = render_with_gdi_dib_wh(sheet_w, sheet_h, |hdc, w, h| unsafe {
        let paper_brush = CreateSolidBrush(colorref(paper_bg));
        let full_rc = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        FillRect(hdc, &full_rc, paper_brush);
        let _ = DeleteObject(paper_brush);

        SetBkMode(hdc, TRANSPARENT);

        let pad_x = (w / 14).max(7);
        let pad_y = (h / 16).max(8);
        let line_font_h = (h / 16).clamp(9, 15);
        let body_font = create_gdi_font(-line_font_h, false, true);
        let bold_font = create_gdi_font(-line_font_h, true, true);
        let old_font = SelectObject(hdc, body_font);

        let mut y = pad_y;
        let line_step = (line_font_h + 4).max(13);
        let bottom_limit = h - pad_y;
        let max_x = w - pad_x;
        for (line_idx, line) in lines.iter().enumerate() {
            if y + line_step > bottom_limit {
                break;
            }
            if line.trim().is_empty() {
                y += (line_step * 2) / 3;
                continue;
            }
            let spans = tokenize_syntax_line(line, family, clean_ext, line_idx);
            draw_gdi_colored_spans(hdc, &spans, pad_x, max_x, y, line_step, body_font, bold_font);
            y += line_step;
        }
        SelectObject(hdc, old_font);
        let _ = DeleteObject(body_font);
        let _ = DeleteObject(bold_font);
    });

    drawn.unwrap_or_else(|| RgbaImage::from_pixel(sheet_w.max(1), sheet_h.max(1), paper_bg))
}

fn render_text_card(stream: &mut dyn ReadSeek, clean_ext: &str, size: u32) -> RgbaImage {
    let _ = stream.seek(SeekFrom::Start(0));
    let mut raw = Vec::new();
    let _ = stream.take(16 * 1024).read_to_end(&mut raw);
    let raw_text = decode_text_bytes(&raw);
    // Strip control characters (except \r, \n, \t) and BiDi/zero-width overrides before GDI shaping,
    // then mask credentials: the rendered card is persisted by Explorer in thumbcache_*.db.
    let text: String = crate::redact::redact_text_block(&crate::redact::sanitize_display_text(&raw_text));
    let family = detect_syntax_family(clean_ext, &text);
    let non_empty_lines = text.lines().filter(|l| !l.trim().is_empty()).count();
    let keep_single_line = family != SyntaxFamily::Plain && non_empty_lines >= 4;

    let mut lines: Vec<String> = Vec::with_capacity(24);
    for raw_line in text.lines() {
        let expanded = raw_line.replace('\t', "    ");
        if keep_single_line {
            lines.push(expanded.chars().take(80).collect());
        } else {
            let char_count = expanded.chars().count();
            if char_count <= 42 {
                lines.push(expanded);
            } else {
                let mut chunk = String::with_capacity(36);
                for (idx, ch) in expanded.chars().enumerate() {
                    if idx > 0 && idx % 36 == 0 {
                        lines.push(std::mem::take(&mut chunk));
                        if lines.len() >= 24 {
                            break;
                        }
                    }
                    chunk.push(ch);
                }
                if !chunk.is_empty() && lines.len() < 24 {
                    lines.push(chunk);
                }
            }
        }
        if lines.len() >= 24 {
            break;
        }
    }

    let (frame_bg, accent_line) = match family {
        SyntaxFamily::Markdown => (Rgba([62, 74, 168, 255]), Rgba([120, 136, 252, 255])),
        SyntaxFamily::Json | SyntaxFamily::Config => (Rgba([54, 72, 96, 255]), Rgba([112, 160, 220, 255])),
        SyntaxFamily::Csv => (Rgba([56, 86, 78, 255]), Rgba([118, 182, 160, 255])),
        SyntaxFamily::Markup => (Rgba([68, 68, 108, 255]), Rgba([146, 148, 224, 255])),
        SyntaxFamily::Code | SyntaxFamily::Log => (Rgba([56, 62, 72, 255]), Rgba([128, 152, 184, 255])),
        SyntaxFamily::Plain => (Rgba([76, 82, 88, 255]), Rgba([168, 176, 184, 255])),
    };

    let fallback_empty = vec![crate::i18n::t("doc.empty_text_file")];
    let display_lines = if lines.is_empty() {
        &fallback_empty
    } else {
        &lines
    };

    let (avail_w, avail_h) = program_frame_inner_bounds(size);
    let sheet_w = ((avail_w as f32 * 0.80).round() as u32).clamp(32, avail_w);
    let sheet = if family == SyntaxFamily::Plain {
        render_document_sheet_card(
            sheet_w,
            avail_h,
            frame_bg,
            Rgba([250, 251, 252, 255]),
            COL_TEXT_DEFAULT,
            display_lines,
            true,
        )
    } else {
        render_colorized_text_sheet_card(
            sheet_w,
            avail_h,
            Rgba([250, 251, 252, 255]),
            display_lines,
            family,
            clean_ext,
        )
    };
    apply_program_frame(&sheet, size, frame_bg, accent_line, FoldCorner::TopRight)
}

fn decode_with_codepage(
    bytes: &[u8],
    codepage: u32,
    strict: bool,
) -> Option<String> {
    use windows::Win32::Globalization::{
        MultiByteToWideChar, MB_ERR_INVALID_CHARS, MULTI_BYTE_TO_WIDE_CHAR_FLAGS,
    };
    if bytes.is_empty() {
        return Some(String::new());
    }
    let flags = if strict {
        MB_ERR_INVALID_CHARS
    } else {
        MULTI_BYTE_TO_WIDE_CHAR_FLAGS(0)
    };
    unsafe {
        let needed = MultiByteToWideChar(codepage, flags, bytes, None);
        if needed <= 0 {
            return None;
        }
        let mut wide = vec![0u16; needed as usize];
        let written = MultiByteToWideChar(codepage, flags, bytes, Some(&mut wide));
        if written > 0 {
            Some(String::from_utf16_lossy(&wide[..written as usize]))
        } else {
            None
        }
    }
}

fn decode_text_bytes(raw: &[u8]) -> String {
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&raw[3..]).into_owned();
    }
    if raw.starts_with(&[0xFF, 0xFE]) && raw.len() >= 2 {
        let units: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if raw.starts_with(&[0xFE, 0xFF]) && raw.len() >= 2 {
        let units: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }

    // Truncate to a clean line boundary within the first 4 KB so preview extraction
    // never splits a trailing DBCS character in half or fails due to later bytes.
    let head = if raw.len() > 4096 {
        let cap = &raw[..4096];
        if let Some(pos) = cap.iter().rposition(|&b| b == b'\n') {
            if pos > 0 {
                &cap[..pos]
            } else {
                cap
            }
        } else if cap.last().copied().unwrap_or(0) >= 0x80 {
            &cap[..cap.len() - 1]
        } else {
            cap
        }
    } else {
        raw
    };

    if let Ok(s) = std::str::from_utf8(head) {
        return s.to_string();
    }

    // Build ordered candidate codepages (avoiding 65001/UTF-8 so Beta UTF-8 Windows
    // properly falls back to Big5 / GBK / Shift-JIS / EUC-KR for legacy ANSI text files).
    use windows::Win32::Globalization::{GetACP, GetUserDefaultUILanguage, CP_ACP};
    let (acp, ui_lang) = unsafe { (GetACP(), GetUserDefaultUILanguage()) };
    let primary_lang = ui_lang & 0x03FF;
    let sub_lang = ui_lang >> 10;

    let mut candidates = Vec::with_capacity(6);
    if acp != 0 && acp != 65001 {
        candidates.push(acp);
    }
    let ordered_cjk: &[u32] = match primary_lang {
        0x04 => match sub_lang {
            // zh-TW (1), zh-HK (3), zh-MO (5)
            0x01 | 0x03 | 0x05 => &[950, 936, 932, 949, 1252],
            // zh-CN (2), zh-SG (4)
            _ => &[936, 950, 932, 949, 1252],
        },
        0x11 => &[932, 950, 936, 949, 1252], // Japanese
        0x12 => &[949, 950, 936, 932, 1252], // Korean
        _ => &[950, 936, 932, 949, 1252],
    };
    for &cp in ordered_cjk {
        if !candidates.contains(&cp) {
            candidates.push(cp);
        }
    }

    for &cp in &candidates {
        if let Some(decoded) = decode_with_codepage(head, cp, true) {
            return decoded;
        }
    }

    // Lenient fallback using the primary CJK / ACP codepage before UTF-8 lossy
    if let Some(&primary_cp) = candidates.first() {
        if let Some(decoded) = decode_with_codepage(head, primary_cp, false) {
            return decoded;
        }
    }
    if let Some(decoded) = decode_with_codepage(head, CP_ACP, false) {
        return decoded;
    }

    String::from_utf8_lossy(head).into_owned()
}

/// Checks if the stream appears to contain readable plain text (UTF-8, UTF-16 BOM, or ANSI/CJK)
/// rather than binary data or a corrupted archive.
pub fn is_likely_plain_text_stream(stream: &mut dyn ReadSeek) -> bool {
    if stream.seek(SeekFrom::Start(0)).is_err() {
        return false;
    }
    let mut raw_buf = [0u8; 4096];
    let mut filled = 0usize;
    while filled < raw_buf.len() {
        match stream.read(&mut raw_buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => {
                let _ = stream.seek(SeekFrom::Start(0));
                return false;
            }
        }
    }
    let _ = stream.seek(SeekFrom::Start(0));
    let buf = &raw_buf[..filled];

    if buf.is_empty() {
        return false;
    }

    // Reject known binary magic signatures immediately
    let known_binary = buf.starts_with(b"PK\x03\x04")
        || buf.starts_with(b"PK\x05\x06")
        || buf.starts_with(b"Rar!")
        || buf.starts_with(b"7z\xBC\xAF\x27\x1C")
        || buf.starts_with(b"%PDF")
        || buf.starts_with(b"\x89PNG")
        || buf.starts_with(&[0xFF, 0xD8, 0xFF])
        || buf.starts_with(b"GIF8")
        || buf.starts_with(b"RIFF")
        || buf.starts_with(b"\x7FELF")
        || buf.starts_with(b"MZ")
        || buf.starts_with(b"OggS")
        || buf.starts_with(b"fLaC")
        || buf.starts_with(b"ID3")
        || buf.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
        || buf.starts_with(b"8BPS")
        || buf.starts_with(&[0xD0, 0xCF, 0x11, 0xE0])
        || (buf.len() >= 8 && &buf[4..8] == b"ftyp");
    if known_binary {
        return false;
    }

    // Accept explicit Unicode BOMs
    if buf.starts_with(&[0xEF, 0xBB, 0xBF])
        || (buf.len() >= 2 && (buf.starts_with(&[0xFF, 0xFE]) || buf.starts_with(&[0xFE, 0xFF])))
    {
        return true;
    }

    // Non-UTF-16 streams with NUL bytes are almost certainly binary
    if buf.contains(&0x00) {
        return false;
    }

    // Count binary control bytes (excluding tab \t, LF \n, FF \x0C, CR \r)
    let control_count = buf
        .iter()
        .filter(|&&b| matches!(b, 0x01..=0x08 | 0x0B | 0x0E..=0x1F))
        .count();
    if control_count * 50 > buf.len() {
        return false;
    }

    let decoded = decode_text_bytes(&buf);
    let total_chars = decoded.chars().count();
    if !decoded.chars().any(|c| !c.is_whitespace()) {
        return false;
    }
    let bad_chars = decoded.chars().filter(|&c| c == '\u{FFFD}').count();
    bad_chars < total_chars / 20 + 1
}

// ---------------------------------------------------------------------------
// GDI Stylized Document / Excel / Program Frame Rendering
// ---------------------------------------------------------------------------

fn colorref(c: Rgba<u8>) -> windows::Win32::Foundation::COLORREF {
    // Win32 COLORREF is 0x00BBGGRR
    windows::Win32::Foundation::COLORREF(
        ((c[2] as u32) << 16) | ((c[1] as u32) << 8) | (c[0] as u32),
    )
}

fn program_frame_inner_bounds(size: u32) -> (u32, u32) {
    let border = (size as f32 * 0.052).round().clamp(4.0, 12.0) as u32;
    let avail_w = size.saturating_sub(border * 2).max(16);
    let avail_h = size.saturating_sub(border * 2).max(16);
    (avail_w, avail_h)
}

fn render_with_gdi_dib_wh<F>(width: u32, height: u32, draw_fn: F) -> Option<RgbaImage>
where
    F: FnOnce(windows::Win32::Graphics::Gdi::HDC, i32, i32),
{
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };

    if width == 0 || height == 0 {
        return None;
    }

    unsafe {
        let hdc_screen = windows::Win32::Graphics::Gdi::GetDC(None);
        let hdc_mem = CreateCompatibleDC(hdc_screen);
        let _ = windows::Win32::Graphics::Gdi::ReleaseDC(None, hdc_screen);
        if hdc_mem.is_invalid() {
            return None;
        }

        let mut bmi = BITMAPINFO::default();
        bmi.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32), // top-down DIB
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };

        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let hbm = CreateDIBSection(hdc_mem, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        if hbm.is_invalid() || bits.is_null() {
            let _ = DeleteDC(hdc_mem);
            return None;
        }

        let old_bm = SelectObject(hdc_mem, hbm);
        draw_fn(hdc_mem, width as i32, height as i32);

        let pixel_bytes = (width as usize) * (height as usize) * 4;
        let bgra = std::slice::from_raw_parts(bits as *const u8, pixel_bytes);
        let mut rgba = RgbaImage::new(width, height);
        for (i, chunk) in bgra.chunks_exact(4).enumerate() {
            let x = (i % width as usize) as u32;
            let y = (i / width as usize) as u32;
            // GDI leaves alpha byte 0 on text/fills unless explicitly alpha-drawn -> force opaque 255
            rgba.put_pixel(x, y, Rgba([chunk[2], chunk[1], chunk[0], 255]));
        }

        SelectObject(hdc_mem, old_bm);
        let _ = DeleteObject(hbm);
        let _ = DeleteDC(hdc_mem);
        Some(rgba)
    }
}

/// Safely draws a single line of text via GDI with Rust-side pixel-width ellipsis truncation.
/// Never passes an empty slice or `DT_END_ELLIPSIS` (which can trigger buffer/null-ptr crashes in USER32!DrawTextW).
unsafe fn draw_gdi_text_line(
    hdc: windows::Win32::Graphics::Gdi::HDC,
    text: &str,
    rc: &mut windows::Win32::Foundation::RECT,
    flags: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
) {
    use windows::Win32::Graphics::Gdi::{DrawTextW, GetTextExtentExPointW, DT_END_ELLIPSIS};

    let cleaned = text.trim_end_matches(&['\r', '\n']);
    if cleaned.trim().is_empty() {
        return;
    }
    let max_w = (rc.right - rc.left).max(1);
    let mut utf16: Vec<u16> = cleaned.chars().take(120).collect::<String>().encode_utf16().collect();
    if utf16.is_empty() {
        return;
    }

    let mut fit_count: i32 = utf16.len() as i32;
    let mut sz = windows::Win32::Foundation::SIZE::default();
    let mut dx = vec![0i32; utf16.len()];
    let ok = GetTextExtentExPointW(
        hdc,
        windows::core::PCWSTR(utf16.as_ptr()),
        utf16.len() as i32,
        max_w,
        Some(&mut fit_count),
        Some(dx.as_mut_ptr()),
        &mut sz,
    );
    if ok.as_bool() && (fit_count as usize) < utf16.len() {
        let take_units = (fit_count as usize).saturating_sub(1);
        let safe_units = if take_units > 0
            && take_units < utf16.len()
            && (0xD800..=0xDBFF).contains(&utf16[take_units - 1])
        {
            take_units - 1
        } else {
            take_units
        };
        let prefix = String::from_utf16_lossy(&utf16[..safe_units]);
        let truncated = format!("{}…", prefix.trim_end());
        utf16 = truncated.encode_utf16().collect();
    }

    if utf16.is_empty() {
        return;
    }
    let char_len = utf16.len();
    utf16.push(0); // extra null sentinel safety
    let _ = DrawTextW(
        hdc,
        &mut utf16[..char_len],
        rc,
        flags & !DT_END_ELLIPSIS,
    );
}

fn create_gdi_font(height: i32, bold: bool, monospace: bool) -> windows::Win32::Graphics::Gdi::HFONT {
    use windows::Win32::Graphics::Gdi::{
        CreateFontW, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH,
        FF_DONTCARE, OUT_DEFAULT_PRECIS,
    };
    let face_name = if monospace {
        "Consolas\0"
    } else {
        "Microsoft JhengHei UI\0"
    };
    let face_w: Vec<u16> = face_name.encode_utf16().collect();
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            if bold { 700 } else { 400 },
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            windows::core::PCWSTR(face_w.as_ptr()),
        )
    }
}

fn render_document_sheet_card(
    sheet_w: u32,
    sheet_h: u32,
    _accent_top: Rgba<u8>,
    paper_bg: Rgba<u8>,
    text_fg: Rgba<u8>,
    lines: &[String],
    monospace: bool,
) -> RgbaImage {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{
        CreateSolidBrush, DeleteObject, FillRect, SelectObject, SetBkMode, SetTextColor, DT_LEFT,
        DT_NOPREFIX, DT_SINGLELINE, TRANSPARENT,
    };

    let drawn = render_with_gdi_dib_wh(sheet_w, sheet_h, |hdc, w, h| unsafe {
        let paper_brush = CreateSolidBrush(colorref(paper_bg));
        let full_rc = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        FillRect(hdc, &full_rc, paper_brush);
        let _ = DeleteObject(paper_brush);

        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, colorref(text_fg));

        let pad_x = (w / 14).max(7);
        let pad_y = (h / 16).max(8);
        let line_font_h = (h / 16).clamp(9, 15);
        let body_font = create_gdi_font(-line_font_h, false, monospace);
        let old_body_font = SelectObject(hdc, body_font);

        let mut y = pad_y;
        let line_step = (line_font_h + 4).max(13);
        let bottom_limit = h - pad_y;
        for line in lines {
            if y + line_step > bottom_limit {
                break;
            }
            if line.trim().is_empty() {
                y += (line_step * 2) / 3;
                continue;
            }
            let mut rc = RECT {
                left: pad_x,
                top: y,
                right: w - pad_x,
                bottom: y + line_step,
            };
            draw_gdi_text_line(hdc, line, &mut rc, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX);
            y += line_step;
        }
        SelectObject(hdc, old_body_font);
        let _ = DeleteObject(body_font);
    });

    drawn.unwrap_or_else(|| RgbaImage::from_pixel(sheet_w.max(1), sheet_h.max(1), paper_bg))
}

fn render_excel_grid_card(sheet_w: u32, sheet_h: u32, rows: &[Vec<String>]) -> RgbaImage {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Graphics::Gdi::{
        CreatePen, CreateSolidBrush, DeleteObject, FillRect, LineTo, MoveToEx, SelectObject,
        SetBkMode, SetTextColor, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, PS_SOLID,
        TRANSPARENT,
    };

    let drawn = render_with_gdi_dib_wh(sheet_w, sheet_h, |hdc, w, h| unsafe {
        let sheet_br = CreateSolidBrush(colorref(Rgba([252, 253, 252, 255])));
        let full = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        };
        FillRect(hdc, &full, sheet_br);
        let _ = DeleteObject(sheet_br);

        // Column header band A | B | C flush inside frame (no XLSX header bar or outer border)
        let row_h = (h / 9).max(16);
        let col_hdr_br = CreateSolidBrush(colorref(Rgba([230, 240, 234, 255])));
        let col_hdr_rc = RECT {
            left: 0,
            top: 0,
            right: w,
            bottom: row_h,
        };
        FillRect(hdc, &col_hdr_rc, col_hdr_br);
        let _ = DeleteObject(col_hdr_br);

        let col_count = 3i32;
        let col_w = (w / col_count).max(18);

        let grid_pen = CreatePen(PS_SOLID, 1, colorref(Rgba([206, 220, 212, 255])));
        let old_gp = SelectObject(hdc, grid_pen);
        for c in 1..col_count {
            let gx = c * col_w;
            let _ = MoveToEx(hdc, gx, 0, None);
            let _ = LineTo(hdc, gx, h);
        }
        let mut gy = row_h;
        while gy < h {
            let _ = MoveToEx(hdc, 0, gy, None);
            let _ = LineTo(hdc, w, gy);
            gy += row_h;
        }
        SelectObject(hdc, old_gp);
        let _ = DeleteObject(grid_pen);

        SetBkMode(hdc, TRANSPARENT);
        let cell_font = create_gdi_font(-(row_h * 56 / 100).clamp(8, 13), false, false);
        let old_cf = SelectObject(hdc, cell_font);
        SetTextColor(hdc, colorref(Rgba([45, 88, 64, 255])));
        for (c_idx, label) in ["A", "B", "C"].iter().enumerate() {
            let mut rc = RECT {
                left: 6 + (c_idx as i32) * col_w,
                top: 0,
                right: ((c_idx as i32 + 1) * col_w) - 4,
                bottom: row_h,
            };
            draw_gdi_text_line(hdc, label, &mut rc, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
        }

        SetTextColor(hdc, colorref(Rgba([32, 36, 34, 255])));
        for (r_idx, row) in rows.iter().take(9).enumerate() {
            let ry = ((r_idx as i32) + 1) * row_h;
            if ry + (row_h / 2) > h {
                break;
            }
            for (c_idx, cell) in row.iter().take(3).enumerate() {
                if cell.trim().is_empty() {
                    continue;
                }
                let mut rc = RECT {
                    left: 5 + (c_idx as i32) * col_w,
                    top: ry,
                    right: ((c_idx as i32 + 1) * col_w) - 4,
                    bottom: ry + row_h,
                };
                draw_gdi_text_line(
                    hdc,
                    cell,
                    &mut rc,
                    DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_VCENTER,
                );
            }
        }
        SelectObject(hdc, old_cf);
        let _ = DeleteObject(cell_font);
    });

    drawn.unwrap_or_else(|| RgbaImage::from_pixel(sheet_w.max(1), sheet_h.max(1), Rgba([252, 253, 252, 255])))
}

// ---------------------------------------------------------------------------
// 4b. BitTorrent (.torrent) Bencode Parser & File-List Card
// ---------------------------------------------------------------------------

const MAX_TORRENT_BYTES: usize = 16 * 1024 * 1024;
const MAX_BENCODE_DEPTH: usize = 64;
/// Upper bound on bencode nodes materialised from one .torrent (real torrents: a few thousand).
const MAX_BENCODE_NODES: usize = 200_000;

enum BencodeVal<'a> {
    // Value parsed for validation only; kept for future use (e.g. file sizes).
    Int(#[allow(dead_code)] i64),
    Bytes(&'a [u8]),
    List(Vec<BencodeVal<'a>>),
    Dict(Vec<(&'a [u8], BencodeVal<'a>)>),
}

/// Parses one bencode value. `budget` is the number of nodes still allowed to be materialised:
/// without it, a 16 MiB file of `lelele...` or `i0ei0e...` expands into millions of heap nodes
/// (hundreds of MiB inside Explorer) even though every individual value is tiny.
fn parse_bencode<'a>(
    data: &'a [u8],
    pos: &mut usize,
    depth: usize,
    budget: &mut usize,
) -> Option<BencodeVal<'a>> {
    if depth > MAX_BENCODE_DEPTH || *pos >= data.len() || *budget == 0 {
        return None;
    }
    *budget -= 1;
    match data[*pos] {
        b'i' => {
            *pos += 1;
            let start = *pos;
            while *pos < data.len() && data[*pos] != b'e' {
                *pos += 1;
            }
            if *pos >= data.len() {
                return None;
            }
            if *pos - start > 20 {
                return None;
            }
            let s = std::str::from_utf8(&data[start..*pos]).ok()?;
            let n = s.parse::<i64>().ok()?;
            *pos += 1;
            Some(BencodeVal::Int(n))
        }
        b'l' => {
            *pos += 1;
            let mut items = Vec::new();
            while *pos < data.len() && data[*pos] != b'e' {
                items.push(parse_bencode(data, pos, depth + 1, budget)?);
            }
            if *pos >= data.len() {
                return None;
            }
            *pos += 1;
            Some(BencodeVal::List(items))
        }
        b'd' => {
            *pos += 1;
            let mut pairs = Vec::new();
            while *pos < data.len() && data[*pos] != b'e' {
                let BencodeVal::Bytes(key) = parse_bencode(data, pos, depth + 1, budget)? else {
                    return None;
                };
                let val = parse_bencode(data, pos, depth + 1, budget)?;
                pairs.push((key, val));
            }
            if *pos >= data.len() {
                return None;
            }
            *pos += 1;
            Some(BencodeVal::Dict(pairs))
        }
        b'0'..=b'9' => {
            let start = *pos;
            while *pos < data.len() && data[*pos] != b':' {
                if !data[*pos].is_ascii_digit() || *pos - start > 20 {
                    return None;
                }
                *pos += 1;
            }
            if *pos >= data.len() {
                return None;
            }
            let len = std::str::from_utf8(&data[start..*pos]).ok()?.parse::<usize>().ok()?;
            *pos += 1;
            let end = (*pos).checked_add(len)?;
            if end > data.len() {
                return None;
            }
            let slice = &data[*pos..end];
            *pos = end;
            Some(BencodeVal::Bytes(slice))
        }
        _ => None,
    }
}

fn bencode_dict_get<'a>(pairs: &'a [(&'a [u8], BencodeVal<'a>)], key: &[u8]) -> Option<&'a BencodeVal<'a>> {
    pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| v)
}

#[derive(Debug, Clone)]
pub(crate) struct TorrentDisplayRow {
    pub(crate) is_dir: bool,
    pub(crate) indent: u8,
    pub(crate) label: String,
    pub(crate) ext_lower: String,
}

pub(crate) fn parse_torrent_rows(bytes: &[u8]) -> Vec<TorrentDisplayRow> {
    let mut pos = 0usize;
    let mut budget = MAX_BENCODE_NODES;
    let Some(BencodeVal::Dict(root)) = parse_bencode(bytes, &mut pos, 0, &mut budget) else {
        return Vec::new();
    };
    let Some(BencodeVal::Dict(info)) = bencode_dict_get(&root, b"info") else {
        return Vec::new();
    };

    // Torrent names are attacker-chosen: strip RTLO/zero-width/control characters so a listing
    // cannot visually disguise `photo\u{202E}gpj.exe` as `photoexe.jpg`.
    let root_name = match bencode_dict_get(info, b"name.utf-8").or_else(|| bencode_dict_get(info, b"name")) {
        Some(BencodeVal::Bytes(b)) => crate::redact::sanitize_label(&String::from_utf8_lossy(b)),
        _ => String::from("Torrent"),
    };

    let mut rows = Vec::new();
    if let Some(BencodeVal::List(files)) = bencode_dict_get(info, b"files") {
        rows.push(TorrentDisplayRow {
            is_dir: true,
            indent: 0,
            label: root_name,
            ext_lower: String::new(),
        });

        let mut file_paths: Vec<String> = Vec::new();
        for item in files {
            if let BencodeVal::Dict(fd) = item {
                if let Some(BencodeVal::List(segs)) =
                    bencode_dict_get(fd, b"path.utf-8").or_else(|| bencode_dict_get(fd, b"path"))
                {
                    let parts: Vec<String> = segs
                        .iter()
                        .filter_map(|s| match s {
                            BencodeVal::Bytes(b) => Some(crate::redact::sanitize_label(&String::from_utf8_lossy(b))),
                            _ => None,
                        })
                        .collect();
                    if !parts.is_empty() {
                        file_paths.push(parts.join("/"));
                    }
                }
            }
        }
        file_paths.sort_by(|a, b| a.to_ascii_lowercase().cmp(&b.to_ascii_lowercase()));

        let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
        for rel in file_paths {
            let parts: Vec<&str> = rel.split('/').filter(|s| !s.is_empty()).collect();
            if parts.len() > 1 {
                let top_sub = parts[0].to_string();
                if seen_dirs.insert(top_sub.clone()) {
                    rows.push(TorrentDisplayRow {
                        is_dir: true,
                        indent: 1,
                        label: top_sub,
                        ext_lower: String::new(),
                    });
                }
            }
            if let Some(fname) = parts.last() {
                let ext_lower = fname
                    .rsplit('.')
                    .next()
                    .filter(|_| fname.contains('.'))
                    .map(|e| e.to_ascii_lowercase())
                    .unwrap_or_default();
                rows.push(TorrentDisplayRow {
                    is_dir: false,
                    indent: if parts.len() > 1 { 2 } else { 1 },
                    label: (*fname).to_string(),
                    ext_lower,
                });
            }
            if rows.len() >= 20 {
                break;
            }
        }
    } else {
        let ext_lower = root_name
            .rsplit('.')
            .next()
            .filter(|_| root_name.contains('.'))
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_default();
        rows.push(TorrentDisplayRow {
            is_dir: false,
            indent: 0,
            label: root_name,
            ext_lower,
        });
    }
    rows
}

fn fill_rect_rgba(img: &mut RgbaImage, x0: i32, y0: i32, w: i32, h: i32, col: Rgba<u8>) {
    let iw = img.width() as i32;
    let ih = img.height() as i32;
    for y in y0.max(0)..(y0 + h).min(ih) {
        for x in x0.max(0)..(x0 + w).min(iw) {
            img.put_pixel(x as u32, y as u32, col);
        }
    }
}

fn draw_torrent_row_icon(img: &mut RgbaImage, x: i32, y: i32, icon_sz: i32, row: &TorrentDisplayRow) {
    let s = icon_sz.max(10);
    if row.is_dir {
        let tab_w = (s * 45) / 100;
        let tab_h = (s * 18 / 100).max(2);
        let body_y = y + tab_h;
        let body_h = (s - tab_h - 1).max(6);
        fill_rect_rgba(img, x, y, tab_w, tab_h + 1, Rgba([226, 170, 42, 255]));
        fill_rect_rgba(img, x, body_y, s, body_h, Rgba([244, 198, 68, 255]));
        fill_rect_rgba(img, x, body_y, s, 1, Rgba([255, 224, 120, 255]));
        return;
    }

    let badge_col = match row.ext_lower.as_str() {
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "m4v" | "ts" | "m2ts" | "3gp" => {
            Rgba([146, 54, 184, 255]) // Video purple
        }
        "jpg" | "jpeg" | "png" | "webp" | "gif" | "avif" | "bmp" | "jxl" => {
            Rgba([46, 156, 178, 255]) // Image teal
        }
        "zip" | "rar" | "7z" | "tar" | "cbz" | "cbr" | "cb7" | "cbt" => {
            Rgba([206, 134, 42, 255]) // Archive amber
        }
        "mp3" | "flac" | "m4a" | "aac" | "ogg" | "opus" | "wav" | "ape" => {
            Rgba([54, 168, 108, 255]) // Audio green
        }
        _ => Rgba([130, 142, 160, 255]),
    };

    // Clean rounded/beveled file badge icon
    let top_h = (s * 28 / 100).max(3);
    fill_rect_rgba(img, x + 1, y, s - 2, top_h, Rgba([220, 226, 236, 255]));
    fill_rect_rgba(img, x + 1, y + top_h, s - 2, s - top_h, badge_col);
    fill_rect_rgba(img, x + 3, y + top_h + 2, (s - 6).max(2), 1, Rgba([245, 248, 255, 210]));
}

fn render_torrent_card(stream: &mut dyn ReadSeek, size: u32) -> RgbaImage {
    let _ = stream.seek(SeekFrom::Start(0));
    let mut raw = Vec::new();
    let _ = stream.take(MAX_TORRENT_BYTES as u64).read_to_end(&mut raw);
    let mut rows = parse_torrent_rows(&raw);
    if rows.is_empty() {
        rows.push(TorrentDisplayRow {
            is_dir: false,
            indent: 0,
            label: "BitTorrent Metadata (.torrent)".to_string(),
            ext_lower: String::from("torrent"),
        });
    }

    let frame_bg = Rgba([32, 42, 60, 255]);
    let accent_line = Rgba([66, 148, 245, 255]);
    let sheet_bg = Rgba([30, 32, 36, 255]);
    let (avail_w, avail_h) = program_frame_inner_bounds(size);
    let sheet_w = avail_w.max(32);
    let sheet_h = avail_h.max(32);

    let pad_x = ((sheet_w as f32 * 0.08).round() as i32).max(6);
    let pad_y = ((sheet_h as f32 * 0.08).round() as i32).max(6);
    let line_h = ((sheet_h as f32 * 0.095).round() as i32).clamp(11, 20);
    let font_h = -((line_h as f32 * 0.78).round() as i32).clamp(8, 16);
    let icon_sz = (line_h - 3).clamp(9, 15);
    let indent_step = (icon_sz / 2 + 4).max(8);

    // Draw text rows via GDI onto dark sheet background, then composite mini icons
    let drawn = unsafe {
        use windows::Win32::Foundation::{COLORREF, RECT};
        use windows::Win32::Graphics::Gdi::{
            CreateCompatibleDC, CreateDIBSection, CreateFontW, CreateSolidBrush, DeleteDC,
            DeleteObject, FillRect, SelectObject, SetBkMode, SetTextColor, BITMAPINFO,
            BITMAPINFOHEADER, BI_RGB, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_LEFT, DT_NOPREFIX,
            DT_SINGLELINE, DT_VCENTER, FW_NORMAL, FW_SEMIBOLD, TRANSPARENT,
        };

        let hdc_mem = CreateCompatibleDC(None);
        if hdc_mem.is_invalid() {
            None
        } else {
            let mut bi = BITMAPINFO::default();
            bi.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: sheet_w as i32,
                biHeight: -(sheet_h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let hbm = CreateDIBSection(hdc_mem, &bi, DIB_RGB_COLORS, &mut bits, None, 0);
            if let Ok(hbm) = hbm {
                if !bits.is_null() {
                    let old_bm = SelectObject(hdc_mem, hbm);
                    let bg_ref = COLORREF(
                        ((sheet_bg[2] as u32) << 16)
                            | ((sheet_bg[1] as u32) << 8)
                            | (sheet_bg[0] as u32),
                    );
                    let bg_br = CreateSolidBrush(bg_ref);
                    let full_rc = RECT {
                        left: 0,
                        top: 0,
                        right: sheet_w as i32,
                        bottom: sheet_h as i32,
                    };
                    let _ = FillRect(hdc_mem, &full_rc, bg_br);
                    let _ = DeleteObject(bg_br);
                    SetBkMode(hdc_mem, TRANSPARENT);

                    let face: Vec<u16> = "Segoe UI\0".encode_utf16().collect();
                    let hfont_bold = CreateFontW(
                        font_h,
                        0,
                        0,
                        0,
                        FW_SEMIBOLD.0 as i32,
                        0,
                        0,
                        0,
                        DEFAULT_CHARSET.0 as u32,
                        0,
                        0,
                        5,
                        0,
                        windows::core::PCWSTR(face.as_ptr()),
                    );
                    let hfont_norm = CreateFontW(
                        font_h,
                        0,
                        0,
                        0,
                        FW_NORMAL.0 as i32,
                        0,
                        0,
                        0,
                        DEFAULT_CHARSET.0 as u32,
                        0,
                        0,
                        5,
                        0,
                        windows::core::PCWSTR(face.as_ptr()),
                    );
                    let old_font = SelectObject(hdc_mem, hfont_norm);

                    let mut cur_y = pad_y;
                    for row in rows.iter() {
                        if cur_y + line_h > sheet_h as i32 - pad_y {
                            break;
                        }
                        if row.is_dir {
                            SelectObject(hdc_mem, hfont_bold);
                            SetTextColor(hdc_mem, COLORREF(0x00F4F2F0));
                        } else {
                            SelectObject(hdc_mem, hfont_norm);
                            SetTextColor(hdc_mem, COLORREF(0x00E4DFDA));
                        }
                        let row_x = pad_x + (row.indent as i32) * indent_step;
                        let text_x = row_x + icon_sz + 5;
                        let mut rc = RECT {
                            left: text_x,
                            top: cur_y,
                            right: sheet_w as i32 - pad_x,
                            bottom: cur_y + line_h,
                        };
                        draw_gdi_text_line(
                            hdc_mem,
                            &row.label,
                            &mut rc,
                            DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
                        );
                        cur_y += line_h + 2;
                    }

                    SelectObject(hdc_mem, old_font);
                    let _ = DeleteObject(hfont_bold);
                    let _ = DeleteObject(hfont_norm);
                    let total_bytes = (sheet_w * sheet_h * 4) as usize;
                    let bgra = std::slice::from_raw_parts(bits as *const u8, total_bytes);
                    let mut rgba = RgbaImage::new(sheet_w, sheet_h);
                    for (i, chunk) in bgra.chunks_exact(4).enumerate() {
                        let x = (i % sheet_w as usize) as u32;
                        let y = (i / sheet_w as usize) as u32;
                        rgba.put_pixel(x, y, Rgba([chunk[2], chunk[1], chunk[0], 255]));
                    }
                    SelectObject(hdc_mem, old_bm);
                    let _ = DeleteObject(hbm);
                    let _ = DeleteDC(hdc_mem);
                    Some(rgba)
                } else {
                    let _ = DeleteObject(hbm);
                    let _ = DeleteDC(hdc_mem);
                    None
                }
            } else {
                let _ = DeleteDC(hdc_mem);
                None
            }
        }
    };

    let mut sheet = drawn.unwrap_or_else(|| RgbaImage::from_pixel(sheet_w, sheet_h, sheet_bg));
    let mut cur_y = pad_y;
    for row in rows.iter() {
        if cur_y + line_h > sheet_h as i32 - pad_y {
            break;
        }
        let row_x = pad_x + (row.indent as i32) * indent_step;
        let icon_y = cur_y + (line_h - icon_sz) / 2;
        draw_torrent_row_icon(&mut sheet, row_x, icon_y, icon_sz, row);
        cur_y += line_h + 2;
    }

    apply_program_frame(&sheet, size, frame_bg, accent_line, FoldCorner::TopRight)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FoldCorner {
    None,
    TopRight,
    BottomLeftCurl,
}

/// Frames a preview image/sheet inside a Windows 11 rounded program-colored frame with
/// top/bottom accent stripes, subtle lighting gradients, and optional folded dog-ear corner.
fn apply_program_frame(
    inner: &RgbaImage,
    size: u32,
    frame_bg: Rgba<u8>,
    accent_line: Rgba<u8>,
    fold: FoldCorner,
) -> RgbaImage {
    use crate::badge::{rounded_rect_asym_sdf, smoothstep};

    let border = (size as f32 * 0.052).round().clamp(4.0, 12.0) as u32;
    let (avail_w, avail_h) = program_frame_inner_bounds(size);

    let fitted = if inner.width() <= avail_w
        && inner.height() <= avail_h
        && (inner.height() >= (avail_h * 85) / 100 || inner.width() >= (avail_w * 85) / 100)
    {
        inner.clone()
    } else {
        image::DynamicImage::ImageRgba8(inner.clone())
            .resize(avail_w, avail_h, FilterType::Lanczos3)
            .to_rgba8()
    };

    let out_w = (fitted.width() + border * 2).min(size).max(32);
    let out_h = (fitted.height() + border * 2).min(size).max(32);
    let ox = (out_w.saturating_sub(fitted.width())) / 2;
    let oy = (out_h.saturating_sub(fitted.height())) / 2;

    let wf = out_w as f32;
    let hf = out_h as f32;
    let min_side = wf.min(hf);
    let r_outer = (min_side * 0.068).clamp(7.0, 15.0);
    let r_inner = (r_outer * 0.45).clamp(3.0, 6.5);
    let stripe_h = (hf * 0.014).clamp(2.0, 3.2);

    let fold_size = (min_side * 0.18).round().clamp(14.0, 42.0);
    let r_tip = (fold_size * 0.16).clamp(2.5, 5.5);
    let shadow_spread = (fold_size * 0.28).clamp(4.0, 10.0);

    let inner_l = ox as f32;
    let inner_t = oy as f32;
    let inner_r = (ox + fitted.width()) as f32;
    let inner_b = (oy + fitted.height()) as f32;

    let mut canvas = RgbaImage::from_pixel(out_w, out_h, Rgba([0, 0, 0, 0]));

    for y in 0..out_h {
        let py = y as f32 + 0.5;
        let tv = if out_h > 1 { py / hf } else { 0.0 };

        for x in 0..out_w {
            let px = x as f32 + 0.5;
            let d_outer = rounded_rect_asym_sdf(px, py, 0.0, 0.0, wf, hf, r_outer, r_outer);

            let (d_doc, u, v, d_hyp) = match fold {
                FoldCorner::TopRight => {
                    let u = wf - px;
                    let v = py;
                    let d_hyp = (fold_size - (u + v)) * std::f32::consts::FRAC_1_SQRT_2;
                    (d_outer.max(d_hyp), u, v, d_hyp)
                }
                FoldCorner::BottomLeftCurl => {
                    let u = px;
                    let v = hf - py;
                    // Gentle concave curl along the diagonal cut matching a curled page corner
                    let diag_t = ((u - v) / fold_size.max(1.0)).clamp(-1.0, 1.0);
                    let curl_bow = fold_size * 0.14 * (1.0 - diag_t * diag_t);
                    let d_hyp = ((fold_size - curl_bow) - (u + v)) * std::f32::consts::FRAC_1_SQRT_2;
                    (d_outer.max(d_hyp), u, v, d_hyp)
                }
                FoldCorner::None => (d_outer, 0.0, 0.0, -1.0),
            };

            let doc_cov = (0.5 - d_doc).clamp(0.0, 1.0);
            if doc_cov <= 0.002 {
                continue;
            }

            // Subtle Win11 top-to-bottom lighting gradient on frame + top/bottom accent stripes
            let frame_grad = 10.0 * (1.0 - tv) - 10.0 * tv;
            let top_stripe = 1.0 - smoothstep(stripe_h - 0.6, stripe_h + 0.4, py);
            let bot_stripe = 1.0 - smoothstep(stripe_h - 0.6, stripe_h + 0.4, hf - py);
            let stripe_mix = top_stripe.max(bot_stripe);

            let mut frame_rgb = [
                ((frame_bg[0] as f32 + frame_grad) * (1.0 - stripe_mix) + accent_line[0] as f32 * stripe_mix)
                    .clamp(0.0, 255.0),
                ((frame_bg[1] as f32 + frame_grad) * (1.0 - stripe_mix) + accent_line[1] as f32 * stripe_mix)
                    .clamp(0.0, 255.0),
                ((frame_bg[2] as f32 + frame_grad) * (1.0 - stripe_mix) + accent_line[2] as f32 * stripe_mix)
                    .clamp(0.0, 255.0),
            ];

            // Subtle outer specular rim along rounded edges
            if d_outer > -1.25 && d_outer <= 0.5 {
                let rim_t = smoothstep(-1.25, -0.35, d_outer) * (1.0 - smoothstep(-0.2, 0.45, d_outer));
                let tl_bias = if py < hf * 0.45 { 1.0 } else { 0.4 };
                for c in 0..3 {
                    frame_rgb[c] = (frame_rgb[c] + 24.0 * rim_t * tl_bias).clamp(0.0, 255.0);
                }
            }

            // Composite inner rounded sheet/artwork cleanly inside frame
            let d_inner = rounded_rect_asym_sdf(px, py, inner_l, inner_t, inner_r, inner_b, r_inner, r_inner);
            let inner_cov = (0.5 - d_inner).clamp(0.0, 1.0);
            let mut rgb = if inner_cov > 0.002 && x >= ox && y >= oy {
                let ix = (x - ox).min(fitted.width().saturating_sub(1));
                let iy = (y - oy).min(fitted.height().saturating_sub(1));
                let ip = fitted.get_pixel(ix, iy).0;
                [
                    ip[0] as f32 * inner_cov + frame_rgb[0] * (1.0 - inner_cov),
                    ip[1] as f32 * inner_cov + frame_rgb[1] * (1.0 - inner_cov),
                    ip[2] as f32 * inner_cov + frame_rgb[2] * (1.0 - inner_cov),
                ]
            } else {
                frame_rgb
            };

            // Folded dog-ear corner triangle + soft cast shadow
            if fold != FoldCorner::None {
                let fold_box_sdf = |uu: f32, vv: f32| -> f32 {
                    let cx = fold_size - r_tip;
                    let cy = fold_size - r_tip;
                    if uu > cx && vv > cy {
                        ((uu - cx).powi(2) + (vv - cy).powi(2)).sqrt() - r_tip
                    } else {
                        (uu - fold_size).max(vv - fold_size)
                    }
                };

                let d_flap_inner = fold_box_sdf(u, v);
                let flap_cov = (0.5 - d_flap_inner).clamp(0.0, 1.0);

                // Soft cast shadow beneath & beside the folded triangle onto underlying sheet/frame
                if d_flap_inner > -0.5 {
                    let (sh_u, sh_v) = match fold {
                        FoldCorner::BottomLeftCurl => ((u - 1.6).max(0.0), (v - 1.6).max(0.0)),
                        _ => ((u - 0.8).max(0.0), (v - 2.0).max(0.0)),
                    };
                    let d_sh = fold_box_sdf(sh_u, sh_v);
                    if d_sh < shadow_spread {
                        let sh_t = 1.0 - smoothstep(0.0, shadow_spread, d_sh.max(0.0));
                        let sh_strength = if fold == FoldCorner::BottomLeftCurl { 0.44 } else { 0.30 };
                        let shade = 1.0 - sh_strength * sh_t * sh_t;
                        for c in 0..3 {
                            rgb[c] *= shade;
                        }
                    }
                }

                if flap_cov > 0.002 {
                    let crease_t = ((u + v - fold_size) / fold_size.max(1.0)).clamp(0.0, 1.0);
                    let mut flap_rgb = if fold == FoldCorner::BottomLeftCurl {
                        // Curled white/pearl page back with subtle diagonal roll shading
                        let roll = (1.0 - crease_t) * 0.10 + crease_t * 0.85;
                        let lum = 248.0 - 38.0 * roll;
                        [lum, lum + 2.0, lum + 5.0]
                    } else {
                        let tip_shade = 1.0 - 0.24 * crease_t;
                        [
                            (214.0 * 0.80 + accent_line[0] as f32 * 0.20) * tip_shade,
                            (218.0 * 0.80 + accent_line[1] as f32 * 0.20) * tip_shade,
                            (224.0 * 0.80 + accent_line[2] as f32 * 0.20) * tip_shade,
                        ]
                    };

                    // Subtle diagonal crease highlight + crisp inner edge definition
                    if d_hyp > -1.4 {
                        let c_hl = 16.0 * (1.0 - smoothstep(-1.4, 0.0, d_hyp));
                        for c in 0..3 {
                            flap_rgb[c] = (flap_rgb[c] + c_hl).clamp(0.0, 255.0);
                        }
                    } else if d_flap_inner > -1.3 {
                        let edge_dark = 0.82 + 0.18 * (1.0 - smoothstep(-1.3, 0.0, d_flap_inner));
                        for c in 0..3 {
                            flap_rgb[c] *= edge_dark;
                        }
                    }

                    for c in 0..3 {
                        rgb[c] = flap_rgb[c] * flap_cov + rgb[c] * (1.0 - flap_cov);
                    }
                }
            }

            canvas.put_pixel(
                x,
                y,
                Rgba([
                    rgb[0].round().clamp(0.0, 255.0) as u8,
                    rgb[1].round().clamp(0.0, 255.0) as u8,
                    rgb[2].round().clamp(0.0, 255.0) as u8,
                    (doc_cov * 255.0).round().clamp(0.0, 255.0) as u8,
                ]),
            );
        }
    }

    canvas
}

// ---------------------------------------------------------------------------
// 5. Bottom-right Associated Windows Default Application Icon Overlay
// ---------------------------------------------------------------------------

/// Fetches the associated Windows default program icon for `dot_ext` (e.g. ".xlsx", ".txt")
/// and composites it cleanly in the bottom-right corner of `canvas` without any backdrop box.
pub fn overlay_associated_app_icon(canvas: &mut RgbaImage, dot_ext: &str) {
    let icon_rgba = match fetch_shell_ext_icon_rgba(dot_ext) {
        Some(img) => img,
        None => return,
    };

    let (cw, ch) = canvas.dimensions();
    if cw < 40 || ch < 40 {
        return;
    }

    let badge_side = ((cw.min(ch) * 28) / 100).clamp(20, 76);
    let scaled_icon = image::DynamicImage::ImageRgba8(icon_rgba)
        .resize(badge_side, badge_side, FilterType::Lanczos3)
        .to_rgba8();
    let (iw, ih) = scaled_icon.dimensions();
    if iw == 0 || ih == 0 {
        return;
    }

    let margin = (cw.min(ch) / 26).max(4);
    let ix0 = cw.saturating_sub(iw + margin) as i32;
    let iy0 = ch.saturating_sub(ih + margin) as i32;

    // Subtle per-pixel drop shadow directly behind opaque icon pixels (preserving canvas alpha)
    let shadow_offsets: [(i32, i32, f32); 5] = [
        (1, 2, 0.28),
        (0, 2, 0.20),
        (2, 2, 0.18),
        (1, 1, 0.22),
        (1, 3, 0.12),
    ];
    for &(sx, sy, weight) in &shadow_offsets {
        for iy in 0..ih {
            for ix in 0..iw {
                let src = *scaled_icon.get_pixel(ix, iy);
                if src[3] <= 12 {
                    continue;
                }
                let tx = ix0 + ix as i32 + sx;
                let ty = iy0 + iy as i32 + sy;
                if tx < 0 || ty < 0 || tx >= cw as i32 || ty >= ch as i32 {
                    continue;
                }
                let sa = (src[3] as f32 / 255.0) * weight;
                let dst = *canvas.get_pixel(tx as u32, ty as u32);
                let da = dst[3] as f32 / 255.0;
                let out_a = sa + da * (1.0 - sa);
                if out_a <= 0.002 {
                    continue;
                }
                let factor = (da * (1.0 - sa)) / out_a;
                canvas.put_pixel(
                    tx as u32,
                    ty as u32,
                    Rgba([
                        (dst[0] as f32 * factor).round().clamp(0.0, 255.0) as u8,
                        (dst[1] as f32 * factor).round().clamp(0.0, 255.0) as u8,
                        (dst[2] as f32 * factor).round().clamp(0.0, 255.0) as u8,
                        (out_a * 255.0).round().clamp(0.0, 255.0) as u8,
                    ]),
                );
            }
        }
    }

    // Direct high-resolution Porter-Duff alpha composite of the app icon onto canvas
    for iy in 0..ih {
        for ix in 0..iw {
            let tx = ix0 + ix as i32;
            let ty = iy0 + iy as i32;
            if tx < 0 || ty < 0 || tx >= cw as i32 || ty >= ch as i32 {
                continue;
            }
            let src = *scaled_icon.get_pixel(ix, iy);
            let a = (src[3] as f32) / 255.0;
            if a <= 0.004 {
                continue;
            }
            let dst = *canvas.get_pixel(tx as u32, ty as u32);
            let da = dst[3] as f32 / 255.0;
            let out_a = a + da * (1.0 - a);
            if out_a <= 0.002 {
                continue;
            }
            let inv_d = da * (1.0 - a);
            let out_r = ((src[0] as f32 * a + dst[0] as f32 * inv_d) / out_a)
                .round()
                .clamp(0.0, 255.0) as u8;
            let out_g = ((src[1] as f32 * a + dst[1] as f32 * inv_d) / out_a)
                .round()
                .clamp(0.0, 255.0) as u8;
            let out_b = ((src[2] as f32 * a + dst[2] as f32 * inv_d) / out_a)
                .round()
                .clamp(0.0, 255.0) as u8;
            let out_a_u8 = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
            canvas.put_pixel(tx as u32, ty as u32, Rgba([out_r, out_g, out_b, out_a_u8]));
        }
    }
}

fn trim_transparent_padding(img: RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    let mut min_x = w;
    let mut min_y = h;
    let mut max_x = 0u32;
    let mut max_y = 0u32;
    for y in 0..h {
        for x in 0..w {
            if img.get_pixel(x, y)[3] > 8 {
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if min_x > max_x || min_y > max_y {
        return img;
    }
    let cw = max_x - min_x + 1;
    let ch = max_y - min_y + 1;
    if cw < 8 || ch < 8 {
        return img;
    }
    // Keep crisp tight framing with 1px safety border if already nearly full-bleed
    let side = cw.max(ch);
    if side + 4 >= w && side + 4 >= h {
        return img;
    }
    let mut out = RgbaImage::from_pixel(side, side, Rgba([0, 0, 0, 0]));
    let ox = (side - cw) / 2;
    let oy = (side - ch) / 2;
    for y in 0..ch {
        for x in 0..cw {
            out.put_pixel(ox + x, oy + y, *img.get_pixel(min_x + x, min_y + y));
        }
    }
    out
}

unsafe fn hicon_to_rgba(hicon: windows::Win32::UI::WindowsAndMessaging::HICON) -> Option<RgbaImage> {
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};

    let mut ii = ICONINFO::default();
    if GetIconInfo(hicon, &mut ii).is_err() {
        return None;
    }

    let mut res_img = None;
    let hdc = CreateCompatibleDC(None);
    if !hdc.is_invalid() && !ii.hbmColor.is_invalid() {
        let mut bm = BITMAP::default();
        if GetObjectW(
            ii.hbmColor,
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bm as *mut _ as *mut std::ffi::c_void),
        ) > 0
            && bm.bmWidth > 0
            && bm.bmHeight != 0
        {
            let w = bm.bmWidth as u32;
            let h = bm.bmHeight.unsigned_abs();
            let mut bmi = BITMAPINFO::default();
            bmi.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            };
            let mut color_bgra = vec![0u8; (w as usize) * (h as usize) * 4];
            let got = GetDIBits(
                hdc,
                ii.hbmColor,
                0,
                h,
                Some(color_bgra.as_mut_ptr() as *mut std::ffi::c_void),
                &mut bmi,
                DIB_RGB_COLORS,
            );
            if got > 0 {
                let has_alpha = color_bgra.chunks_exact(4).any(|px| px[3] != 0);
                let mut mask_bgra = vec![0u8; (w as usize) * (h as usize) * 4];
                if !has_alpha && !ii.hbmMask.is_invalid() {
                    let _ = GetDIBits(
                        hdc,
                        ii.hbmMask,
                        0,
                        h,
                        Some(mask_bgra.as_mut_ptr() as *mut std::ffi::c_void),
                        &mut bmi,
                        DIB_RGB_COLORS,
                    );
                }
                let mut img = RgbaImage::new(w, h);
                for i in 0..((w as usize) * (h as usize)) {
                    let c = &color_bgra[i * 4..i * 4 + 4];
                    let a = if has_alpha {
                        c[3]
                    } else if !ii.hbmMask.is_invalid() && mask_bgra[i * 4] == 0 {
                        255
                    } else {
                        0
                    };
                    img.put_pixel((i % w as usize) as u32, (i / w as usize) as u32, Rgba([c[2], c[1], c[0], a]));
                }
                let trimmed = trim_transparent_padding(img);
                if trimmed.pixels().any(|p| p[3] > 8) {
                    res_img = Some(trimmed);
                }
            }
        }
    }

    if !hdc.is_invalid() {
        let _ = DeleteDC(hdc);
    }
    if !ii.hbmColor.is_invalid() {
        let _ = DeleteObject(ii.hbmColor);
    }
    if !ii.hbmMask.is_invalid() {
        let _ = DeleteObject(ii.hbmMask);
    }
    res_img
}

fn read_reg_str_expand(
    root: windows::Win32::System::Registry::HKEY,
    subkey: &str,
    value_name: Option<&str>,
) -> Option<String> {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{RegGetValueW, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ};

    let subkey_w: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    let val_w: Option<Vec<u16>> =
        value_name.map(|v| v.encode_utf16().chain(std::iter::once(0)).collect());
    let val_ptr = val_w
        .as_ref()
        .map(|v| windows::core::PCWSTR(v.as_ptr()))
        .unwrap_or(windows::core::PCWSTR::null());

    let mut buf = vec![0u16; 1024];
    let mut cb = (buf.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            root,
            windows::core::PCWSTR(subkey_w.as_ptr()),
            val_ptr,
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut cb),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..len]).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn sh_load_indirect_string(source: &str) -> Option<String> {
    #[link(name = "shlwapi")]
    extern "system" {
        fn SHLoadIndirectString(
            psz_source: windows::core::PCWSTR,
            psz_out_buf: windows::core::PWSTR,
            cch_out_buf: u32,
            ppv_reserved: *mut *mut std::ffi::c_void,
        ) -> i32;
    }

    let src_w: Vec<u16> = source.encode_utf16().chain(std::iter::once(0)).collect();
    let mut out = vec![0u16; 1024];
    let hr = unsafe {
        SHLoadIndirectString(
            windows::core::PCWSTR(src_w.as_ptr()),
            windows::core::PWSTR(out.as_mut_ptr()),
            out.len() as u32,
            std::ptr::null_mut(),
        )
    };
    if hr >= 0 {
        let len = out.iter().position(|&c| c == 0).unwrap_or(out.len());
        let s = String::from_utf16_lossy(&out[..len]).trim().to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

fn strip_mrt_qualifiers(stem: &str) -> &str {
    let lower = stem.to_ascii_lowercase();
    let mut cut = stem.len();
    for marker in [
        ".altform-",
        ".targetsize-",
        ".scale-",
        ".theme-",
        ".contrast-",
        "_contrast-",
    ] {
        if let Some(pos) = lower.find(marker) {
            cut = cut.min(pos);
        }
    }
    &stem[..cut]
}

fn extract_trailing_number(haystack_lower: &str, key: &str) -> u32 {
    if let Some(idx) = haystack_lower.find(key) {
        let rest = &haystack_lower[idx + key.len()..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        return digits.parse::<u32>().unwrap_or(0);
    }
    0
}

fn extract_appx_package_logo_rgba(progid: &str) -> Option<RgbaImage> {
    use std::path::{Path, PathBuf};
    use windows::Win32::System::Registry::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER};

    let app_key_hkcr = format!(r"{progid}\Application");
    let app_key_hkcu = format!(r"Software\Classes\{progid}\Application");
    let app_icon = read_reg_str_expand(HKEY_CLASSES_ROOT, &app_key_hkcr, Some("ApplicationIcon"))
        .or_else(|| read_reg_str_expand(HKEY_CURRENT_USER, &app_key_hkcu, Some("ApplicationIcon")))?;

    if !app_icon.starts_with("@{") && !app_icon.to_ascii_lowercase().contains("ms-resource:") {
        let p = Path::new(&app_icon);
        if p.is_file() {
            if let Ok(img) = image::open(p) {
                let trimmed = trim_transparent_padding(img.to_rgba8());
                if trimmed.width() >= 8 && trimmed.height() >= 8 && trimmed.pixels().any(|px| px[3] > 8) {
                    return Some(trimmed);
                }
            }
        }
    }

    let resolved = sh_load_indirect_string(&app_icon);
    let mut assets_dir: Option<PathBuf> = resolved
        .as_deref()
        .and_then(|s| Path::new(s).parent().map(Path::to_path_buf));
    let mut stem: Option<String> = resolved
        .as_deref()
        .and_then(|s| Path::new(s).file_stem())
        .and_then(|s| s.to_str())
        .map(|s| strip_mrt_qualifiers(s).to_string());

    if (assets_dir.is_none() || stem.is_none()) && app_icon.starts_with("@{") {
        if let Some(q) = app_icon.find('?') {
            let pkg_full = &app_icon[2..q];
            // Package full names are `[A-Za-z0-9._-]` only; anything else (separators, `..`,
            // colons) would let a registry value redirect the icon read outside WindowsApps.
            let pkg_ok = !pkg_full.is_empty()
                && pkg_full.len() <= 256
                && !pkg_full.contains("..")
                && pkg_full
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
            let windows_apps = std::env::var_os("ProgramW6432")
                .or_else(|| std::env::var_os("ProgramFiles"))
                .map(|p| PathBuf::from(p).join("WindowsApps"));
            if let (true, true, Some(windows_apps)) = (pkg_ok, assets_dir.is_none(), windows_apps) {
                let pkg_root = windows_apps.join(pkg_full);
                let candidate_dir = pkg_root.join("Assets");
                if candidate_dir.is_dir() {
                    assets_dir = Some(candidate_dir);
                } else if pkg_root.is_dir() {
                    assets_dir = Some(pkg_root);
                }
            }
        }
        if stem.is_none() {
            let trimmed_res = app_icon.trim_end_matches('}');
            if let Some(last_seg) = trimmed_res.rsplit('/').next() {
                let clean = last_seg
                    .strip_suffix(".png")
                    .or_else(|| last_seg.strip_suffix(".PNG"))
                    .unwrap_or(last_seg);
                if !clean.is_empty() {
                    stem = Some(strip_mrt_qualifiers(clean).to_string());
                }
            }
        }
    }

    let mut ranked: Vec<(u32, u32, PathBuf)> = Vec::new();
    let stem_str = stem.as_deref().unwrap_or("Square44x44Logo");
    let stem_lower = stem_str.to_ascii_lowercase();

    if let Some(ref dir) = assets_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let ext_ok = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("png"))
                    .unwrap_or(false);
                if !ext_ok {
                    continue;
                }
                let Some(fstem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let f_low = fstem.to_ascii_lowercase();
                if f_low.contains("contrast-black")
                    || f_low.contains("contrast-white")
                    || f_low.contains(".contrast-")
                    || f_low.contains("_contrast-")
                    || f_low.contains("theme-light")
                {
                    continue;
                }
                let matches_stem = f_low == stem_lower
                    || f_low.starts_with(&format!("{stem_lower}."))
                    || f_low.starts_with(&format!("{stem_lower}_"));
                let (tier, sub) = if matches_stem {
                    if f_low.contains("altform-unplated") && f_low.contains("targetsize-") {
                        (800, extract_trailing_number(&f_low, "targetsize-").min(512))
                    } else if f_low.contains("altform-unplated") {
                        (750, extract_trailing_number(&f_low, "scale-").min(512))
                    } else if f_low.contains("altform-lightunplated") && f_low.contains("targetsize-") {
                        (650, extract_trailing_number(&f_low, "targetsize-").min(512))
                    } else if f_low.contains("targetsize-") {
                        (500, extract_trailing_number(&f_low, "targetsize-").min(512))
                    } else if f_low.contains("scale-") {
                        (400, extract_trailing_number(&f_low, "scale-").min(512))
                    } else {
                        (350, 0)
                    }
                } else if f_low.starts_with("square44x44logo") && f_low.contains("altform-unplated") {
                    (600, extract_trailing_number(&f_low, "targetsize-").min(512))
                } else if f_low.starts_with("storelogo") || f_low.starts_with("square150x150logo") {
                    (200, extract_trailing_number(&f_low, "scale-").min(512))
                } else {
                    continue;
                };
                ranked.push((tier, sub, path));
            }
        }

        // Explicit fallback probes in case directory listing is restricted on WindowsApps
        let explicit = [
            (799, 256, format!("{stem_str}.altform-unplated_targetsize-256.png")),
            (798, 96, format!("{stem_str}.altform-unplated_targetsize-96.png")),
            (797, 64, format!("{stem_str}.altform-unplated_targetsize-64.png")),
            (796, 48, format!("{stem_str}.altform-unplated_targetsize-48.png")),
            (740, 0, format!("{stem_str}.altform-unplated.png")),
            (649, 256, format!("{stem_str}.altform-lightunplated_targetsize-256.png")),
            (499, 256, format!("{stem_str}.targetsize-256.png")),
            (399, 400, format!("{stem_str}.scale-400.png")),
            (398, 200, format!("{stem_str}.scale-200.png")),
        ];
        for (tier, sub, rel) in explicit {
            ranked.push((tier, sub, dir.join(rel)));
        }
    }

    if let Some(ref res_path) = resolved {
        ranked.push((150, 0, PathBuf::from(res_path)));
    }

    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));

    let mut tried = std::collections::HashSet::new();
    for (_tier, _sub, path) in ranked {
        let key = path.to_string_lossy().to_ascii_lowercase();
        if !tried.insert(key) || !path.is_file() {
            continue;
        }
        if let Ok(img) = image::open(&path) {
            let trimmed = trim_transparent_padding(img.to_rgba8());
            if trimmed.width() >= 8 && trimmed.height() >= 8 && trimmed.pixels().any(|px| px[3] > 8) {
                return Some(trimmed);
            }
        }
    }
    None
}

fn parse_exe_from_command(cmd: &str) -> Option<String> {
    let s = cmd.trim();
    if s.is_empty() {
        return None;
    }
    let exe_candidate = if let Some(after_quote) = s.strip_prefix('"') {
        after_quote.split('"').next().unwrap_or("").trim().to_string()
    } else {
        let lower = s.to_ascii_lowercase();
        if let Some(idx) = lower.find(".exe") {
            s[..idx + 4].trim().to_string()
        } else {
            s.split_whitespace().next().unwrap_or("").trim().to_string()
        }
    };
    if exe_candidate.is_empty() {
        return None;
    }
    let base = std::path::Path::new(&exe_candidate)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(
        base.as_str(),
        "applicationframehost.exe" | "rundll32.exe" | "cmd.exe" | "openwith.exe" | "launcher.exe"
    ) {
        return None;
    }
    if std::path::Path::new(&exe_candidate).is_file() {
        Some(exe_candidate)
    } else {
        None
    }
}

fn resolve_exe_from_progid(progid: &str) -> Option<String> {
    use windows::Win32::System::Registry::{HKEY_CLASSES_ROOT, HKEY_CURRENT_USER};

    let cmd_key_hkcr = format!(r"{progid}\Shell\open\command");
    let cmd_key_hkcu = format!(r"Software\Classes\{progid}\Shell\open\command");
    if let Some(cmd) = read_reg_str_expand(HKEY_CLASSES_ROOT, &cmd_key_hkcr, None)
        .or_else(|| read_reg_str_expand(HKEY_CURRENT_USER, &cmd_key_hkcu, None))
    {
        if let Some(exe) = parse_exe_from_command(&cmd) {
            return Some(exe);
        }
    }
    None
}

fn resolve_associated_exe(query: &str) -> Option<String> {
    #[link(name = "shlwapi")]
    extern "system" {
        fn AssocQueryStringW(
            flags: u32,
            str_type: u32,
            psz_assoc: windows::core::PCWSTR,
            psz_extra: windows::core::PCWSTR,
            psz_out: windows::core::PWSTR,
            pcch_out: *mut u32,
        ) -> i32;
    }

    const ASSOCF_NOTRUNCATE: u32 = 0x0000_0020;
    const ASSOCSTR_EXECUTABLE: u32 = 2;

    let ext_w: Vec<u16> = query.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    let hr = unsafe {
        AssocQueryStringW(
            ASSOCF_NOTRUNCATE,
            ASSOCSTR_EXECUTABLE,
            windows::core::PCWSTR(ext_w.as_ptr()),
            windows::core::PCWSTR::null(),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
    };
    if hr >= 0 && len > 1 {
        let s = String::from_utf16_lossy(&buf[..(len as usize - 1)])
            .trim_matches(|c: char| c == '\0' || c.is_whitespace())
            .to_string();
        if !s.is_empty() {
            let base = std::path::Path::new(&s)
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.to_ascii_lowercase())
                .unwrap_or_default();
            if base != "applicationframehost.exe" && base != "openwith.exe" {
                return Some(s);
            }
        }
    }
    None
}

unsafe fn extract_shell_imagelist_icon(target_w: &[u16], use_file_attrs: bool) -> Option<RgbaImage> {
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_SYSICONINDEX, SHGFI_USEFILEATTRIBUTES};
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};

    #[link(name = "shell32")]
    extern "system" {
        fn SHGetImageList(
            i_image_list: i32,
            riid: *const windows::core::GUID,
            ppv_obj: *mut *mut std::ffi::c_void,
        ) -> i32;
    }

    #[repr(C)]
    struct IImageListVtbl {
        query_interface: usize,
        add_ref: usize,
        release: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
        add: usize,
        replace_icon: usize,
        set_overlay_image: usize,
        replace: usize,
        add_masked: usize,
        draw: usize,
        remove: usize,
        get_icon: unsafe extern "system" fn(*mut std::ffi::c_void, i32, u32, *mut HICON) -> i32,
    }

    const SHIL_EXTRALARGE: i32 = 2; // 48x48
    const SHIL_JUMBO: i32 = 4; // 256x256
    const ILD_TRANSPARENT: u32 = 0x0000_0001;
    const IID_IIMAGE_LIST: windows::core::GUID = windows::core::GUID::from_values(
        0x46eb5926,
        0x582e,
        0x4017,
        [0x9f, 0xdf, 0xe8, 0x99, 0x8d, 0xaa, 0x09, 0x50],
    );

    let mut sfi = SHFILEINFOW::default();
    let flags = SHGFI_SYSICONINDEX | if use_file_attrs { SHGFI_USEFILEATTRIBUTES } else { windows::Win32::UI::Shell::SHGFI_FLAGS(0) };
    let ok = SHGetFileInfoW(
        windows::core::PCWSTR(target_w.as_ptr()),
        if use_file_attrs { FILE_ATTRIBUTE_NORMAL } else { windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES(0) },
        Some(&mut sfi),
        std::mem::size_of::<SHFILEINFOW>() as u32,
        flags,
    );
    if ok == 0 || sfi.iIcon < 0 {
        return None;
    }

    for &list_id in &[SHIL_JUMBO, SHIL_EXTRALARGE] {
        let mut obj: *mut std::ffi::c_void = std::ptr::null_mut();
        let hr = SHGetImageList(list_id, &IID_IIMAGE_LIST, &mut obj);
        if hr >= 0 && !obj.is_null() {
            let vtbl = &*(*(obj as *mut *const IImageListVtbl));
            let mut hicon = HICON::default();
            let get_hr = (vtbl.get_icon)(obj, sfi.iIcon, ILD_TRANSPARENT, &mut hicon);
            (vtbl.release)(obj);
            if get_hr >= 0 && !hicon.is_invalid() {
                let img = hicon_to_rgba(hicon);
                let _ = DestroyIcon(hicon);
                if let Some(rgba) = img {
                    return Some(rgba);
                }
            }
        }
    }
    None
}

unsafe fn extract_exe_icon_direct(exe_w: &[u16]) -> Option<RgbaImage> {
    use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};

    #[link(name = "user32")]
    extern "system" {
        fn PrivateExtractIconsW(
            sz_file_name: windows::core::PCWSTR,
            n_icon_index: i32,
            cx_icon: i32,
            cy_icon: i32,
            phicon: *mut HICON,
            piconid: *mut u32,
            n_icons: u32,
            flags: u32,
        ) -> u32;
    }

    for side in [256i32, 128, 64, 48] {
        let mut hicon = HICON::default();
        let mut icon_id = 0u32;
        let count = PrivateExtractIconsW(
            windows::core::PCWSTR(exe_w.as_ptr()),
            0,
            side,
            side,
            &mut hicon,
            &mut icon_id,
            1,
            0,
        );
        if count > 0 && !hicon.is_invalid() {
            let img = hicon_to_rgba(hicon);
            let _ = DestroyIcon(hicon);
            if let Some(rgba) = img {
                return Some(rgba);
            }
        }
    }
    None
}

fn extract_icon_for_exe_path(exe_path: &str) -> Option<RgbaImage> {
    let exe_w: Vec<u16> = exe_path.encode_utf16().chain(std::iter::once(0)).collect();
    let direct = unsafe { extract_exe_icon_direct(&exe_w) };
    let shell_exe = unsafe { extract_shell_imagelist_icon(&exe_w, false) };
    match (direct, shell_exe) {
        (Some(a), Some(b)) => {
            if a.width() >= b.width() {
                Some(a)
            } else {
                Some(b)
            }
        }
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn fetch_shell_ext_icon_rgba(dot_ext: &str) -> Option<RgbaImage> {
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
    use windows::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES};
    use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(pv_reserved: *mut std::ffi::c_void, dw_co_init: u32) -> i32;
        fn CoUninitialize();
    }
    const COINIT_APARTMENTTHREADED: u32 = 0x2;

    unsafe {
        let co_hr = CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED);
        let did_com_init = co_hr >= 0;

        let mut best: Option<RgbaImage> = None;

        // 1. Walk ProgIDs in Windows priority order (UserChoiceLatest -> UserChoice -> HKCU -> HKCR -> OpenWithProgids).
        //    Prefer AppX unplated package logo PNG or standalone .exe icon (no white paper file icon).
        let progids = crate::registry::get_progids_for_extension(dot_ext);
        for progid in &progids {
            if let Some(appx_logo) = extract_appx_package_logo_rgba(progid) {
                best = Some(appx_logo);
                break;
            }
            if let Some(exe_path) = resolve_exe_from_progid(progid) {
                if let Some(img) = extract_icon_for_exe_path(&exe_path) {
                    best = Some(img);
                    break;
                }
            }
            if let Some(exe_path) = resolve_associated_exe(progid) {
                if let Some(img) = extract_icon_for_exe_path(&exe_path) {
                    best = Some(img);
                    break;
                }
            }
        }

        // 2. Try AssocQueryStringW directly on the extension
        if best.is_none() {
            if let Some(exe_path) = resolve_associated_exe(dot_ext) {
                best = extract_icon_for_exe_path(&exe_path);
            }
        }

        // 3. Fallback to extension jumbo/extralarge shell icon if no app icon was obtained
        let ext_w: Vec<u16> = dot_ext.encode_utf16().chain(std::iter::once(0)).collect();
        if best.is_none() {
            best = extract_shell_imagelist_icon(&ext_w, true);
        }

        // 4. Last-resort standard shell icon lookup
        if best.is_none() {
            let mut sfi = SHFILEINFOW::default();
            let ok = SHGetFileInfoW(
                windows::core::PCWSTR(ext_w.as_ptr()),
                FILE_ATTRIBUTE_NORMAL,
                Some(&mut sfi),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES,
            );
            if ok != 0 && !sfi.hIcon.is_invalid() {
                best = hicon_to_rgba(sfi.hIcon);
                let _ = DestroyIcon(sfi.hIcon);
            }
        }

        if did_com_init {
            CoUninitialize();
        }
        best
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn colorized_text_extensions_in_checklist_default_off_and_rendered_with_colors() {
        let colorized_checklist_exts = [
            ".json", ".csv", ".html", ".htm", ".xml", ".yaml", ".yml", ".toml", ".ini", ".log",
            ".ps1", ".cmd", ".bat", ".sh", ".sql", ".py", ".js", ".tsx", ".css", ".rs",
        ];
        for ext in colorized_checklist_exts {
            let info = crate::media::SUPPORTED_MEDIA_FORMATS
                .iter()
                .find(|f| f.ext.eq_ignore_ascii_case(ext))
                .unwrap_or_else(|| panic!("expected {ext} in SUPPORTED_MEDIA_FORMATS"));
            assert_eq!(info.category, crate::media::MediaCategory::Syntax);
            assert!(!info.default_on, "{ext} should default to unchecked (default_on: false)");
            assert!(
                crate::media::is_other_or_custom_text_extension(ext, &[]),
                "expected Other pipeline routing for {ext}"
            );
        }

        for ext in [".txt", ".md"] {
            let info = crate::media::SUPPORTED_MEDIA_FORMATS
                .iter()
                .find(|f| f.ext.eq_ignore_ascii_case(ext))
                .unwrap_or_else(|| panic!("expected {ext} in SUPPORTED_MEDIA_FORMATS"));
            assert_eq!(info.category, crate::media::MediaCategory::Other);
            assert!(info.default_on, "{ext} should remain default_on: true");
        }

        for unlisted in [".jsonc", ".jsonl", ".tsv", ".psm1", ".c", ".go"] {
            assert!(crate::media::is_builtin_plain_text_extension(unlisted));
            assert!(crate::media::SUPPORTED_MEDIA_FORMATS
                .iter()
                .all(|f| !f.ext.eq_ignore_ascii_case(unlisted)));
        }

        let json_sample = br#"{
  "name": "CBXShell-rs",
  "version": 2026,
  "enabled": true,
  "tags": ["thumb", "syntax"]
}"#;
        let ps1_sample = br#"# PowerShell sample
param([string]$Target = ".\samples")
foreach ($item in Get-ChildItem $Target) {
    Write-Host $item.Name
}"#;

        for (ext, payload) in [(".json", json_sample.as_slice()), (".ps1", ps1_sample.as_slice())] {
            let mut cur = Cursor::new(payload);
            let img = create_other_thumbnail_rgba(&mut cur, ext, 256)
                .unwrap_or_else(|e| panic!("failed rendering {ext}: {e}"));
            let mut chroma_count = 0usize;
            for y in 28..(img.height() * 2 / 3) {
                for x in 36..(img.width() * 2 / 3) {
                    let [r, g, b, _] = img.get_pixel(x, y).0;
                    if r.abs_diff(g) > 25 || g.abs_diff(b) > 25 || r.abs_diff(b) > 25 {
                        chroma_count += 1;
                    }
                }
            }
            assert!(
                chroma_count >= 15,
                "expected syntax-colored pixels inside {ext} sheet card, got {chroma_count}"
            );
        }
    }

    #[test]
    fn plain_text_stream_heuristic_distinguishes_text_and_binary() {
        let html = b"<!DOCTYPE html><html><head><title>Help</title></head><body><h1>Docs</h1></body></html>";
        let mut cur_html = Cursor::new(html.as_slice());
        assert!(is_likely_plain_text_stream(&mut cur_html));

        let utf16_le_bom = b"\xFF\xFEH\x00e\x00l\x00l\x00o\x00";
        let mut cur_u16 = Cursor::new(utf16_le_bom.as_slice());
        assert!(is_likely_plain_text_stream(&mut cur_u16));

        let zip_magic = b"PK\x03\x04corrupted zip header without eocd";
        let mut cur_zip = Cursor::new(zip_magic.as_slice());
        assert!(!is_likely_plain_text_stream(&mut cur_zip));

        let nul_binary = b"hello\x00world\x00\x01\x02\x03";
        let mut cur_bin = Cursor::new(nul_binary.as_slice());
        assert!(!is_likely_plain_text_stream(&mut cur_bin));
    }

    #[test]
    fn unified_reader_falls_back_to_plain_text_when_archive_fails() {
        let content = b"{\"status\": \"ok\", \"title\": \"CBXShell Test\", \"items\": [1, 2, 3]}";
        let mut cur = Cursor::new(content.as_slice());
        let settings = crate::settings::Settings::default();
        let res = crate::create_thumbnail_from_reader_unified(
            &mut cur,
            192,
            &settings,
            Some("unknown_payload.unknown_txt"),
        );
        let (hbmp, summary) = res.expect("expected plain-text fallback thumbnail");
        assert!(
            summary.contains("純文字解析(UNKNOWN_TXT)") || summary.contains("Plain-text (UNKNOWN_TXT)"),
            "unexpected summary: {summary}"
        );
        assert!(!hbmp.is_invalid());
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::DeleteObject(hbmp);
        }
    }

    #[test]
    fn torrent_bencode_parses_and_renders_with_curled_bottom_left() {
        let torrent = b"d8:announce23:https://example.com/ann4:infod5:filesld6:lengthi1024e4:pathl14:SampleClip.mp4eed6:lengthi512e4:pathl8:SubTitle10:readme.txteee4:name11:SamplePack112:piece lengthi16384e6:pieces20:01234567890123456789ee";
        let rows = parse_torrent_rows(torrent);
        assert!(!rows.is_empty());
        assert!(rows[0].is_dir);
        assert_eq!(rows[0].label, "SamplePack1");
        assert!(rows.iter().any(|r| !r.is_dir && r.label == "SampleClip.mp4"));

        let mut cur = Cursor::new(torrent.as_slice());
        let img = create_other_thumbnail_rgba(&mut cur, ".torrent", 256)
            .expect("expected torrent thumbnail");
        assert!(img.width() >= 128 && img.height() >= 128);
        // Outer top-right corner is folded away (transparent), while inner flap is bright opaque
        let outer = img.get_pixel(img.width() - 4, 4).0;
        assert_eq!(outer[3], 0, "expected folded-away top-right outer corner, got {:?}", outer);
        let flap_off = (img.width() as f32 * 0.11).round() as u32;
        let flap_x = img.width() - flap_off;
        let flap_y = flap_off;
        let p = img.get_pixel(flap_x, flap_y).0;
        assert!(p[3] > 200 && p[0] > 140 && p[1] > 140 && p[2] > 140, "expected folded top-right flap at ({flap_x},{flap_y}), got {:?}", p);
    }
}
