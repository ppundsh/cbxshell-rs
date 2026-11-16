//! CJK and multi-byte encoding detection for archive filenames (CP932/Shift-JIS, Big5, GBK, UTF-8).

#[link(name = "kernel32")]
extern "system" {
    fn MultiByteToWideChar(
        CodePage: u32,
        dwFlags: u32,
        lpMultiByteStr: *const u8,
        cbMultiByte: i32,
        lpWideCharStr: *mut u16,
        cchWideChar: i32,
    ) -> i32;
    fn GetACP() -> u32;
}

const MB_ERR_INVALID_CHARS: u32 = 0x00000008;

/// Decodes raw archive entry filename bytes into a clean, Mojibake-free Rust `String`.
/// Prioritizes UTF-8, then CP932 (Shift-JIS / Japanese DLsite & Comic releases),
/// then system ACP (e.g. CP950 / CP936), then fallback lossy.
pub fn decode_archive_filename(raw: &[u8]) -> String {
    if raw.is_empty() {
        return String::new();
    }

    // 1. If it's valid UTF-8, prefer UTF-8
    if let Ok(s) = std::str::from_utf8(raw) {
        return s.to_string();
    }

    // 2. Not valid UTF-8: Try CJK legacy code pages.
    // In comic, manga, and doujin archives (e.g. RJ numbers from DLsite),
    // non-UTF8 archives are overwhelmingly CP932 (Shift-JIS).
    let sys_acp = unsafe { GetACP() };
    let candidate_codepages = [932, sys_acp, 950, 936];

    for cp in candidate_codepages {
        if cp == 0 || cp == 65001 {
            continue;
        }
        if let Some(decoded) = decode_with_codepage(raw, cp) {
            return decoded;
        }
    }

    // 3. Lossy fallback
    String::from_utf8_lossy(raw).into_owned()
}

fn decode_with_codepage(raw: &[u8], cp: u32) -> Option<String> {
    unsafe {
        let needed = MultiByteToWideChar(
            cp,
            MB_ERR_INVALID_CHARS,
            raw.as_ptr(),
            raw.len() as i32,
            std::ptr::null_mut(),
            0,
        );
        if needed <= 0 {
            return None;
        }
        let mut buf = vec![0u16; needed as usize];
        let res = MultiByteToWideChar(
            cp,
            MB_ERR_INVALID_CHARS,
            raw.as_ptr(),
            raw.len() as i32,
            buf.as_mut_ptr(),
            needed,
        );
        if res > 0 {
            Some(String::from_utf16_lossy(&buf))
        } else {
            None
        }
    }
}
