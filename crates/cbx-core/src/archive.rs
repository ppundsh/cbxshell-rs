//! Archive format detection, cover candidate discovery, and entry counting.
//!
//! 100% safe Rust (`#![forbid(unsafe_code)]`). Enforces strict defenses against:
//! - Decompression bombs (Zip bombs, 7z solid block bombs, RAR bombs, TAR bombs)
//! - Compression-ratio anomalies (`MAX_COMPRESSION_RATIO` = 200x for entries > 8 MiB)
//! - Solid-archive drain exhaustion (`MAX_SOLID_DRAIN_BYTES` = 128 MiB cumulative cap)
//! - Million-entry central-directory DoS (`MAX_SCAN_ENTRIES` = 10,000)
//! - Path traversal (`..`), NUL-byte injection, and symlink/device entries
//! - Scan timeout deadlines

#![forbid(unsafe_code)]

use crate::epub::resolve_epub_cover;
use std::cmp::Ordering;
use std::io::{Read, Seek, SeekFrom};
use std::time::{Duration, Instant};

/// Maximum number of archive entries inspected before stopping the scan.
pub const MAX_SCAN_ENTRIES: usize = 10_000;
/// Maximum cumulative bytes drained across preceding entries in a solid 7z/RAR block (128 MiB).
pub const MAX_SOLID_DRAIN_BYTES: u64 = 128 * 1024 * 1024;
/// Maximum allowed uncompressed-to-compressed ratio for large entries (> 8 MiB).
pub const MAX_COMPRESSION_RATIO: u64 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RiskRules {
    pub executables: bool,
    pub traversal: bool,
    pub hidden_files: bool,
    pub text_files: bool,
    pub sensitive: bool,
    pub evasion: bool,
}

impl Default for RiskRules {
    fn default() -> Self {
        Self {
            executables: true,
            traversal: true,
            hidden_files: true,
            text_files: true,
            sensitive: true,
            evasion: true,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CoverOptions {
    pub sort: bool,
    pub prefer_continuous_sequence: bool,
    pub max_image_bytes: u64,
    pub scan_timeout: Duration,
    pub anomaly_detection: bool,
    pub risk_rules: RiskRules,
}

impl Default for CoverOptions {
    fn default() -> Self {
        Self {
            sort: true,
            prefer_continuous_sequence: true,
            max_image_bytes: 256 * 1024 * 1024,
            scan_timeout: Duration::from_secs(4),
            anomaly_detection: true,
            risk_rules: RiskRules::default(),
        }
    }
}

fn continuous_sequence_group_key(path: &str) -> (usize, String, String) {
    let norm = path.replace('\\', "/");
    let trimmed = norm.trim_start_matches("./").trim_start_matches('/');
    let (parent, file) = match trimmed.rfind('/') {
        Some(pos) => (&trimmed[..pos], &trimmed[pos + 1..]),
        None => ("", trimmed),
    };
    let depth = if parent.is_empty() {
        0
    } else {
        parent.split('/').filter(|s| !s.is_empty()).count()
    };

    let lower = file.to_ascii_lowercase();
    let stem = match lower.rfind('.') {
        Some(pos) if pos > 0 => &lower[..pos],
        _ => lower.as_str(),
    };
    let mut skeleton = String::with_capacity(stem.len());
    let mut in_digits = false;
    let mut has_digits = false;
    for ch in stem.chars() {
        if ch.is_ascii_digit() {
            has_digits = true;
            if !in_digits {
                skeleton.push('#');
                in_digits = true;
            }
        } else {
            in_digits = false;
            skeleton.push(ch);
        }
    }
    if !has_digits {
        return (depth, parent.to_ascii_lowercase(), format!("@nonseq:{}", stem));
    }
    (depth, parent.to_ascii_lowercase(), skeleton)
}

/// Selects the cover image index from `(entry_index, entry_name)` candidates.
///
/// When `sort` and `prefer_continuous` are true, prefers the earliest entry (by natural sort)
/// of the largest continuous numbered filename group (length >= 2) at the shallowest/first
/// folder containing a continuous sequence. For example, `_000.png.._062.png` beats stray `000000.png`.
pub fn pick_best_cover_index(
    entries: &[(usize, String)],
    sort: bool,
    prefer_continuous: bool,
) -> Option<usize> {
    if entries.is_empty() {
        return None;
    }
    if !sort {
        return Some(entries[0].0);
    }

    if prefer_continuous {
        use std::collections::HashMap;
        let mut by_folder: HashMap<(usize, String), HashMap<String, Vec<(usize, &str)>>> =
            HashMap::new();

        for (idx, name) in entries {
            let (depth, parent, skel) = continuous_sequence_group_key(name);
            if !skel.starts_with("@nonseq:") {
                by_folder
                    .entry((depth, parent))
                    .or_default()
                    .entry(skel)
                    .or_default()
                    .push((*idx, name.as_str()));
            }
        }

        // Order folders by (depth, natural_cmp(parent))
        let mut folders: Vec<(usize, String, HashMap<String, Vec<(usize, &str)>>)> = by_folder
            .into_iter()
            .filter_map(|((depth, parent), groups)| {
                let has_seq = groups.values().any(|v| v.len() >= 2);
                if has_seq {
                    Some((depth, parent, groups))
                } else {
                    None
                }
            })
            .collect();

        folders.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| natural_cmp(&a.1, &b.1)));

        if let Some((_, _, groups)) = folders.into_iter().next() {
            let mut best: Option<(usize, String, usize)> = None;
            for (_skel, mut items) in groups {
                if items.len() < 2 {
                    continue;
                }
                items.sort_by(|a, b| natural_cmp(a.1, b.1));
                let (first_idx, first_name) = items[0];
                let count = items.len();
                let is_better = match &best {
                    None => true,
                    Some((best_count, best_first, _)) => {
                        if count != *best_count {
                            count > *best_count
                        } else {
                            natural_cmp_less(first_name, best_first)
                        }
                    }
                };
                if is_better {
                    best = Some((count, first_name.to_string(), first_idx));
                }
            }
            if let Some((_, _, best_idx)) = best {
                return Some(best_idx);
            }
        }
    }

    let mut best_pos = 0usize;
    for i in 1..entries.len() {
        if natural_cmp_less(&entries[i].1, &entries[best_pos].1) {
            best_pos = i;
        }
    }
    Some(entries[best_pos].0)
}

#[cfg(test)]
mod continuous_sequence_tests {
    use super::pick_best_cover_index;

    #[test]
    fn prefers_continuous_sequence_over_zero_padded_outlier() {
        let mut entries: Vec<(usize, String)> = Vec::new();
        for i in 0..=62 {
            entries.push((i, format!("_{:03}.png", i)));
        }
        entries.push((63, "000000.png".to_string()));

        // Without continuous preference: ASCII '0' < '_' picks index 63 ("000000.png")
        assert_eq!(pick_best_cover_index(&entries, true, false), Some(63));
        // With continuous preference: picks index 0 ("_000.png")
        assert_eq!(pick_best_cover_index(&entries, true, true), Some(0));
    }

    #[test]
    fn picks_chapter_01_before_longer_chapter_02() {
        let mut entries: Vec<(usize, String)> = Vec::new();
        for i in 1..=10 {
            entries.push((entries.len(), format!("ch01/{:03}.jpg", i)));
        }
        for i in 1..=30 {
            entries.push((entries.len(), format!("ch02/{:03}.jpg", i)));
        }
        assert_eq!(pick_best_cover_index(&entries, true, true), Some(0));
    }
}

#[derive(Debug, Clone)]
pub struct CoverCandidate {
    pub name: String,
    pub data: Vec<u8>,
    pub from_epub_metadata: bool,
    pub is_epub: bool,
    pub container: ContainerFormat,
    pub has_risk: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ArchiveStats {
    pub images: u32,
    pub files: u32,
    pub complete: bool,
    pub has_risk: bool,
    pub risk_reasons: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerFormat {
    Zip,
    SevenZ,
    Rar,
    Tar,
}

impl ContainerFormat {
    pub const fn default_badge_label(self) -> &'static str {
        match self {
            Self::Zip => "ZIP",
            Self::Rar => "RAR",
            Self::SevenZ => "7Z",
            Self::Tar => "TAR",
        }
    }
}

/// Resolves the static archive badge label from an optional filename/extension hint
/// (`"CBZ"`, `"ZIP"`, `"CBR"`, `"RAR"`, `"CB7"`, `"7Z"`, `"CBT"`, `"TAR"`), falling back to
/// the detected container format if no extension hint is available.
pub fn archive_badge_label(ext_or_name: Option<&str>, container: ContainerFormat, is_epub: bool) -> &'static str {
    if is_epub {
        return "EPUB";
    }
    if let Some(s) = ext_or_name {
        let lower = s.trim().to_ascii_lowercase();
        let ext = lower.rsplit('.').next().unwrap_or(&lower);
        match ext {
            "cbz" => return "CBZ",
            "zip" => return "ZIP",
            "cbr" => return "CBR",
            "rar" => return "RAR",
            "cb7" => return "CB7",
            "7z" => return "7Z",
            "cbt" => return "CBT",
            "tar" => return "TAR",
            "epub" => return "EPUB",
            _ => {}
        }
    }
    container.default_badge_label()
}

/// Supported 100% Pure-Rust image & HDR extensions inside comic/e-book archives:
/// - Classic: JPEG, PNG/APNG, GIF, BMP/DIB, TIFF (8/16/32-bit float HDR), ICO/CUR
/// - Modern Next-Gen: WebP (static & animated), JPEG XL (`.jxl`), QOI (`.qoi`)
/// - HDR & High Bit-Depth: Radiance RGBE HDR (`.hdr`/`.pic`), OpenEXR (`.exr`), Farbfeld (`.ff`)
/// - Texture / Portable: DDS (`.dds`), TGA (`.tga`), PNM (`.pbm`/`.pgm`/`.ppm`/`.pam`)
const IMAGE_EXTENSIONS: &[&str] = &[
    ".jpg", ".jpeg", ".jpe", ".jfif", ".png", ".apng", ".gif", ".bmp", ".dib", ".tif", ".tiff",
    ".ico", ".cur", ".webp", ".jxl", ".qoi", ".hdr", ".pic", ".exr", ".dds", ".tga", ".pbm",
    ".pgm", ".ppm", ".pam", ".ff",
];

pub fn is_image_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    IMAGE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

/// Checks if a path segment stem matches a Windows DOS reserved device name
/// (`CON`, `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`, `COM0`..`COM9`, `LPT0`..`LPT9`, including the
/// superscript-digit forms `COM¹`..`COM³` / `LPT¹`..`LPT³` that Win32 also maps to devices).
/// Trailing spaces are ignored because Win32 strips them (`"CON .png"` opens the console device).
fn is_windows_reserved_device_name(name: &str) -> bool {
    let n = name.trim_end_matches(' ').to_uppercase();
    if matches!(n.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$") {
        return true;
    }
    let mut chars = n.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    if prefix == "COM" || prefix == "LPT" {
        let rest: Vec<char> = chars.collect();
        return rest.len() == 1 && matches!(rest[0], '0'..='9' | '\u{00B9}' | '\u{00B2}' | '\u{00B3}');
    }
    false
}

pub const DANGEROUS_EXTENSIONS: &[&str] = &[
    ".exe", ".bat", ".cmd", ".vbs", ".vbe", ".js", ".jse", ".ps1", ".psm1", ".scr", ".pif", ".com",
    ".hta", ".cpl", ".msi", ".msp", ".dll", ".sys", ".wsf", ".wsh", ".wsc", ".sct", ".reg", ".jar",
    ".lnk", ".url",
];

pub const SENSITIVE_EXTENSIONS: &[&str] = &[
    ".key", ".pem", ".crt", ".p12", ".pfx", ".ovpn", ".kdbx",
    ".env", ".secret", ".token", ".id_rsa", ".id_ed25519", ".passwd",
];

pub const TEXT_OR_PROMPT_INJECTION_EXTENSIONS: &[&str] = &[
    ".txt", ".text", ".md", ".markdown", ".prompt", ".instructions",
    ".nfo", ".diz", ".log", ".json", ".jsonl", ".yaml", ".yml",
    ".xml", ".csv", ".tsv", ".ini", ".cfg", ".conf", ".rtf",
    ".htm", ".html",
];

/// Returns true if the file is a standard, essential EPUB structural/content component.
fn is_standard_epub_file(norm: &str) -> bool {
    let lower = norm.to_ascii_lowercase();
    lower == "mimetype"
        || lower == "meta-inf/container.xml"
        || lower.starts_with("meta-inf/")
        || lower.ends_with(".opf")
        || lower.ends_with(".ncx")
        || lower.ends_with(".xhtml")
        || lower.ends_with(".html")
        || lower.ends_with(".htm")
        || lower.ends_with(".xml")
        || lower.ends_with(".css")
}

/// Comprehensive security inspection for archive members.
/// Enforces detection of:
/// - Path traversal (`..`, `...`, multi-dot)
/// - Absolute paths (leading `/` or `\`)
/// - Windows drive letters / Alternate Data Streams (`:`)
/// - Windows DOS reserved device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1-9`, `LPT1-9`)
/// - Trailing dots or trailing spaces (Win32 filename evasion)
/// - Unicode RTLO / BiDi override spoofing (`U+202A..U+202E`, `U+2066..U+2069`)
/// - macOS resource fork directory (`__MACOSX/`)
/// - Hidden directories or dotfiles (`.git/`, `.hidden/`, `.secret/`, `.secret.txt`, `.env`, `.DS_Store`)
/// - Disguised executable or script binaries (`.exe`, `.bat`, `.ps1`, `.scr`, etc.)
/// - Sensitive credential or secret files (`.key`, `.pem`, `.kdbx`, `.token`, etc.)
/// - Plain text / structured files as AI prompt injection / instruction attack vectors
/// - Decompression ratio bombs (> 200:1)
pub fn inspect_entry_security(
    raw_name: &str,
    uncomp_size: u64,
    comp_size: u64,
    is_epub: bool,
) -> Option<&'static str> {
    inspect_entry_security_with_rules(raw_name, uncomp_size, comp_size, is_epub, None)
}

pub fn inspect_entry_security_with_rules(
    raw_name: &str,
    uncomp_size: u64,
    comp_size: u64,
    is_epub: bool,
    rules: Option<&RiskRules>,
) -> Option<&'static str> {
    let default_rules = RiskRules::default();
    let rules = rules.unwrap_or(&default_rules);

    if raw_name.is_empty() || raw_name.len() > 1024 {
        return Some("Oversized or empty entry path");
    }
    if rules.evasion {
        if raw_name.bytes().any(|b| b == 0 || (b < 0x20 && b != b'\t')) {
            return Some("Control characters or NUL byte injection");
        }
        if raw_name.chars().any(|c| {
            matches!(
                c,
                '\u{200B}'..='\u{200F}'
                    | '\u{202A}'..='\u{202E}'
                    | '\u{2060}'..='\u{2064}'
                    | '\u{2066}'..='\u{2069}'
                    | '\u{FEFF}'
            )
        }) {
            return Some("Unicode RTLO / BiDi or zero-width extension spoofing");
        }
    }
    let norm = raw_name.replace('\\', "/");
    if rules.traversal && norm.starts_with('/') {
        return Some("Absolute root path evasion");
    }

    if rules.hidden_files {
        // Flag macOS resource fork metadata (__MACOSX)
        if norm.starts_with("__MACOSX/") || norm.contains("/__MACOSX/") || norm == "__MACOSX" {
            return Some("macOS resource fork metadata (__MACOSX)");
        }
    }

    for seg in norm.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if rules.traversal {
                    return Some("Parent directory traversal (.. escape)");
                }
            }
            s => {
                if rules.hidden_files && s.starts_with('.') {
                    return Some("Hidden directory or dotfile entry");
                }
                if rules.traversal && s.contains(':') {
                    return Some("NTFS Alternate Data Stream or drive letter");
                }
                if rules.evasion {
                    if s.ends_with('.') || s.ends_with(' ') {
                        return Some("Win32 trailing dot or space evasion");
                    }
                    let seg_trimmed = s.trim_matches([' ', '.']);
                    if seg_trimmed.is_empty() {
                        return Some("Multi-dot or blank segment evasion");
                    }
                    let stem = seg_trimmed.split('.').next().unwrap_or(seg_trimmed);
                    if is_windows_reserved_device_name(stem) {
                        return Some("Windows DOS reserved device name");
                    }
                }
            }
        }
    }
    let lower = raw_name.to_ascii_lowercase();
    if rules.executables {
        for ext in DANGEROUS_EXTENSIONS {
            if lower.ends_with(ext) {
                return Some("Disguised executable or script binary");
            }
        }
    }
    if rules.sensitive {
        for ext in SENSITIVE_EXTENSIONS {
            if lower.ends_with(ext) {
                return Some("Sensitive credential or secret file");
            }
        }
    }

    // Treat plain text / structured files as threats if enabled
    if rules.text_files && (!is_epub || !is_standard_epub_file(&norm)) {
        for ext in TEXT_OR_PROMPT_INJECTION_EXTENSIONS {
            if lower.ends_with(ext) {
                return Some("Plain text file (potential instruction payload)");
            }
        }
    }

    if uncomp_size > 8 * 1024 * 1024
        && (comp_size == 0 || (uncomp_size / comp_size) > MAX_COMPRESSION_RATIO)
    {
        return Some("Abnormal decompression ratio bomb (>200:1)");
    }
    None
}

/// Sanitizes an archive member path, enforcing defense-in-depth against:
/// - Relative path traversal (`..` escapes, multi-dot `...`, CWE-22/CWE-23)
/// - Absolute paths (leading `/` or `\`)
/// - Windows drive letters and Alternate Data Streams (colons `:` anywhere)
/// - Windows reserved device names (CON, PRN, AUX, NUL, COM1-9, LPT1-9)
/// - Unicode bidirectional overrides (RTLO extension spoofing, U+202A..U+202E, U+2066..U+2069)
/// - NUL bytes and ASCII control characters (< 0x20)
/// - Excessive nesting (> 32 levels) and oversized path lengths (> 1024 bytes)
pub fn sanitize_archive_path(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.len() > 1024 {
        return None;
    }
    // Reject NUL bytes and control characters
    if raw.bytes().any(|b| b == 0 || (b < 0x20 && b != b'\t')) {
        return None;
    }
    // Reject Unicode bidirectional overrides and zero-width / invisible format chars
    if raw.chars().any(|c| {
        matches!(
            c,
            '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
        )
    }) {
        return None;
    }
    let norm = raw.replace('\\', "/");
    // Reject absolute paths
    if norm.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::with_capacity(8);
    for seg in norm.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                // Strictly reject any path containing parent-directory traversal (CWE-22 / CWE-23)
                return None;
            }
            s => {
                // Reject Windows drive letters, alternate data streams (ADS), or colons anywhere
                if s.contains(':') {
                    return None;
                }
                // Reject trailing dots or spaces (stripped by Win32 APIs, dangerous for extension/device bypasses)
                if s.ends_with('.') || s.ends_with(' ') {
                    return None;
                }
                // Reject empty or multi-dot segments (e.g. "...", "....", or whitespace-only)
                let seg_trimmed = s.trim_matches([' ', '.']);
                if seg_trimmed.is_empty() {
                    return None;
                }
                // Reject Windows DOS reserved device names (even with file extensions, e.g. "CON.png", "AUX.tar.gz")
                let stem = seg_trimmed.split('.').next().unwrap_or(seg_trimmed);
                if is_windows_reserved_device_name(stem) {
                    return None;
                }
                parts.push(s);
                if parts.len() > 32 {
                    return None;
                }
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

pub fn is_junk_name(name: &str) -> bool {
    let norm = name.replace('\\', "/");
    if norm.starts_with("__MACOSX/") || norm.contains("/__MACOSX/") {
        return true;
    }
    let file_part = norm.rsplit('/').next().unwrap_or(&norm);
    file_part.starts_with("._")
        || file_part.eq_ignore_ascii_case("Thumbs.db")
        || file_part.eq_ignore_ascii_case("desktop.ini")
        || file_part.eq_ignore_ascii_case(".DS_Store")
}

/// Pure-Rust natural (logical) string comparison matching Windows Explorer (`StrCmpLogicalW` semantics):
/// compares non-digit segments case-insensitively and digit runs numerically (with shorter leading-zero
/// runs ordered before longer ones when numeric values are equal).
pub fn natural_cmp_less(a: &str, b: &str) -> bool {
    natural_cmp(a, b) == Ordering::Less
}

pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let mut ia = a.as_bytes().iter().peekable();
    let mut ib = b.as_bytes().iter().peekable();

    loop {
        match (ia.peek(), ib.peek()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(&&ca), Some(&&cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    // Count leading zeros and collect significant digits
                    let mut zeros_a = 0usize;
                    while ia.peek() == Some(&&b'0') {
                        ia.next();
                        zeros_a += 1;
                    }
                    let mut digits_a = Vec::new();
                    while let Some(&&d) = ia.peek() {
                        if d.is_ascii_digit() {
                            digits_a.push(d);
                            ia.next();
                        } else {
                            break;
                        }
                    }

                    let mut zeros_b = 0usize;
                    while ib.peek() == Some(&&b'0') {
                        ib.next();
                        zeros_b += 1;
                    }
                    let mut digits_b = Vec::new();
                    while let Some(&&d) = ib.peek() {
                        if d.is_ascii_digit() {
                            digits_b.push(d);
                            ib.next();
                        } else {
                            break;
                        }
                    }

                    // Compare by number of significant digits first, then lexicographically
                    match digits_a.len().cmp(&digits_b.len()) {
                        Ordering::Equal => match digits_a.cmp(&digits_b) {
                            Ordering::Equal => match zeros_a.cmp(&zeros_b) {
                                Ordering::Equal => continue,
                                ord => return ord,
                            },
                            ord => return ord,
                        },
                        ord => return ord,
                    }
                } else {
                    ia.next();
                    ib.next();
                    // Prefer path separator '/' before other characters so directory grouping stays natural
                    if ca != cb {
                        if ca == b'/' {
                            return Ordering::Less;
                        }
                        if cb == b'/' {
                            return Ordering::Greater;
                        }
                        let la = ca.to_ascii_lowercase();
                        let lb = cb.to_ascii_lowercase();
                        if la != lb {
                            return la.cmp(&lb);
                        }
                    }
                }
            }
        }
    }
}

pub fn sniff_container_format<R: Read + Seek>(reader: &mut R) -> Result<ContainerFormat, String> {
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut header = [0u8; 512];
    let n = read_up_to(reader, &mut header).map_err(|e| e.to_string())?;
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;

    if n >= 4
        && &header[0..2] == b"PK"
        && matches!(&header[2..4], b"\x03\x04" | b"\x05\x06" | b"\x07\x08")
    {
        return Ok(ContainerFormat::Zip);
    }
    if n >= 6 && &header[0..6] == b"7z\xBC\xAF\x27\x1C" {
        return Ok(ContainerFormat::SevenZ);
    }
    if n >= 7 && &header[0..6] == b"Rar!\x1A\x07" {
        return Ok(ContainerFormat::Rar);
    }
    if n >= 262 && &header[257..262] == b"ustar" {
        return Ok(ContainerFormat::Tar);
    }
    // Self-extracting or offset ZIP archives still have a central directory at EOF; try ZIP as default.
    Ok(ContainerFormat::Zip)
}

fn read_up_to<R: Read + ?Sized>(r: &mut R, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        match r.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(total)
}

/// Reads at most `max_bytes` from `r` in 64 KiB chunks while checking `deadline`.
/// Never pre-allocates more than 64 KiB upfront so fake large header sizes cannot trigger OOM.
pub fn read_bounded_with_deadline<R: Read>(
    mut r: R,
    max_bytes: u64,
    deadline: Instant,
) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(64 * 1024);
    let mut chunk = [0u8; 64 * 1024];
    loop {
        if Instant::now() > deadline {
            return Err("Archive extraction timed out".into());
        }
        let n = match r.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        };
        if (out.len() as u64).saturating_add(n as u64) > max_bytes {
            return Err(format!("Entry exceeds size limit ({max_bytes} bytes)"));
        }
        out.extend_from_slice(&chunk[..n]);
    }
    Ok(out)
}

/// Drains a stream in 64 KiB chunks while enforcing both a cumulative byte cap and a deadline.
fn drain_bounded<R: Read>(
    mut r: R,
    drained_so_far: &mut u64,
    max_drain_bytes: u64,
    deadline: Instant,
) -> Result<(), String> {
    let mut sink_buf = [0u8; 64 * 1024];
    loop {
        if Instant::now() > deadline {
            return Err("Solid block drain timed out".into());
        }
        let n = match r.read(&mut sink_buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        };
        *drained_so_far = drained_so_far.saturating_add(n as u64);
        if *drained_so_far > max_drain_bytes {
            return Err("Solid archive exceeded cumulative drain budget (potential bomb)".into());
        }
    }
    Ok(())
}

/// Discovers and extracts the cover image from a seekable stream.
pub fn find_cover<R: Read + Seek>(
    reader: &mut R,
    opt: &CoverOptions,
    exclude: &[String],
) -> Result<CoverCandidate, String> {
    match sniff_container_format(reader)? {
        ContainerFormat::Zip => find_cover_zip(reader, opt, exclude),
        ContainerFormat::SevenZ => find_cover_7z(reader, opt, exclude),
        ContainerFormat::Tar => find_cover_tar(reader, opt, exclude),
        ContainerFormat::Rar => crate::rar_pure::find_cover_rar(reader, opt, exclude),
    }
}

/// Counts total files and image files inside an archive for Explorer's `IQueryInfo` tooltip.
pub fn count_entries<R: Read + Seek>(
    reader: &mut R,
    timeout: Duration,
) -> Result<ArchiveStats, String> {
    match sniff_container_format(reader)? {
        ContainerFormat::Zip => count_entries_zip(reader, timeout),
        ContainerFormat::SevenZ => count_entries_7z(reader, timeout),
        ContainerFormat::Tar => count_entries_tar(reader, timeout),
        ContainerFormat::Rar => crate::rar_pure::count_entries_rar(reader, timeout),
    }
}

// ---------------------------------------------------------------------------
// ZIP / ZIP64 / CBZ / EPUB (pure Rust via `zip` crate)

/// Fast pre-check that a `PK\x05\x06` End of Central Directory signature exists within the
/// last 65,557 bytes of the stream. This prevents `zip` 2.6's backward scanner from walking
/// across gigabytes of data on truncated or non-ZIP files.
pub(crate) fn verify_zip_eocd_tail<R: Read + Seek + ?Sized>(reader: &mut R) -> Result<(), String> {
    const MAX_EOCD_SEARCH: u64 = 65_535 + 22 + 128;
    let file_len = reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    if file_len < 22 {
        return Err("Archive is smaller than minimum ZIP EOCD size (22 bytes)".into());
    }
    let tail_len = file_len.min(MAX_EOCD_SEARCH) as usize;
    reader
        .seek(SeekFrom::Start(file_len - tail_len as u64))
        .map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; tail_len];
    let n = read_up_to(reader, &mut buf).map_err(|e| e.to_string())?;
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    if n < 22 || !buf[..n].windows(4).rev().any(|w| w == b"PK\x05\x06") {
        return Err("Invalid ZIP/ZIP64: missing End of Central Directory signature in tail".into());
    }
    Ok(())
}

fn find_cover_zip<R: Read + Seek>(
    reader: &mut R,
    opt: &CoverOptions,
    exclude: &[String],
) -> Result<CoverCandidate, String> {
    verify_zip_eocd_tail(reader)?;
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| format!("Invalid ZIP/ZIP64: {e}"))?;
    let deadline = Instant::now() + opt.scan_timeout;

    let mut is_epub = zip.file_names().take(MAX_SCAN_ENTRIES).any(|n| {
        let norm = n.replace('\\', "/");
        norm.eq_ignore_ascii_case("META-INF/container.xml")
            || norm.to_ascii_lowercase().ends_with(".opf")
    });
    let mut container_xml = String::new();
    let mut opfs: Vec<(String, String)> = Vec::new();
    let mut image_entries: Vec<(usize, String)> = Vec::new();

    let mut best_idx: Option<usize> = None;
    let mut best_name = String::new();
    let mut has_risk = false;

    let total_entries = zip.len().min(MAX_SCAN_ENTRIES);
    for i in 0..total_entries {
        if Instant::now() > deadline {
            break;
        }
        let Ok(file) = zip.by_index_raw(i) else {
            continue;
        };
        let raw_name_bytes = file.name_raw();
        let decoded_name = crate::encoding::decode_archive_filename(raw_name_bytes);
        let raw_name = decoded_name.as_str();
        let uncomp_size = file.size();
        let comp_size = file.compressed_size();
        if inspect_entry_security_with_rules(raw_name, uncomp_size, comp_size, is_epub, Some(&opt.risk_rules)).is_some() {
            has_risk = true;
        }
        if file.is_dir() || file.encrypted() || file.is_symlink() {
            continue;
        }
        let Some(name) = sanitize_archive_path(raw_name) else {
            continue;
        };
        if is_junk_name(&name) {
            continue;
        }
        drop(file);

        // Reject extreme compression ratios (Zip Bomb protection)
        if uncomp_size > 8 * 1024 * 1024
            && (comp_size == 0 || (uncomp_size / comp_size) > MAX_COMPRESSION_RATIO)
        {
            continue;
        }

        // Check for EPUB metadata files
        if name.eq_ignore_ascii_case("mimetype") && uncomp_size <= 16 * 1024 {
            if let Ok(f) = zip.by_index(i) {
                if let Ok(bytes) = read_bounded_with_deadline(f, 16 * 1024, deadline) {
                    if bytes.starts_with(b"application/epub+zip") {
                        is_epub = true;
                    }
                }
            }
            continue;
        }
        if name.eq_ignore_ascii_case("META-INF/container.xml") && uncomp_size <= 64 * 1024 {
            if let Ok(f) = zip.by_index(i) {
                if let Ok(bytes) = read_bounded_with_deadline(f, 64 * 1024, deadline) {
                    container_xml = String::from_utf8_lossy(&bytes).into_owned();
                    is_epub = true;
                }
            }
            continue;
        }
        if name.to_ascii_lowercase().ends_with(".opf")
            && opfs.len() < 4
            && uncomp_size <= 1024 * 1024
        {
            if let Ok(f) = zip.by_index(i) {
                if let Ok(bytes) = read_bounded_with_deadline(f, 1024 * 1024, deadline) {
                    opfs.push((name, String::from_utf8_lossy(&bytes).into_owned()));
                    is_epub = true;
                }
            }
            continue;
        }

        if !is_image_name(&name) || exclude.iter().any(|ex| ex == &name) {
            continue;
        }
        if uncomp_size == 0 || uncomp_size > opt.max_image_bytes {
            continue;
        }

        image_entries.push((i, name.clone()));

        let better = best_idx.is_none() || (opt.sort && natural_cmp_less(&name, &best_name));
        if better {
            best_idx = Some(i);
            best_name = name;
            if !opt.sort && !is_epub {
                break;
            }
        }
    }

    if !is_epub && opt.sort && opt.prefer_continuous_sequence && image_entries.len() > 1 {
        if let Some(chosen_idx) = pick_best_cover_index(&image_entries, opt.sort, true) {
            best_idx = Some(chosen_idx);
            if let Some((_, chosen_name)) = image_entries.iter().find(|(i, _)| *i == chosen_idx) {
                best_name = chosen_name.clone();
            }
        }
    }

    // EPUB OPF metadata takes priority over alphabetical image order
    let mut from_epub_metadata = false;
    if is_epub && !opfs.is_empty() {
        for (_, opf_content) in &opfs {
            if let Some(href) = crate::epub::parse_opf_cover_href(opf_content) {
                if crate::epub::inspect_opf_href_security(&href).is_some() {
                    has_risk = true;
                }
            }
        }
        if let Some(cover_path) = resolve_epub_cover(&container_xml, &opfs) {
            if let Some((idx, matched_name)) = image_entries
                .iter()
                .find(|(_, n)| n.eq_ignore_ascii_case(&cover_path))
            {
                if !exclude.iter().any(|ex| ex == matched_name) {
                    best_idx = Some(*idx);
                    best_name = matched_name.clone();
                    from_epub_metadata = true;
                }
            }
        }
    }

    let idx = best_idx.ok_or_else(|| "No cover image candidate found in ZIP".to_string())?;
    let entry = zip.by_index(idx).map_err(|e| e.to_string())?;
    let comp_size = entry.compressed_size();
    let data = read_bounded_with_deadline(entry, opt.max_image_bytes, deadline)?;
    if data.is_empty() {
        return Err("Cover entry is empty".into());
    }
    let actual_len = data.len() as u64;
    if actual_len > 8 * 1024 * 1024
        && (comp_size == 0 || (actual_len / comp_size) > MAX_COMPRESSION_RATIO)
    {
        return Err("Extracted ZIP entry violated compression ratio limit (>200:1)".into());
    }

    Ok(CoverCandidate {
        name: best_name,
        data,
        from_epub_metadata,
        is_epub,
        container: ContainerFormat::Zip,
        has_risk: opt.anomaly_detection && has_risk,
    })
}

fn count_entries_zip<R: Read + Seek>(
    reader: &mut R,
    timeout: Duration,
) -> Result<ArchiveStats, String> {
    verify_zip_eocd_tail(reader)?;
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + timeout;
    let is_epub = zip.file_names().take(MAX_SCAN_ENTRIES).any(|n| {
        let norm = n.replace('\\', "/");
        norm.eq_ignore_ascii_case("META-INF/container.xml")
            || norm.to_ascii_lowercase().ends_with(".opf")
    });
    let total_entries = zip.len().min(MAX_SCAN_ENTRIES);
    let mut stats = ArchiveStats {
        images: 0,
        files: 0,
        complete: zip.len() <= MAX_SCAN_ENTRIES,
        has_risk: false,
        risk_reasons: Vec::new(),
    };
    for idx in 0..total_entries {
        if (idx & 0xFF) == 0 && Instant::now() > deadline {
            stats.complete = false;
            break;
        }
        let Ok(file) = zip.by_index_raw(idx) else {
            continue;
        };
        let raw_name_bytes = file.name_raw();
        let decoded_name = crate::encoding::decode_archive_filename(raw_name_bytes);
        let raw_name = decoded_name.as_str();
        if let Some(reason) = inspect_entry_security(raw_name, 0, 0, is_epub) {
            stats.has_risk = true;
            if !stats.risk_reasons.iter().any(|r| r == reason) {
                stats.risk_reasons.push(reason.to_string());
            }
        }
        if raw_name.ends_with('/') || raw_name.ends_with('\\') {
            continue;
        }
        let Some(name) = sanitize_archive_path(raw_name) else {
            continue;
        };
        stats.files += 1;
        if !is_junk_name(&name) && is_image_name(&name) {
            stats.images += 1;
        }
    }
    Ok(stats)
}

// ---------------------------------------------------------------------------
// 7z / CB7 (pure Rust via `sevenz-rust2` crate)

/// Maximum accepted size of the (possibly compressed) 7z "next header" block.
/// Real-world comic archives use a few KiB; 16 MiB covers ~100k entries.
pub const MAX_7Z_NEXT_HEADER_BYTES: u64 = 16 * 1024 * 1024;

/// Validates the fixed 32-byte 7z signature header *before* handing the stream to the parser.
///
/// The parser trusts `NextHeaderOffset` / `NextHeaderSize` and allocates header buffers from
/// them; a forged size (e.g. 0xFFFF_FFFF_FFFF) is an allocation bomb that aborts the host
/// process (Explorer / surrogate) instead of returning an error.
pub(crate) fn verify_7z_signature_header<R: Read + Seek + ?Sized>(reader: &mut R) -> Result<(), String> {
    let file_len = reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut hdr = [0u8; 32];
    let n = read_up_to(reader, &mut hdr).map_err(|e| e.to_string())?;
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    if n < 32 || &hdr[0..6] != b"7z\xBC\xAF\x27\x1C" {
        return Err("Invalid 7z signature header".into());
    }
    let next_off = u64::from_le_bytes(hdr[12..20].try_into().unwrap_or([0; 8]));
    let next_size = u64::from_le_bytes(hdr[20..28].try_into().unwrap_or([0; 8]));
    if next_size == 0 || next_size > MAX_7Z_NEXT_HEADER_BYTES {
        return Err("7z next-header size out of bounds (possible allocation bomb)".into());
    }
    let end = 32u64
        .checked_add(next_off)
        .and_then(|v| v.checked_add(next_size))
        .ok_or_else(|| "7z next-header offset overflow".to_string())?;
    if end > file_len {
        return Err("7z next-header points beyond end of file".into());
    }
    Ok(())
}

fn find_cover_7z<R: Read + Seek>(
    reader: &mut R,
    opt: &CoverOptions,
    exclude: &[String],
) -> Result<CoverCandidate, String> {
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    verify_7z_signature_header(reader)?;

    let mut sz = sevenz_rust2::ArchiveReader::new(reader, sevenz_rust2::Password::empty())
        .map_err(|e| format!("Invalid 7z archive: {e}"))?;

    // First pass over headers in memory to pick the winning entry name
    let mut best_name = String::new();
    let mut image_entries: Vec<(usize, String)> = Vec::new();
    let mut has_risk = false;
    for entry in sz.archive().files.iter().take(MAX_SCAN_ENTRIES) {
        if inspect_entry_security_with_rules(entry.name(), entry.size(), 0, false, Some(&opt.risk_rules)).is_some() {
            has_risk = true;
        }
        if entry.is_directory() || !entry.has_stream() || entry.is_anti_item() {
            continue;
        }
        let Some(name) = sanitize_archive_path(entry.name()) else {
            continue;
        };
        if is_junk_name(&name) || !is_image_name(&name) {
            continue;
        }
        if exclude.iter().any(|ex| ex == &name) {
            continue;
        }
        let size = entry.size();
        if size == 0 || size > opt.max_image_bytes {
            continue;
        }
        image_entries.push((image_entries.len(), name.clone()));
        if best_name.is_empty() || (opt.sort && natural_cmp_less(&name, &best_name)) {
            best_name = name;
            if !opt.sort {
                break;
            }
        }
    }

    if opt.sort && opt.prefer_continuous_sequence && image_entries.len() > 1 {
        if let Some(idx) = pick_best_cover_index(&image_entries, opt.sort, true) {
            best_name = image_entries[idx].1.clone();
        }
    }

    if best_name.is_empty() {
        return Err("No cover image candidate found in 7z archive".into());
    }

    let deadline = Instant::now() + opt.scan_timeout;
    let max_bytes = opt.max_image_bytes;
    let target = best_name.clone();
    let mut extracted: Option<Vec<u8>> = None;
    let mut solid_drained: u64 = 0;

    let _ = sz.for_each_entries(|entry, reader_entry| {
        if Instant::now() > deadline {
            return Ok(false);
        }
        let Some(name) = sanitize_archive_path(entry.name()) else {
            return Ok(false);
        };
        if name == target {
            if let Ok(bytes) = read_bounded_with_deadline(reader_entry, max_bytes, deadline) {
                extracted = Some(bytes);
            }
            return Ok(false); // stop iterating once we got our cover
        }
        // For solid blocks, drain earlier entries with a strict cumulative byte & time budget
        if drain_bounded(reader_entry, &mut solid_drained, MAX_SOLID_DRAIN_BYTES, deadline).is_err()
        {
            return Ok(false);
        }
        Ok(true)
    });

    let data = extracted.ok_or_else(|| "Failed to extract cover from 7z archive".to_string())?;
    Ok(CoverCandidate {
        name: best_name,
        data,
        from_epub_metadata: false,
        is_epub: false,
        container: ContainerFormat::SevenZ,
        has_risk: opt.anomaly_detection && has_risk,
    })
}

fn count_entries_7z<R: Read + Seek>(
    reader: &mut R,
    timeout: Duration,
) -> Result<ArchiveStats, String> {
    let deadline = Instant::now() + timeout;
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    verify_7z_signature_header(reader)?;
    let sz = sevenz_rust2::ArchiveReader::new(reader, sevenz_rust2::Password::empty())
        .map_err(|e| e.to_string())?;
    let files = &sz.archive().files;
    let mut stats = ArchiveStats {
        images: 0,
        files: 0,
        complete: files.len() <= MAX_SCAN_ENTRIES,
        has_risk: false,
        risk_reasons: Vec::new(),
    };
    for (idx, entry) in files.iter().take(MAX_SCAN_ENTRIES).enumerate() {
        if (idx & 0xFF) == 0 && Instant::now() > deadline {
            stats.complete = false;
            break;
        }
        if let Some(reason) = inspect_entry_security(entry.name(), entry.size(), 0, false) {
            stats.has_risk = true;
            if !stats.risk_reasons.iter().any(|r| r == reason) {
                stats.risk_reasons.push(reason.to_string());
            }
        }
        if entry.is_directory() || entry.is_anti_item() {
            continue;
        }
        let Some(name) = sanitize_archive_path(entry.name()) else {
            continue;
        };
        stats.files += 1;
        if !is_junk_name(&name) && is_image_name(&name) {
            stats.images += 1;
        }
    }
    Ok(stats)
}

// ---------------------------------------------------------------------------
// TAR / CBT (pure Rust via `tar` crate)

fn find_cover_tar<R: Read + Seek>(
    reader: &mut R,
    opt: &CoverOptions,
    exclude: &[String],
) -> Result<CoverCandidate, String> {
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut ar = tar::Archive::new(reader);
    let deadline = Instant::now() + opt.scan_timeout;

    let mut best_name = String::new();
    let mut best_data = Vec::new();
    let mut has_risk = false;
    let mut tar_image_entries: Vec<(usize, String)> = Vec::new();

    let entries = ar.entries().map_err(|e| e.to_string())?;
    for (idx, entry_res) in entries.take(MAX_SCAN_ENTRIES).enumerate() {
        if (idx & 0x3F) == 0 && Instant::now() > deadline {
            break;
        }
        let Ok(entry) = entry_res else {
            break;
        };
        let raw_path = entry
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let size = entry.size();
        if inspect_entry_security_with_rules(&raw_path, size, size, false, Some(&opt.risk_rules)).is_some() {
            has_risk = true;
        }
        if !entry.header().entry_type().is_file() {
            continue;
        }
        if size == 0 || size > opt.max_image_bytes {
            continue;
        }
        let Some(name) = sanitize_archive_path(&raw_path) else {
            continue;
        };
        if is_junk_name(&name) || !is_image_name(&name) {
            continue;
        }
        if exclude.iter().any(|ex| ex == &name) {
            continue;
        }
        tar_image_entries.push((tar_image_entries.len(), name.clone()));
        let better = best_name.is_empty() || (opt.sort && natural_cmp_less(&name, &best_name));
        if !better {
            continue;
        }
        if let Ok(data) = read_bounded_with_deadline(entry, opt.max_image_bytes, deadline) {
            best_name = name;
            best_data = data;
            if !opt.sort {
                break;
            }
        }
    }

    if opt.sort && opt.prefer_continuous_sequence && tar_image_entries.len() > 1 {
        if let Some(chosen_ord) = pick_best_cover_index(&tar_image_entries, opt.sort, true) {
            let chosen_name = tar_image_entries[chosen_ord].1.clone();
            if chosen_name != best_name && Instant::now() < deadline {
                let reader_ref = ar.into_inner();
                if reader_ref.seek(SeekFrom::Start(0)).is_ok() {
                    let mut ar2 = tar::Archive::new(reader_ref);
                    if let Ok(entries2) = ar2.entries() {
                        for (idx2, entry_res2) in entries2.take(MAX_SCAN_ENTRIES).enumerate() {
                            if (idx2 & 0x3F) == 0 && Instant::now() > deadline {
                                break;
                            }
                            let Ok(entry2) = entry_res2 else { break; };
                            if !entry2.header().entry_type().is_file() {
                                continue;
                            }
                            let raw_path2 = entry2
                                .path()
                                .map(|p| p.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            if let Some(name2) = sanitize_archive_path(&raw_path2) {
                                if name2 == chosen_name {
                                    if let Ok(data2) = read_bounded_with_deadline(entry2, opt.max_image_bytes, deadline) {
                                        best_name = name2;
                                        best_data = data2;
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if best_name.is_empty() {
        Err("No cover image candidate found in TAR archive".into())
    } else {
        Ok(CoverCandidate {
            name: best_name,
            data: best_data,
            from_epub_metadata: false,
            is_epub: false,
            container: ContainerFormat::Tar,
            has_risk: opt.anomaly_detection && has_risk,
        })
    }
}

fn count_entries_tar<R: Read + Seek>(
    reader: &mut R,
    timeout: Duration,
) -> Result<ArchiveStats, String> {
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut ar = tar::Archive::new(reader);
    let deadline = Instant::now() + timeout;
    let mut stats = ArchiveStats {
        images: 0,
        files: 0,
        complete: true,
        has_risk: false,
        risk_reasons: Vec::new(),
    };
    let entries = ar.entries().map_err(|e| e.to_string())?;
    for (idx, entry_res) in entries.enumerate() {
        if idx >= MAX_SCAN_ENTRIES || Instant::now() > deadline {
            stats.complete = false;
            break;
        }
        let Ok(entry) = entry_res else {
            stats.complete = false;
            break;
        };
        let raw_path = entry
            .path()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        let size = entry.size();
        if inspect_entry_security(&raw_path, size, size, false).is_some() {
            stats.has_risk = true;
        }
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let Some(name) = sanitize_archive_path(&raw_path) else {
            continue;
        };
        stats.files += 1;
        if !is_junk_name(&name) && is_image_name(&name) {
            stats.images += 1;
        }
    }
    Ok(stats)
}
