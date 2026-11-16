#![forbid(unsafe_code)]

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

/// Maximum XML events processed per EPUB metadata file to prevent XML tag-flood DoS.
const MAX_XML_EVENTS: usize = 25_000;
/// Maximum `<item>` entries retained from an OPF manifest.
const MAX_MANIFEST_ITEMS: usize = 5_000;

/// Resolves the canonical normalized archive path of the cover image declared in an EPUB's
/// `META-INF/container.xml` and `.opf` package document(s).
pub fn resolve_epub_cover(container_xml: &str, opfs: &[(String, String)]) -> Option<String> {
    if opfs.is_empty() {
        return None;
    }

    let opf_path = parse_container_rootfile(container_xml);
    let chosen_opf = opf_path
        .as_deref()
        .and_then(|want| opfs.iter().find(|(p, _)| p.eq_ignore_ascii_case(want)))
        .unwrap_or(&opfs[0]);

    let href = parse_opf_cover_href(&chosen_opf.1)?;
    let href_clean = href.split(['#', '?']).next().unwrap_or(&href);
    // Reject absolute paths, backslashes, or drive letters in EPUB href
    if href_clean.starts_with('/') || href_clean.starts_with('\\') || href_clean.contains(':') {
        return None;
    }
    let decoded = percent_decode(href_clean);

    let base_dir = chosen_opf
        .0
        .rfind('/')
        .map(|idx| &chosen_opf.0[..=idx])
        .unwrap_or("");

    let raw_combined = format!("{base_dir}{decoded}");
    let normalized = normalize_archive_path(&raw_combined)?;
    crate::archive::sanitize_archive_path(&normalized)
}

/// Inspects an EPUB OPF manifest cover href for security evasion variants:
/// - Leading slash / absolute path evasion (`/absolute/...`, `\root\...`)
/// - Drive letters or NTFS Alternate Data Streams (`C:...`, `safe.png:stream`)
/// - URL-encoded or raw directory traversal (`%2e%2e`, `..`)
pub fn inspect_opf_href_security(href: &str) -> Option<&'static str> {
    let href_clean = href.split(['#', '?']).next().unwrap_or(href);
    if href_clean.starts_with('/') || href_clean.starts_with('\\') {
        return Some("EPUB OPF leading slash / absolute path evasion");
    }
    if href_clean.contains(':') {
        return Some("EPUB OPF drive letter or ADS stream evasion");
    }
    let decoded = percent_decode(href_clean);
    let norm = decoded.replace('\\', "/");
    for seg in norm.split('/') {
        if seg == ".." {
            return Some("EPUB OPF parent directory traversal evasion");
        }
    }
    None
}

fn parse_container_rootfile(xml: &str) -> Option<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut events = 0usize;
    loop {
        events += 1;
        if events > MAX_XML_EVENTS {
            break;
        }
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                if local_name_eq(e, b"rootfile") {
                    if let Some(fp) = attr_val(&reader, e, b"full-path") {
                        if !fp.is_empty() {
                            return Some(fp.replace('\\', "/"));
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    None
}

pub fn parse_opf_cover_href(opf_xml: &str) -> Option<String> {
    let mut reader = Reader::from_str(opf_xml);
    reader.config_mut().trim_text(true);

    let mut cover_id: Option<String> = None;
    let mut items: Vec<(String, String, String, String)> = Vec::new(); // (id, href, media_type, properties)
    let mut events = 0usize;

    loop {
        events += 1;
        if events > MAX_XML_EVENTS {
            break;
        }
        match reader.read_event() {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                if local_name_eq(e, b"meta") {
                    if let Some(name) = attr_val(&reader, e, b"name") {
                        if name.eq_ignore_ascii_case("cover") {
                            if let Some(content) = attr_val(&reader, e, b"content") {
                                if !content.is_empty() {
                                    cover_id = Some(content);
                                }
                            }
                        }
                    }
                } else if local_name_eq(e, b"item") && items.len() < MAX_MANIFEST_ITEMS {
                    let href = attr_val(&reader, e, b"href").unwrap_or_default();
                    if !href.is_empty() {
                        let id = attr_val(&reader, e, b"id").unwrap_or_default();
                        let mt = attr_val(&reader, e, b"media-type").unwrap_or_default();
                        let props = attr_val(&reader, e, b"properties").unwrap_or_default();
                        items.push((id, href, mt, props));
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    // Priority 1: EPUB 3 `properties="cover-image"`
    if let Some((_, href, _, _)) = items
        .iter()
        .find(|(_, _, _, props)| props.split_whitespace().any(|t| t == "cover-image"))
    {
        return Some(href.clone());
    }

    // Priority 2: EPUB 2 `<meta name="cover" content="ID"/>` matching `<item id="ID" .../>`
    if let Some(ref cid) = cover_id {
        if let Some((_, href, mt, _)) = items.iter().find(|(id, _, _, _)| id == cid) {
            if mt.is_empty() || mt.to_ascii_lowercase().starts_with("image/") {
                return Some(href.clone());
            }
        }
    }

    // Priority 3: Heuristic `<item media-type="image/..." id/href containing "cover"/>`
    if let Some((_, href, _, _)) = items.iter().find(|(id, href, mt, _)| {
        mt.to_ascii_lowercase().starts_with("image/")
            && (id.to_ascii_lowercase().contains("cover")
                || href.to_ascii_lowercase().contains("cover"))
    }) {
        return Some(href.clone());
    }

    None
}

fn local_name_eq(e: &BytesStart<'_>, target: &[u8]) -> bool {
    let raw = e.name().into_inner();
    let local = raw
        .iter()
        .rposition(|&b| b == b':')
        .map(|i| &raw[i + 1..])
        .unwrap_or(raw);
    local.eq_ignore_ascii_case(target)
}

fn attr_val(reader: &Reader<&[u8]>, e: &BytesStart<'_>, name: &[u8]) -> Option<String> {
    for attr in e.attributes().flatten() {
        let key = attr.key.into_inner();
        let local = key
            .iter()
            .rposition(|&b| b == b':')
            .map(|i| &key[i + 1..])
            .unwrap_or(key);
        if local.eq_ignore_ascii_case(name) {
            if let Ok(val) = attr.decode_and_unescape_value(reader.decoder()) {
                return Some(val.into_owned());
            }
        }
    }
    None
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (from_hex(bytes[i + 1]), from_hex(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub fn normalize_archive_path(path: &str) -> Option<String> {
    let norm = path.replace('\\', "/");
    if norm.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for seg in norm.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => {
                if s.contains(':') {
                    return None;
                }
                parts.push(s);
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}
