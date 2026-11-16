use std::io::{Read, Seek, SeekFrom};

use image::{imageops::FilterType, Rgba, RgbaImage};
use quick_xml::events::Event;
use quick_xml::Reader as XmlReader;
use zip::ZipArchive;

use crate::archive::CoverOptions;

const MAX_PDF_MEMORY_BYTES: usize = 128 * 1024 * 1024;
const MAX_MOBI_SCAN_BYTES: usize = 64 * 1024 * 1024;

/// Trait alias for readable & seekable e-book streams.
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// Generates a cover `RgbaImage` for an e-book stream (`.epub`, `.iba`, `.mobi`, `.azw`, `.pdf`).
pub fn extract_ebook_cover_image(
    stream: &mut dyn ReadSeek,
    ext: &str,
    target_size: u32,
    opt: &CoverOptions,
) -> Result<RgbaImage, String> {
    let clean_ext = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    let max_dim = target_size.clamp(64, 1024);

    match clean_ext.as_str() {
        "epub" | "iba" => extract_epub_or_iba_cover(stream, max_dim, opt),
        "mobi" | "azw" | "azw3" => extract_mobi_or_azw_cover(stream, max_dim, opt),
        "pdf" => extract_pdf_cover(stream, max_dim),
        _ => Err(format!("Unsupported ebook format: {}", ext)),
    }
}

/// Decodes raw image bytes into a resized `RgbaImage` fitting inside `max_dim`.
fn decode_and_resize_rgba(bytes: &[u8], max_dim: u32) -> Result<RgbaImage, String> {
    // Strict limits: ebook covers are attacker-controlled images (dimension/allocation bombs).
    let img = crate::image_dec::decode_bytes_strict(bytes)?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err("Decoded ebook cover has zero dimensions".to_string());
    }
    let resized = if w > max_dim || h > max_dim {
        img.resize(max_dim, max_dim, FilterType::Triangle)
    } else {
        img
    };
    Ok(resized.to_rgba8())
}

fn is_supported_ebook_img_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".png")
        || lower.ends_with(".webp")
        || lower.ends_with(".gif")
        || lower.ends_with(".bmp")
}

fn resolve_zip_relative_path(base_dir: &str, href: &str) -> String {
    let decoded_href = percent_decode_basic(href);
    let clean_href = decoded_href.split('#').next().unwrap_or(&decoded_href);
    let normalized = clean_href.replace('\\', "/");
    let combined = if base_dir.is_empty() || normalized.starts_with('/') {
        normalized.trim_start_matches('/').to_string()
    } else {
        format!("{}/{}", base_dir.trim_end_matches('/'), normalized)
    };

    let mut stack: Vec<&str> = Vec::new();
    for seg in combined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            s => stack.push(s),
        }
    }
    stack.join("/")
}

fn percent_decode_basic(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                out.push(((h << 4) | l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn read_zip_entry_bounded(
    archive: &mut ZipArchive<&mut dyn ReadSeek>,
    idx: usize,
    max_bytes: u64,
) -> Result<Vec<u8>, String> {
    let mut entry = archive
        .by_index(idx)
        .map_err(|e| format!("ZIP entry open failed: {}", e))?;
    if entry.size() > max_bytes {
        return Err("ZIP entry exceeds size limit".to_string());
    }
    let cap = (entry.size() as usize).min(16 * 1024 * 1024);
    let mut buf = Vec::with_capacity(cap);
    let mut chunk = (&mut entry).take(max_bytes + 1);
    chunk
        .read_to_end(&mut buf)
        .map_err(|e| format!("ZIP entry read failed: {}", e))?;
    if buf.len() as u64 > max_bytes {
        return Err("ZIP entry decompressed payload exceeded limit".to_string());
    }
    Ok(buf)
}

fn find_zip_entry_case_insensitive(names: &[(usize, String)], target: &str) -> Option<usize> {
    let target_norm = target.replace('\\', "/").trim_start_matches('/').to_ascii_lowercase();
    names
        .iter()
        .find(|(_, n)| {
            n.replace('\\', "/")
                .trim_start_matches('/')
                .to_ascii_lowercase()
                == target_norm
        })
        .map(|(i, _)| *i)
}

fn extract_opf_path_from_container(xml_bytes: &[u8]) -> Option<String> {
    let mut reader = XmlReader::from_reader(xml_bytes);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) => {
                if e.local_name().as_ref().eq_ignore_ascii_case(b"rootfile") {
                    for attr in e.attributes().flatten() {
                        if attr.key.local_name().as_ref().eq_ignore_ascii_case(b"full-path") {
                            if let Ok(val) = attr.unescape_value() {
                                return Some(val.into_owned());
                            }
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    None
}

fn extract_cover_href_from_opf(xml_bytes: &[u8]) -> Option<String> {
    let mut reader = XmlReader::from_reader(xml_bytes);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut meta_cover_id: Option<String> = None;
    let mut items: Vec<(String, String, String, String)> = Vec::new(); // (id, href, properties, media_type)
    let mut guide_cover_href: Option<String> = None;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) => {
                let lname = e.local_name();
                if lname.as_ref().eq_ignore_ascii_case(b"meta") {
                    let mut name_attr = String::new();
                    let mut content_attr = String::new();
                    for attr in e.attributes().flatten() {
                        if attr.key.local_name().as_ref().eq_ignore_ascii_case(b"name") {
                            name_attr = attr.unescape_value().map(|s| s.into_owned()).unwrap_or_default();
                        } else if attr.key.local_name().as_ref().eq_ignore_ascii_case(b"content") {
                            content_attr = attr.unescape_value().map(|s| s.into_owned()).unwrap_or_default();
                        }
                    }
                    if name_attr.eq_ignore_ascii_case("cover") && !content_attr.is_empty() {
                        meta_cover_id = Some(content_attr);
                    }
                } else if lname.as_ref().eq_ignore_ascii_case(b"item") {
                    let mut id = String::new();
                    let mut href = String::new();
                    let mut props = String::new();
                    let mut mtype = String::new();
                    for attr in e.attributes().flatten() {
                        let k = attr.key.local_name();
                        let val = attr.unescape_value().map(|s| s.into_owned()).unwrap_or_default();
                        if k.as_ref().eq_ignore_ascii_case(b"id") {
                            id = val;
                        } else if k.as_ref().eq_ignore_ascii_case(b"href") {
                            href = val;
                        } else if k.as_ref().eq_ignore_ascii_case(b"properties") {
                            props = val;
                        } else if k.as_ref().eq_ignore_ascii_case(b"media-type") {
                            mtype = val;
                        }
                    }
                    if !href.is_empty() {
                        items.push((id, href, props, mtype));
                    }
                } else if lname.as_ref().eq_ignore_ascii_case(b"reference") {
                    let mut ref_type = String::new();
                    let mut href = String::new();
                    for attr in e.attributes().flatten() {
                        let k = attr.key.local_name();
                        let val = attr.unescape_value().map(|s| s.into_owned()).unwrap_or_default();
                        if k.as_ref().eq_ignore_ascii_case(b"type") {
                            ref_type = val;
                        } else if k.as_ref().eq_ignore_ascii_case(b"href") {
                            href = val;
                        }
                    }
                    if ref_type.eq_ignore_ascii_case("cover") && !href.is_empty() {
                        guide_cover_href = Some(href);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    // 1. EPUB 3 properties="cover-image"
    for (_, href, props, _) in &items {
        if props.split_whitespace().any(|p| p.eq_ignore_ascii_case("cover-image")) {
            return Some(href.clone());
        }
    }

    // 2. EPUB 2 <meta name="cover" content="..."/> matched against manifest item id
    if let Some(ref cover_id) = meta_cover_id {
        if let Some((_, href, _, mtype)) = items.iter().find(|(id, _, _, _)| id.eq_ignore_ascii_case(cover_id)) {
            if mtype.starts_with("image/") || is_supported_ebook_img_name(href) {
                return Some(href.clone());
            }
        }
        // Fallback: some malformed OPFs put a direct href inside content=""
        if is_supported_ebook_img_name(cover_id) {
            return Some(cover_id.clone());
        }
    }

    // 3. Manifest item with id or href strongly indicating cover image
    for (id, href, _, mtype) in &items {
        if (mtype.starts_with("image/") || is_supported_ebook_img_name(href))
            && (id.to_ascii_lowercase().contains("cover") || href.to_ascii_lowercase().contains("cover"))
        {
            return Some(href.clone());
        }
    }

    // 4. Guide cover HTML reference (if it points directly to an image)
    if let Some(href) = guide_cover_href {
        if is_supported_ebook_img_name(&href) {
            return Some(href);
        }
    }

    None
}

fn extract_epub_or_iba_cover(
    stream: &mut dyn ReadSeek,
    max_dim: u32,
    opt: &CoverOptions,
) -> Result<RgbaImage, String> {
    let _ = stream.seek(SeekFrom::Start(0));
    crate::archive::verify_zip_eocd_tail(stream)?;
    let mut archive = ZipArchive::new(stream).map_err(|e| format!("EPUB/IBA open failed: {}", e))?;
    let total = archive.len().min(65536);

    let mut entries: Vec<(usize, String)> = Vec::with_capacity(total);
    let mut img_entries: Vec<(usize, String)> = Vec::new();
    for i in 0..total {
        if let Ok(file) = archive.by_index_raw(i) {
            if file.is_dir() {
                continue;
            }
            let name = file.name().to_string();
            if is_supported_ebook_img_name(&name) {
                img_entries.push((i, name.clone()));
            }
            entries.push((i, name));
        }
    }

    // 1. iBooks / QuickLook preview thumbnails (very common in .iba & Apple EPUB)
    let quicklook_candidates = [
        "QuickLook/Thumbnail.png",
        "QuickLook/Thumbnail.jpg",
        "QuickLook/Thumbnail.jpeg",
        "QuickLook/Preview.png",
        "Thumbnails/thumbnail.png",
        "iBooksArtwork",
    ];
    for cand in quicklook_candidates {
        if let Some(idx) = find_zip_entry_case_insensitive(&entries, cand) {
            if let Ok(bytes) = read_zip_entry_bounded(&mut archive, idx, opt.max_image_bytes) {
                if let Ok(img) = decode_and_resize_rgba(&bytes, max_dim) {
                    return Ok(img);
                }
            }
        }
    }

    // 2. Standard EPUB META-INF/container.xml -> .opf -> cover image
    let mut opf_path: Option<String> = None;
    if let Some(c_idx) = find_zip_entry_case_insensitive(&entries, "META-INF/container.xml") {
        if let Ok(xml_bytes) = read_zip_entry_bounded(&mut archive, c_idx, 2 * 1024 * 1024) {
            opf_path = extract_opf_path_from_container(&xml_bytes);
        }
    }
    if opf_path.is_none() {
        opf_path = entries
            .iter()
            .find(|(_, n)| n.to_ascii_lowercase().ends_with(".opf"))
            .map(|(_, n)| n.clone());
    }

    if let Some(opf) = opf_path {
        if let Some(opf_idx) = find_zip_entry_case_insensitive(&entries, &opf) {
            if let Ok(opf_bytes) = read_zip_entry_bounded(&mut archive, opf_idx, 8 * 1024 * 1024) {
                if let Some(href) = extract_cover_href_from_opf(&opf_bytes) {
                    let opf_norm = opf.replace('\\', "/");
                    let opf_dir = match opf_norm.rfind('/') {
                        Some(p) => &opf_norm[..p],
                        None => "",
                    };
                    let target = resolve_zip_relative_path(opf_dir, &href);
                    if let Some(img_idx) = find_zip_entry_case_insensitive(&entries, &target) {
                        if let Ok(img_bytes) =
                            read_zip_entry_bounded(&mut archive, img_idx, opt.max_image_bytes)
                        {
                            if let Ok(img) = decode_and_resize_rgba(&img_bytes, max_dim) {
                                return Ok(img);
                            }
                        }
                    }
                }
            }
        }
    }

    // 3. Fallback: image named *cover*/*front*/*thumb*, then natural order
    let mut named_covers: Vec<(usize, String)> = img_entries
        .iter()
        .filter(|(_, n)| {
            let l = n.to_ascii_lowercase();
            l.contains("cover") || l.contains("front") || l.contains("thumbnail")
        })
        .cloned()
        .collect();
    if !named_covers.is_empty() {
        named_covers.sort_by(|a, b| crate::archive::natural_cmp(&a.1, &b.1));
        for (idx, _) in named_covers {
            if let Ok(bytes) = read_zip_entry_bounded(&mut archive, idx, opt.max_image_bytes) {
                if let Ok(img) = decode_and_resize_rgba(&bytes, max_dim) {
                    return Ok(img);
                }
            }
        }
    }

    if let Some(idx) = crate::archive::pick_best_cover_index(
        &img_entries,
        opt.sort,
        opt.prefer_continuous_sequence,
    ) {
        let bytes = read_zip_entry_bounded(&mut archive, idx, opt.max_image_bytes)?;
        return decode_and_resize_rgba(&bytes, max_dim);
    }

    Err("No cover image found inside EPUB/IBA".to_string())
}

fn be_u16(b: &[u8], off: usize) -> Option<u16> {
    b.get(off..off + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}

fn be_u32(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn looks_like_image_bytes(b: &[u8]) -> bool {
    if b.len() < 16 {
        return false;
    }
    // JPEG
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return true;
    }
    // PNG
    if b.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        return true;
    }
    // GIF
    if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        return true;
    }
    // WebP
    if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        return true;
    }
    false
}

fn extract_mobi_or_azw_cover(
    stream: &mut dyn ReadSeek,
    max_dim: u32,
    opt: &CoverOptions,
) -> Result<RgbaImage, String> {
    let _ = stream.seek(SeekFrom::Start(0));
    let mut head = [0u8; 4];
    if stream.read_exact(&mut head).is_ok() && &head == b"PK\x03\x04" {
        return extract_epub_or_iba_cover(stream, max_dim, opt);
    }

    let file_len = stream
        .seek(SeekFrom::End(0))
        .map_err(|e| format!("MOBI/AZW seek failed: {}", e))?;
    if file_len < 96 {
        return Err("File too small to be PalmDB MOBI/AZW".to_string());
    }

    let _ = stream.seek(SeekFrom::Start(0));
    let mut pdb_hdr = [0u8; 78];
    stream
        .read_exact(&mut pdb_hdr)
        .map_err(|e| format!("PalmDB header read failed: {}", e))?;
    let num_records = be_u16(&pdb_hdr, 76).unwrap_or(0) as usize;
    if num_records == 0 || num_records > 65535 {
        return Err("Invalid PalmDB record count".to_string());
    }

    let table_len = num_records * 8;
    if 78 + (table_len as u64) > file_len {
        return Err("Truncated PalmDB record table".to_string());
    }

    let mut table = vec![0u8; table_len];
    stream
        .read_exact(&mut table)
        .map_err(|e| format!("PalmDB record table read failed: {}", e))?;

    let mut offsets: Vec<u64> = Vec::with_capacity(num_records);
    for i in 0..num_records {
        let off = be_u32(&table, i * 8).unwrap_or(0) as u64;
        offsets.push(off);
    }

    let rec0_start = offsets[0];
    let rec0_end = offsets.get(1).copied().unwrap_or(file_len).min(file_len);
    if rec0_end <= rec0_start || rec0_end - rec0_start > 1024 * 1024 {
        return Err("Invalid Record 0 bounds in MOBI/AZW".to_string());
    }

    let mut rec0 = vec![0u8; (rec0_end - rec0_start) as usize];
    stream
        .seek(SeekFrom::Start(rec0_start))
        .map_err(|e| format!("Seek rec0 failed: {}", e))?;
    stream
        .read_exact(&mut rec0)
        .map_err(|e| format!("Read rec0 failed: {}", e))?;

    let mut first_image_index: Option<usize> = None;
    let mut exth_cover_offset: Option<u32> = None;
    let mut exth_thumb_offset: Option<u32> = None;

    if rec0.len() >= 132 && &rec0[16..20] == b"MOBI" {
        let mobi_hdr_len = be_u32(&rec0, 20).unwrap_or(0) as usize;
        if mobi_hdr_len >= 96 && rec0.len() >= 112 {
            let fimg = be_u32(&rec0, 108).unwrap_or(u32::MAX);
            if fimg != u32::MAX && (fimg as usize) < num_records {
                first_image_index = Some(fimg as usize);
            }
        }
        let exth_flags = be_u32(&rec0, 128).unwrap_or(0);
        let exth_off = 16usize.saturating_add(mobi_hdr_len);
        if (exth_flags & 0x40) != 0 && exth_off + 12 <= rec0.len() && &rec0[exth_off..exth_off + 4] == b"EXTH" {
            let exth_count = be_u32(&rec0, exth_off + 8).unwrap_or(0).min(512);
            let mut pos = exth_off + 12;
            for _ in 0..exth_count {
                if pos + 8 > rec0.len() {
                    break;
                }
                let r_type = be_u32(&rec0, pos).unwrap_or(0);
                let r_len = be_u32(&rec0, pos + 4).unwrap_or(0) as usize;
                if r_len < 8 || pos + r_len > rec0.len() {
                    break;
                }
                if r_len == 12 {
                    let val = be_u32(&rec0, pos + 8).unwrap_or(u32::MAX);
                    if r_type == 201 && val != u32::MAX {
                        exth_cover_offset = Some(val);
                    } else if r_type == 202 && val != u32::MAX {
                        exth_thumb_offset = Some(val);
                    }
                }
                pos += r_len;
            }
        }
    }

    let read_record = |stream: &mut dyn ReadSeek, idx: usize| -> Result<Vec<u8>, String> {
        let start = offsets[idx];
        let end = offsets
            .get(idx + 1)
            .copied()
            .unwrap_or(file_len)
            .min(file_len);
        if end <= start || (end - start) as usize > MAX_MOBI_SCAN_BYTES {
            return Err("Invalid image record size".to_string());
        }
        let mut buf = vec![0u8; (end - start) as usize];
        stream
            .seek(SeekFrom::Start(start))
            .map_err(|e| format!("Record seek failed: {}", e))?;
        stream
            .read_exact(&mut buf)
            .map_err(|e| format!("Record read failed: {}", e))?;
        Ok(buf)
    };

    if let Some(first_img) = first_image_index {
        for off_opt in [exth_cover_offset, exth_thumb_offset] {
            if let Some(rel) = off_opt {
                let target_idx = first_img.saturating_add(rel as usize);
                if target_idx < num_records {
                    if let Ok(bytes) = read_record(stream, target_idx) {
                        if looks_like_image_bytes(&bytes) {
                            if let Ok(img) = decode_and_resize_rgba(&bytes, max_dim) {
                                return Ok(img);
                            }
                        }
                    }
                }
            }
        }
    }

    // Fallback: scan up to 200 records from first_image_index (or record 1) and choose best cover-sized image
    let scan_start = first_image_index.unwrap_or(1).min(num_records);
    let scan_end = (scan_start + 200).min(num_records);
    let mut best_fallback: Option<RgbaImage> = None;
    let mut best_area = 0u64;

    for idx in scan_start..scan_end {
        let Ok(bytes) = read_record(stream, idx) else {
            continue;
        };
        if !looks_like_image_bytes(&bytes) {
            continue;
        }
        if let Ok(img) = decode_and_resize_rgba(&bytes, max_dim) {
            let area = (img.width() as u64) * (img.height() as u64);
            // Return immediately on first reasonably large cover-proportioned graphic
            if img.width() >= 96 && img.height() >= 96 {
                return Ok(img);
            }
            if area > best_area {
                best_area = area;
                best_fallback = Some(img);
            }
        }
    }

    best_fallback.ok_or_else(|| "No cover image found in MOBI/AZW".to_string())
}

fn extract_pdf_cover(stream: &mut dyn ReadSeek, max_dim: u32) -> Result<RgbaImage, String> {
    let file_len = stream
        .seek(SeekFrom::End(0))
        .map_err(|e| format!("PDF seek failed: {}", e))?;
    if file_len == 0 || file_len as usize > MAX_PDF_MEMORY_BYTES {
        return Err("PDF size empty or exceeds safe thumbnail memory limit".to_string());
    }

    let _ = stream.seek(SeekFrom::Start(0));
    let mut pdf_bytes = Vec::with_capacity(file_len as usize);
    stream
        .read_to_end(&mut pdf_bytes)
        .map_err(|e| format!("PDF read failed: {}", e))?;

    // Check %PDF signature (allow minor leading whitespace/garbage within first 1KB)
    let hdr_scan_len = pdf_bytes.len().min(1024);
    if !pdf_bytes[..hdr_scan_len].windows(4).any(|w| w == b"%PDF") {
        return Err("Invalid PDF signature".to_string());
    }

    // Run WinRT PdfDocument on an MTA worker thread so COM IAsyncOperation::get()
    // never blocks or fails inside an STA GUI thread.
    let handle = std::thread::spawn(move || render_pdf_page0_winrt(&pdf_bytes, max_dim));
    let rendered = handle
        .join()
        .map_err(|_| "PDF render worker thread panicked".to_string())??;

    // Fallback pure white background behind transparent PDF page renders
    let mut canvas = RgbaImage::from_pixel(rendered.width(), rendered.height(), Rgba([255, 255, 255, 255]));
    for (x, y, px) in rendered.enumerate_pixels() {
        let a = px[3] as f32 / 255.0;
        let inv = 1.0 - a;
        let r = ((px[0] as f32 * a) + (255.0 * inv)).round().clamp(0.0, 255.0) as u8;
        let g = ((px[1] as f32 * a) + (255.0 * inv)).round().clamp(0.0, 255.0) as u8;
        let b = ((px[2] as f32 * a) + (255.0 * inv)).round().clamp(0.0, 255.0) as u8;
        canvas.put_pixel(x, y, Rgba([r, g, b, 255]));
    }
    Ok(canvas)
}

fn render_pdf_page0_winrt(pdf_bytes: &[u8], max_dim: u32) -> Result<RgbaImage, String> {
    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Storage::Streams::{DataReader, DataWriter, InMemoryRandomAccessStream};
    use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};

    let init_res = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    let need_uninit = init_res.is_ok();

    let res = (|| -> Result<RgbaImage, String> {
        let in_stream = InMemoryRandomAccessStream::new()
            .map_err(|e| format!("Create InMemoryRandomAccessStream failed: {}", e))?;
        {
            let writer = DataWriter::CreateDataWriter(&in_stream)
                .map_err(|e| format!("Create DataWriter failed: {}", e))?;
            writer
                .WriteBytes(pdf_bytes)
                .map_err(|e| format!("PDF WriteBytes failed: {}", e))?;
            writer
                .StoreAsync()
                .map_err(|e| format!("StoreAsync failed: {}", e))?
                .get()
                .map_err(|e| format!("StoreAsync get failed: {}", e))?;
            writer
                .FlushAsync()
                .map_err(|e| format!("FlushAsync failed: {}", e))?
                .get()
                .map_err(|e| format!("FlushAsync get failed: {}", e))?;
            let _ = writer.DetachStream();
        }
        in_stream
            .Seek(0)
            .map_err(|e| format!("Stream Seek(0) failed: {}", e))?;

        let doc = PdfDocument::LoadFromStreamAsync(&in_stream)
            .map_err(|e| format!("PdfDocument::LoadFromStreamAsync failed: {}", e))?
            .get()
            .map_err(|e| format!("PdfDocument load failed (possibly password-protected): {}", e))?;

        if doc.PageCount().unwrap_or(0) == 0 {
            return Err("PDF has zero pages".to_string());
        }

        let page = doc
            .GetPage(0)
            .map_err(|e| format!("PdfDocument::GetPage(0) failed: {}", e))?;
        let page_size = page
            .Size()
            .map_err(|e| format!("PdfPage::Size failed: {}", e))?;

        let pw = (page_size.Width as f32).max(1.0);
        let ph = (page_size.Height as f32).max(1.0);
        let scale = (max_dim as f32) / pw.max(ph);
        let target_w = ((pw * scale).round() as u32).clamp(32, max_dim);
        let target_h = ((ph * scale).round() as u32).clamp(32, max_dim);

        let opts = PdfPageRenderOptions::new()
            .map_err(|e| format!("PdfPageRenderOptions failed: {}", e))?;
        let _ = opts.SetDestinationWidth(target_w);
        let _ = opts.SetDestinationHeight(target_h);

        let out_stream = InMemoryRandomAccessStream::new()
            .map_err(|e| format!("Output stream creation failed: {}", e))?;
        page.RenderWithOptionsToStreamAsync(&out_stream, &opts)
            .map_err(|e| format!("RenderWithOptionsToStreamAsync failed: {}", e))?
            .get()
            .map_err(|e| format!("PDF page render failed: {}", e))?;

        let out_size = out_stream
            .Size()
            .map_err(|e| format!("Output stream size failed: {}", e))?;
        if out_size == 0 || out_size > 16 * 1024 * 1024 {
            return Err("Rendered PDF output stream invalid size".to_string());
        }
        out_stream
            .Seek(0)
            .map_err(|e| format!("Output stream seek failed: {}", e))?;

        let reader = DataReader::CreateDataReader(&out_stream)
            .map_err(|e| format!("Create DataReader failed: {}", e))?;
        let size_u32 = out_size as u32;
        reader
            .LoadAsync(size_u32)
            .map_err(|e| format!("DataReader LoadAsync failed: {}", e))?
            .get()
            .map_err(|e| format!("DataReader LoadAsync get failed: {}", e))?;

        let mut png_bytes = vec![0u8; size_u32 as usize];
        reader
            .ReadBytes(&mut png_bytes)
            .map_err(|e| format!("DataReader ReadBytes failed: {}", e))?;

        let img = crate::image_dec::decode_bytes_strict(&png_bytes)
            .map_err(|e| format!("Decode rendered PDF image failed: {}", e))?;
        Ok(img.to_rgba8())
    })();

    if need_uninit {
        unsafe { RoUninitialize() };
    }
    res
}
