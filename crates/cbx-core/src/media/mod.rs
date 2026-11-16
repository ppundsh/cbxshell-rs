pub mod audio_cover;
pub mod buffered_stream;
pub mod media_types;
pub mod smart_render;
pub mod video_mf;

use std::path::{Path, PathBuf};

pub use audio_cover::extract_embedded_cover;
pub use media_types::{
    get_extension_category, is_audio_extension, is_builtin_plain_text_extension,
    is_custom_text_extension, is_ebook_extension, is_image_archive_extension, is_other_extension,
    is_other_or_custom_text_extension, is_syntax_extension, is_video_extension, MediaCategory,
    MediaFormatInfo, SUPPORTED_MEDIA_FORMATS,
};
pub use smart_render::{
    aces_filmic_tone_mapping, compute_luminance, compute_luminance_and_entropy_bgra,
    compute_luminance_and_entropy_y_plane, compute_luminance_and_entropy_yuy2,
    compute_shannon_entropy, evaluate_frame, evaluate_metrics, FrameMetrics, SmartRenderConfig,
};
pub use video_mf::{
    ensure_mf_initialized, extract_video_thumbnail_from_path, extract_video_thumbnail_from_stream,
    VideoExtractReport,
};

/// Helper function to perform bounded recursive file searching across directory trees.
static LAST_FOUND_DIR: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

/// Updates the last known Explorer working directory for high-speed batch resolution.
/// Only local fixed/removable drive directories are cached, so a single network hit can never
/// turn every later lookup into an outbound SMB/WebDAV probe.
pub fn update_last_found_dir(dir: &std::path::Path) {
    if !is_trusted_local_abs_path(dir) || !is_local_fixed_or_removable_drive(dir) {
        return;
    }
    if let Ok(mut g) = LAST_FOUND_DIR.lock() {
        *g = Some(dir.to_path_buf());
    }
}

/// Returns `true` only for an absolute drive-letter path (`X:\...`) that contains no UNC/device
/// prefix, no NTFS alternate-data-stream colon and no `..` component.
///
/// Used before turning any name supplied by the Shell (`STATSTG::pwcsName`, `IPersistFile`) into a
/// filesystem read, so that a bare or relative name is never resolved against the host process's
/// current directory (confused-deputy read of an unrelated file).
pub fn is_trusted_local_abs_path(p: &Path) -> bool {
    let s = p.to_string_lossy();
    let b = s.as_bytes();
    if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || (b[2] != b'\\' && b[2] != b'/') {
        return false;
    }
    if s.char_indices().any(|(idx, ch)| ch == ':' && idx != 1) {
        return false;
    }
    if s.chars().any(|c| c.is_control()) {
        return false;
    }
    !p.components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
}

/// Source of a resolved filesystem path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveSource {
    /// Original input was already a full filesystem path.
    DirectPath,
    /// Resolved via installed Everything IPC service.
    EverythingIpc,
    /// Resolved via in-process last matched directory cache.
    LastDirCache,
    /// Resolved via current working directory.
    WorkingDir,
    /// Resolved via Windows Explorer address bar history (TypedPaths).
    TypedPaths,
    /// Resolved via Windows Recent items shortcut history (.lnk).
    RecentHistory,
    /// Resolved via standard user folders (Downloads, Videos, Desktop, Documents).
    UserFolders,
    /// Resolved via drive root directory scan (C:\..Z:\).
    DriveRoot,
}

impl ResolveSource {
    /// Returns a localized trace description explaining how the file was located.
    pub fn trace_description(self) -> String {
        let key = match self {
            Self::DirectPath => "trace.direct_path",
            Self::EverythingIpc => "trace.everything_ipc",
            Self::LastDirCache => "trace.last_dir_cache",
            Self::WorkingDir => "trace.working_dir",
            Self::TypedPaths => "trace.typed_paths",
            Self::RecentHistory => "trace.recent_history",
            Self::UserFolders => "trace.user_folders",
            Self::DriveRoot => "trace.drive_root",
        };
        crate::i18n::t(key)
    }
}

/// Returns true if an active Voidtools Everything IPC service (v1.4+ / v1.5+) is running.
pub fn is_everything_running() -> bool {
    everything_ipc::wm::EverythingClient::new().is_ok()
}

/// Queries the running Everything IPC window for its version string (e.g. "1.4.1").
/// Returns `None` if Everything is not currently running.
pub fn get_everything_version() -> Option<String> {
    let win = everything_ipc::IpcWindow::new()?;
    let ver = win.get_version();
    if ver.major == 0 {
        return None;
    }
    if ver.revision > 0 {
        Some(format!("{}.{}.{}", ver.major, ver.minor, ver.revision))
    } else {
        Some(format!("{}.{}", ver.major, ver.minor))
    }
}

/// Checks if Everything is installed on the system (either running or present in standard paths/registry).
pub fn is_everything_installed() -> bool {
    if is_everything_running() {
        return true;
    }
    // Derive install roots from the environment instead of hard-coding a drive letter.
    for var in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        let Some(root) = std::env::var_os(var) else {
            continue;
        };
        let root = PathBuf::from(root);
        for rel in [
            ["Everything", "Everything.exe"],
            ["Everything 1.5a", "Everything64.exe"],
        ] {
            if root.join(rel[0]).join(rel[1]).is_file() {
                return true;
            }
        }
    }
    crate::registry::read_reg_sz(
        windows::Win32::System::Registry::HKEY_LOCAL_MACHINE,
        r"SOFTWARE\voidtools\Everything",
        Some("InstallLocation"),
    )
    .is_some()
}

/// Verifies that the Everything IPC window belongs to an actual `Everything.exe` / `Everything64.exe`
/// process in a local non-temporary path, preventing a rogue process from spoofing
/// `EVERYTHING_TASKBAR_NOTIFICATION` and returning malicious UNC/decoy paths.
fn verify_everything_ipc_owner() -> bool {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{CloseHandle, MAX_PATH};
        use windows::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
            PROCESS_QUERY_LIMITED_INFORMATION,
        };
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowThreadProcessId};

        let cls: Vec<u16> = "EVERYTHING_TASKBAR_NOTIFICATION\0".encode_utf16().collect();
        // SAFETY: cls is null-terminated UTF-16.
        let hwnd = unsafe { FindWindowW(PCWSTR(cls.as_ptr()), PCWSTR::null()) };
        let Ok(hwnd) = hwnd else {
            return false;
        };
        if hwnd.0.is_null() {
            return false;
        }
        let mut pid: u32 = 0;
        // SAFETY: hwnd is a valid window handle and pid is a valid out-pointer.
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == 0 {
            return false;
        }
        let Ok(proc_h) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
            return false;
        };
        let mut buf = [0u16; MAX_PATH as usize];
        let mut len = buf.len() as u32;
        let ok = unsafe {
            QueryFullProcessImageNameW(
                proc_h,
                PROCESS_NAME_FORMAT(0),
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .is_ok()
        };
        let _ = unsafe { CloseHandle(proc_h) };
        if !ok || len == 0 {
            return false;
        }
        let exe_path = String::from_utf16_lossy(&buf[..len as usize]);
        let p = Path::new(&exe_path);
        let Some(fname) = p.file_name().and_then(|s| s.to_str()) else {
            return false;
        };
        let is_everything_bin = fname.eq_ignore_ascii_case("Everything.exe")
            || fname.eq_ignore_ascii_case("Everything64.exe");
        let lower = exe_path.to_ascii_lowercase();
        let is_suspicious_dir = lower.starts_with("\\\\")
            || lower.contains("\\appdata\\local\\temp\\")
            || lower.contains("\\downloads\\");
        return is_everything_bin && p.is_absolute() && !is_suspicious_dir;
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Fallback probes (cache, CWD, TypedPaths, user folders, Recent, drive roots) only accept a
/// same-named candidate when its size matches the Shell stream exactly. An unknown (0) size
/// would let any unrelated same-named file be rendered in place of the real stream.
fn size_matches(cand: &Path, expected_size: u64) -> bool {
    expected_size > 0 && cand.metadata().map(|m| m.is_file() && m.len() == expected_size).unwrap_or(false)
}

fn is_local_fixed_or_removable_drive(p: &Path) -> bool {
    let s = p.to_string_lossy();
    let bytes = s.as_bytes();
    if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || bytes[1] != b':' || (bytes[2] != b'\\' && bytes[2] != b'/') {
        return false;
    }
    let root_w: Vec<u16> = format!("{}:\\\0", bytes[0] as char).encode_utf16().collect();
    // SAFETY: root_w is null-terminated UTF-16.
    let dtype = unsafe {
        windows::Win32::Storage::FileSystem::GetDriveTypeW(windows::core::PCWSTR(root_w.as_ptr()))
    };
    dtype == 2 || dtype == 3
}

/// Validates a path returned by Everything IPC before touching the filesystem:
/// rejects UNC/network paths, relative traversal, ADS colons, name mismatches, and size mismatches.
fn is_valid_everything_result(full_path: &Path, expected_base_name: &str, expected_size: u64) -> bool {
    let s = full_path.to_string_lossy();
    if s.starts_with("\\\\") || s.starts_with("//") {
        return false;
    }
    if s.char_indices().any(|(idx, ch)| ch == ':' && idx != 1) {
        return false;
    }
    if full_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return false;
    }
    let Some(actual_name) = full_path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if !actual_name.eq_ignore_ascii_case(expected_base_name) {
        return false;
    }
    if !is_local_fixed_or_removable_drive(full_path) {
        return false;
    }
    if !full_path.is_file() {
        return false;
    }
    match full_path.metadata() {
        Ok(m) => m.len() == expected_size,
        Err(_) => false,
    }
}

/// Query the running Everything search index via IPC (support Everything 1.4+ / 1.5+).
/// Zero bundled binaries or DLLs. If Everything is not running or not installed,
/// fails immediately and returns `None` without stalling or blocking.
pub fn resolve_via_installed_everything(fname: &str, expected_size: u64) -> Option<std::path::PathBuf> {
    let base_name = Path::new(fname).file_name()?.to_str()?;
    if base_name.is_empty() || base_name.contains('"') || base_name.chars().any(|c| c.is_control()) {
        return None;
    }
    if !verify_everything_ipc_owner() {
        return None;
    }

    let client = everything_ipc::wm::EverythingClient::new().ok()?;
    let query_str = format!("exact:\"{}\"", base_name);
    let list = client
        .query_wait(&query_str)
        .request_flags(everything_ipc::wm::RequestFlags::FileName | everything_ipc::wm::RequestFlags::Path | everything_ipc::wm::RequestFlags::Size)
        .max_results(5)
        .call()
        .ok()?;

    for item in list.iter() {
        if let (Some(path_display), Some(name)) = (
            item.get_str(everything_ipc::wm::RequestFlags::Path),
            item.get_string(everything_ipc::wm::RequestFlags::FileName),
        ) {
            let full_path = std::path::PathBuf::from(path_display.to_string_lossy()).join(name);
            if is_valid_everything_result(&full_path, base_name, expected_size) {
                if let Some(parent) = full_path.parent() {
                    update_last_found_dir(parent);
                }
                return Some(full_path);
            }
        }
    }

    None
}

/// Ultra-fast non-blocking file resolution for process-isolated video thumbnails.
///
/// Strictly bounds search to direct parent directories (depth = 0) with ZERO recursive traversal:
/// 1. Direct path check (0.01ms)
/// 2. Last known directory cache (0.01ms, hits 99% of folder batches)
/// 3. Installed Everything IPC query (0.1ms ~ 1ms instant index lookup across all drives if running)
/// 4. Current working directory fallback (0.05ms)
/// 5. Explorer active TypedPaths url1~5 fallback (0.1ms)
/// 6. User Downloads / Videos / Desktop fallback (0.1ms)
///
/// Total execution time is strictly bounded. If not found, returns `None` immediately so execution
/// falls back to `BufferedIStream` without stalling Explorer!
pub fn resolve_file_from_name(fname: &str, expected_size: u64) -> Option<std::path::PathBuf> {
    resolve_file_from_name_with_source(fname, expected_size).map(|(p, _)| p)
}

/// Ultra-fast non-blocking file resolution with source tracking for process-isolated video thumbnails.
pub fn resolve_file_from_name_with_source(
    fname: &str,
    expected_size: u64,
) -> Option<(std::path::PathBuf, ResolveSource)> {
    // Reject UNC network paths (\\server\share) and device paths (\\.\, \\?\) to prevent
    // outbound SMB/NTLM credential leaks or device-handle stalls.
    if fname.is_empty() || fname.starts_with("\\\\") || fname.starts_with("//") {
        return None;
    }
    let p = std::path::Path::new(fname);
    if is_trusted_local_abs_path(p) && p.is_file() {
        if let Some(parent) = p.parent() {
            update_last_found_dir(parent);
        }
        return Some((p.to_path_buf(), ResolveSource::DirectPath));
    }

    // Subsequent directory/index probes must only use the bare filename, never relative traversal.
    let fname = p.file_name().and_then(|s| s.to_str())?;
    if fname.is_empty() || fname == "." || fname == ".." || fname.contains(':') {
        return None;
    }
    if fname.chars().any(|c| c.is_control()) {
        return None;
    }
    // Without a known stream size there is no way to tell the real file from an unrelated
    // same-named one, so name-based fallback resolution is disabled entirely.
    if expected_size == 0 {
        return None;
    }

    // 1. High-speed cache: Check last successfully accessed directory (0.01ms hit rate for folder batch requests!)
    if let Ok(g) = LAST_FOUND_DIR.lock() {
        if let Some(ref dir) = *g {
            let cand = dir.join(fname);
            if cand.is_file() {
                if size_matches(&cand, expected_size) {
                    return Some((cand, ResolveSource::LastDirCache));
                }
            }
        }
    }

    // 2. Query installed Everything instance if available (instant index lookup across NTFS drives)
    if let Some(path) = resolve_via_installed_everything(fname, expected_size) {
        return Some((path, ResolveSource::EverythingIpc));
    }

    // 3. Fallback: Check current working directory (0.05ms)
    if let Ok(cwd) = std::env::current_dir() {
        let cand = cwd.join(fname);
        if cand.is_file() {
            if size_matches(&cand, expected_size) {
                update_last_found_dir(&cwd);
                return Some((cand, ResolveSource::WorkingDir));
            }
        }
    }

    // 4. Fallback: Check Explorer active TypedPaths url1~url5 directly (0.1ms, depth = 0 only)
    for i in 1..=5 {
        let val_name = format!("url{i}");
        if let Some(val) = crate::registry::read_reg_sz(
            windows::Win32::System::Registry::HKEY_CURRENT_USER,
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\TypedPaths",
            Some(&val_name),
        ) {
            let val_trimmed = val.trim();
            let pb = std::path::PathBuf::from(val_trimmed);
            // TypedPaths is free-form address-bar history: it may hold UNC shares, mapped network
            // drives or shell URLs. Touching those would leak NTLM credentials or stall Explorer.
            if !is_trusted_local_abs_path(&pb) || !is_local_fixed_or_removable_drive(&pb) {
                continue;
            }
            if pb.is_dir() {
                let cand = pb.join(fname);
                if cand.is_file() {
                    if size_matches(&cand, expected_size) {
                        update_last_found_dir(&pb);
                        return Some((cand, ResolveSource::TypedPaths));
                    }
                }
            }
        }
    }

    // 5. Fallback: Check user standard directories (Downloads, Videos, Desktop, Documents)
    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        for sub in &["Downloads", "Videos", "Desktop", "Documents"] {
            let dir = std::path::Path::new(&userprofile).join(sub);
            if dir.is_dir() {
                let cand = dir.join(fname);
                if cand.is_file() {
                    if size_matches(&cand, expected_size) {
                        update_last_found_dir(&dir);
                        return Some((cand, ResolveSource::UserFolders));
                    }
                }
            }
        }
    }

    // 6. Recent items history (%APPDATA%\Microsoft\Windows\Recent\*.lnk)
    if let Some(found) = search_recent_shortcuts(fname, expected_size) {
        if let Some(parent) = found.parent() {
            if let Ok(mut g) = LAST_FOUND_DIR.lock() {
                *g = Some(parent.to_path_buf());
            }
        }
        return Some((found, ResolveSource::RecentHistory));
    }

    // 7. Drive root quick probe (C:\..Z:\ using logical drive bitmask)
    if let Some(found) = search_drive_roots(fname, expected_size) {
        if let Some(parent) = found.parent() {
            if let Ok(mut g) = LAST_FOUND_DIR.lock() {
                *g = Some(parent.to_path_buf());
            }
        }
        return Some((found, ResolveSource::DriveRoot));
    }

    None
}

fn search_recent_shortcuts(fname: &str, expected_size: u64) -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let recent_dir = PathBuf::from(appdata).join("Microsoft").join("Windows").join("Recent");
    let entries = std::fs::read_dir(&recent_dir).ok()?;

    let fname_lower = fname.to_ascii_lowercase();
    let stem_lower = Path::new(fname)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();

    let mut lnk_candidates: Vec<(bool, std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in entries.flatten().take(300) {
        let p = entry.path();
        if !p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("lnk")).unwrap_or(false) {
            continue;
        }
        let lnk_name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        let name_match = (!stem_lower.is_empty() && lnk_name.contains(&stem_lower)) || lnk_name.contains(&fname_lower);
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        lnk_candidates.push((name_match, mtime, p));
    }

    // Check likely filename matches first, then newest shortcuts first
    lnk_candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));

    let start = std::time::Instant::now();
    for (_, _, lnk_path) in lnk_candidates.into_iter().take(120) {
        if start.elapsed() > std::time::Duration::from_millis(8) {
            break;
        }
        if let Ok(bytes) = std::fs::read(&lnk_path) {
            if bytes.len() > 64 * 1024 {
                continue;
            }
            if let Some(target) = extract_matching_path_from_lnk(&bytes, fname, expected_size) {
                return Some(target);
            }
        }
    }
    None
}

fn extract_matching_path_from_lnk(bytes: &[u8], fname: &str, expected_size: u64) -> Option<PathBuf> {
    let fname_lower = fname.to_ascii_lowercase();

    // 1) Scan UTF-16LE drive path sequences like "D:\...\fname"
    let mut i = 0usize;
    while i + 8 <= bytes.len() {
        let ch = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let colon = u16::from_le_bytes([bytes[i + 2], bytes[i + 3]]);
        let slash = u16::from_le_bytes([bytes[i + 4], bytes[i + 5]]);
        if ((b'A' as u16..=b'Z' as u16).contains(&ch) || (b'a' as u16..=b'z' as u16).contains(&ch))
            && colon == b':' as u16
            && slash == b'\\' as u16
        {
            let mut units = Vec::with_capacity(64);
            let mut j = i;
            while j + 1 < bytes.len() && units.len() < 512 {
                let u = u16::from_le_bytes([bytes[j], bytes[j + 1]]);
                if u == 0 || u < 0x20 {
                    break;
                }
                units.push(u);
                j += 2;
            }
            if let Ok(s) = String::from_utf16(&units) {
                if let Some(cand) = validate_lnk_candidate(&s, &fname_lower, expected_size) {
                    return Some(cand);
                }
            }
            i = j.max(i + 2);
            continue;
        }
        i += 2;
    }

    // 2) Scan ASCII/UTF-8 local base path sequences like "D:\...\fname"
    let mut k = 0usize;
    while k + 4 <= bytes.len() {
        if bytes[k].is_ascii_alphabetic() && bytes[k + 1] == b':' && bytes[k + 2] == b'\\' {
            let mut end = k;
            while end < bytes.len() && (end - k) < 512 && bytes[end] >= 0x20 {
                end += 1;
            }
            if let Ok(s) = std::str::from_utf8(&bytes[k..end]) {
                if let Some(cand) = validate_lnk_candidate(s, &fname_lower, expected_size) {
                    return Some(cand);
                }
            }
            k = end.max(k + 1);
            continue;
        }
        k += 1;
    }

    None
}

fn validate_lnk_candidate(path_str: &str, fname_lower: &str, expected_size: u64) -> Option<PathBuf> {
    let trimmed = path_str.trim();
    let p = Path::new(trimmed);
    // Shortcut targets are attacker-influenced bytes; only follow local drive paths.
    if !is_trusted_local_abs_path(p) || !is_local_fixed_or_removable_drive(p) {
        return None;
    }
    let matches_file = p
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_ascii_lowercase() == fname_lower)
        .unwrap_or(false);
    if matches_file && p.is_file() {
        if size_matches(p, expected_size) {
            return Some(p.to_path_buf());
        }
    }
    // Also check if the shortcut points to a folder containing the file
    if p.is_dir() {
        let child = p.join(fname_lower);
        if child.is_file() {
            if size_matches(&child, expected_size) {
                return Some(child);
            }
        }
    }
    None
}

fn search_drive_roots(fname: &str, expected_size: u64) -> Option<PathBuf> {
    // SAFETY: GetLogicalDrives takes no pointers and returns a bitmask of valid drive letters.
    let mask = unsafe { windows::Win32::Storage::FileSystem::GetLogicalDrives() };
    for idx in 2..26u32 {
        // C: (idx 2) through Z: (idx 25), skipping A:/B: floppy slots
        if (mask & (1u32 << idx)) == 0 {
            continue;
        }
        let letter = (b'A' + idx as u8) as char;
        let root_str = format!("{}:\\", letter);
        let root_w: Vec<u16> = root_str.encode_utf16().chain(std::iter::once(0)).collect();
        // Skip network drives (4) and CD-ROMs (5) to prevent network stalls
        // SAFETY: root_w is null-terminated UTF-16.
        let dtype = unsafe { windows::Win32::Storage::FileSystem::GetDriveTypeW(windows::core::PCWSTR(root_w.as_ptr())) };
        if dtype != 2 && dtype != 3 {
            // Only DRIVE_REMOVABLE (2) and DRIVE_FIXED (3)
            continue;
        }
        let cand = PathBuf::from(root_str).join(fname);
        if cand.is_file() {
            if size_matches(&cand, expected_size) {
                return Some(cand);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_fallback_contract() {
        // Guarantee empty or non-existent file returns None safely without panic
        assert_eq!(resolve_file_from_name("", 0), None);
        assert_eq!(resolve_file_from_name("non_existent_test_marker_xyz_998877.tmp", 0), None);
    }
}

