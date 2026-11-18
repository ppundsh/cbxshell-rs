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

// ---------------------------------------------------------------------------------------------
// Network share policy
//
// By default only local drives are ever opened by path. Users may opt in to network shares
// (UNC `\\server\share\...` and mapped network drives); by default that opt-in is further limited
// to servers on the local network. Classification never performs DNS lookups, so it cannot stall
// Explorer or leak names to a resolver.

/// Network share access policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkSharePolicy {
    /// Never open network paths (default).
    Off,
    /// Only shares on local-network servers.
    LanOnly,
    /// Any reachable share.
    Any,
}

impl NetworkSharePolicy {
    pub fn from_settings(allow: bool, lan_only: bool) -> Self {
        match (allow, lan_only) {
            (false, _) => Self::Off,
            (true, true) => Self::LanOnly,
            (true, false) => Self::Any,
        }
    }
}

static NETWORK_POLICY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Sets the process-wide network share policy (called by the shell extension from settings).
pub fn set_network_share_policy(policy: NetworkSharePolicy) {
    let v = match policy {
        NetworkSharePolicy::Off => 0,
        NetworkSharePolicy::LanOnly => 1,
        NetworkSharePolicy::Any => 2,
    };
    NETWORK_POLICY.store(v, std::sync::atomic::Ordering::Release);
}

pub fn network_share_policy() -> NetworkSharePolicy {
    match NETWORK_POLICY.load(std::sync::atomic::Ordering::Acquire) {
        1 => NetworkSharePolicy::LanOnly,
        2 => NetworkSharePolicy::Any,
        _ => NetworkSharePolicy::Off,
    }
}

/// `true` when `host` names a server on the local network, judged from the name alone:
/// private / link-local / loopback IP literals, single-label (NetBIOS-style) names and
/// conventional private suffixes. Public IPs and other dotted names are treated as remote.
pub fn is_lan_host(host: &str) -> bool {
    let h = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if h.is_empty() || h.len() > 253 || h.contains(['@', '%', '/', '\\', ':', ' ']) {
        // '@' marks WebDAV (\\host@SSL\...), which is always an HTTP(S) server.
        return false;
    }
    if let Ok(v4) = h.parse::<std::net::Ipv4Addr>() {
        return v4.is_private() || v4.is_link_local() || v4.is_loopback();
    }
    // IPv6 literals appear in UNC paths as "fe80--1.ipv6-literal.net".
    if let Some(prefix) = h.strip_suffix(".ipv6-literal.net") {
        let addr = prefix.split('s').next().unwrap_or("").replace('-', ":");
        return match addr.parse::<std::net::Ipv6Addr>() {
            Ok(v6) => {
                let seg0 = v6.segments()[0];
                v6.is_loopback() || (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80
            }
            Err(_) => false,
        };
    }
    if !h.contains('.') {
        // Single-label names (NAS, PC names) are resolved by LLMNR / NetBIOS / mDNS on the LAN.
        return h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    }
    const LAN_SUFFIXES: &[&str] = &[".local", ".lan", ".home", ".home.arpa", ".internal", ".localdomain", ".intranet", ".corp", ".private"];
    LAN_SUFFIXES.iter().any(|suf| h.ends_with(suf))
}

/// Server name of a UNC path (`\\server\share\...`), rejecting device / long-path forms.
fn unc_server(s: &str) -> Option<&str> {
    let rest = s.strip_prefix("\\\\").or_else(|| s.strip_prefix("//"))?;
    if rest.starts_with('?') || rest.starts_with('.') {
        return None; // \\?\ and \\.\ device namespaces
    }
    let server = rest.split(['\\', '/']).next()?;
    let share = rest.split(['\\', '/']).nth(1)?;
    if server.is_empty() || share.is_empty() {
        return None;
    }
    Some(server)
}

/// Pure policy check for a network media path (UNC). Mapped drives are handled by
/// [`is_allowed_network_path`], which needs the OS to find their server.
pub fn is_allowed_network_path_with(p: &Path, policy: NetworkSharePolicy) -> bool {
    if policy == NetworkSharePolicy::Off {
        return false;
    }
    let s = p.to_string_lossy();
    if s.contains(':') || s.chars().any(|c| c.is_control()) {
        return false; // no drive-relative tricks or NTFS alternate data streams
    }
    if p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return false;
    }
    let Some(server) = unc_server(&s) else {
        return false;
    };
    policy == NetworkSharePolicy::Any || is_lan_host(server)
}

/// Server behind a mapped network drive letter (`Z:`), from the redirector's DOS device target.
fn mapped_drive_server(letter: char) -> Option<String> {
    let name: Vec<u16> = format!("{letter}:\0").encode_utf16().collect();
    let mut buf = vec![0u16; 1024];
    // SAFETY: valid null-terminated device name and a writable buffer.
    let n = unsafe { windows::Win32::Storage::FileSystem::QueryDosDeviceW(windows::core::PCWSTR(name.as_ptr()), Some(&mut buf)) };
    if n == 0 {
        return None;
    }
    let target = String::from_utf16_lossy(&buf[..(n as usize).min(buf.len())]);
    let target = target.split('\0').next().unwrap_or("");
    // e.g. \Device\LanmanRedirector\;Z:000000000001a2b3\server\share  or  \Device\Mup\;Z:...\server\share
    // Newer builds nest the provider: \Device\Mup\;LanmanRedirector\;Z:000...\server\share,
    // so take the part after the *last* ';'.
    let after = target.rsplit_once(';').map(|(_, r)| r)?;
    let mut parts = after.split('\\');
    let _drive_session = parts.next()?;
    let server = parts.next()?;
    (!server.is_empty()).then(|| server.to_string())
}

/// `true` when `p` is on a network share the current policy allows (UNC path, or a mapped
/// network drive whose server passes the policy).
pub fn is_allowed_network_path(p: &Path) -> bool {
    let policy = network_share_policy();
    if policy == NetworkSharePolicy::Off {
        return false;
    }
    let s = p.to_string_lossy();
    if s.starts_with("\\\\") || s.starts_with("//") {
        return is_allowed_network_path_with(p, policy);
    }
    let b = s.as_bytes();
    if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' || (b[2] != b'\\' && b[2] != b'/') {
        return false;
    }
    if !is_trusted_local_abs_path(p) {
        return false;
    }
    let root_w: Vec<u16> = format!("{}:\\\0", b[0] as char).encode_utf16().collect();
    // SAFETY: root_w is null-terminated UTF-16.
    let dtype = unsafe { windows::Win32::Storage::FileSystem::GetDriveTypeW(windows::core::PCWSTR(root_w.as_ptr())) };
    if dtype != 4 {
        return false; // only DRIVE_REMOTE counts as a network drive
    }
    if policy == NetworkSharePolicy::Any {
        return true;
    }
    mapped_drive_server(b[0] as char).is_some_and(|srv| is_lan_host(&srv))
}

thread_local! {
    /// Why the last name resolution on this thread ignored a network location (for the monitor).
    static RESOLVE_NOTE: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

fn set_resolve_note(note: String) {
    RESOLVE_NOTE.with(|n| *n.borrow_mut() = note);
}

/// Takes the reason the last resolution on this thread skipped a network path (empty if none).
pub fn take_resolve_note() -> String {
    RESOLVE_NOTE.with(|n| std::mem::take(&mut *n.borrow_mut()))
}

/// Server of a network path: the UNC host, or the server behind a mapped network drive.
pub fn network_path_server(p: &Path) -> Option<String> {
    let s = p.to_string_lossy();
    if s.starts_with("\\\\") || s.starts_with("//") {
        return unc_server(&s).map(str::to_string);
    }
    let b = s.as_bytes();
    if is_remote_drive_path(p) {
        return mapped_drive_server(b[0] as char);
    }
    None
}

/// `true` when `p` points at a network location (UNC path or mapped network drive).
pub fn is_network_path(p: &Path) -> bool {
    let s = p.to_string_lossy();
    s.starts_with("\\\\") || s.starts_with("//") || is_remote_drive_path(p)
}

/// Records why a network candidate was not used under the current policy.
fn note_network_rejection(p: &Path) {
    let note = match network_share_policy() {
        NetworkSharePolicy::Off => crate::i18n::t("trace.network_policy_off"),
        _ => crate::i18n::t_fmt(
            "trace.network_not_lan",
            &[("server", &network_path_server(p).unwrap_or_else(|| "?".to_string()))],
        ),
    };
    set_resolve_note(note);
}

/// `true` for a drive-letter path on a mapped network drive (DRIVE_REMOTE).
fn is_remote_drive_path(p: &Path) -> bool {
    let s = p.to_string_lossy();
    let b = s.as_bytes();
    if b.len() < 3 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return false;
    }
    let root_w: Vec<u16> = format!("{}:\\\0", b[0] as char).encode_utf16().collect();
    // SAFETY: root_w is null-terminated UTF-16.
    unsafe { windows::Win32::Storage::FileSystem::GetDriveTypeW(windows::core::PCWSTR(root_w.as_ptr())) == 4 }
}

/// Path the shell extension may open directly: a trusted local path on a non-network drive, or
/// a network path (UNC or mapped drive) allowed by the current share policy.
pub fn is_openable_media_path(p: &Path) -> bool {
    (is_trusted_local_abs_path(p) && !is_remote_drive_path(p)) || is_allowed_network_path(p)
}

/// Helper function to perform bounded recursive file searching across directory trees.
static LAST_FOUND_DIR: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

/// Updates the last known Explorer working directory for high-speed batch resolution.
/// Only local fixed/removable drive directories are cached, so a single network hit can never
/// turn every later lookup into an outbound SMB/WebDAV probe.
pub fn update_last_found_dir(dir: &std::path::Path) {
    let local_ok = is_trusted_local_abs_path(dir) && is_local_fixed_or_removable_drive(dir);
    if !local_ok && !is_allowed_network_path(dir) {
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
/// Servers that recently failed with "network path not found" (and similar): skipped for a while
/// so a stale Everything index entry does not cost an SMB timeout on every thumbnail.
static UNREACHABLE_HOSTS: std::sync::Mutex<Vec<(String, std::time::Instant)>> = std::sync::Mutex::new(Vec::new());
const UNREACHABLE_HOST_TTL: std::time::Duration = std::time::Duration::from_secs(300);

fn host_recently_unreachable(host: &str) -> bool {
    UNREACHABLE_HOSTS.lock().is_ok_and(|mut v| {
        v.retain(|(_, t)| t.elapsed() < UNREACHABLE_HOST_TTL);
        v.iter().any(|(h, _)| h.eq_ignore_ascii_case(host))
    })
}

fn mark_host_unreachable(host: &str) {
    if let Ok(mut v) = UNREACHABLE_HOSTS.lock() {
        if !v.iter().any(|(h, _)| h.eq_ignore_ascii_case(host)) {
            v.push((host.to_string(), std::time::Instant::now()));
        }
    }
}

/// Win32 errors meaning the server / share itself is unreachable (not a missing file).
fn is_unreachable_host_error(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(51 | 53 | 64 | 67 | 1219 | 1231 | 1232 | 1311))
}

/// UNC hosts the user is known to use: Explorer address-bar history (TypedPaths) and the servers
/// behind mapped network drives. Used to re-home Everything hits indexed under another name or
/// address of the same server (e.g. a NAS whose IP changed).
fn known_unc_hosts() -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    let mut push = |h: String| {
        if !h.is_empty() && !hosts.iter().any(|x| x.eq_ignore_ascii_case(&h)) {
            hosts.push(h);
        }
    };
    for i in 1..=25 {
        let val_name = format!("url{i}");
        if let Some(val) = crate::registry::read_reg_sz(
            windows::Win32::System::Registry::HKEY_CURRENT_USER,
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\TypedPaths",
            Some(&val_name),
        ) {
            if let Some(h) = unc_server(val.trim()) {
                push(h.to_string());
            }
        }
    }
    for letter in 'A'..='Z' {
        let root = std::path::PathBuf::from(format!("{letter}:\\"));
        if is_remote_drive_path(&root) {
            if let Some(h) = mapped_drive_server(letter) {
                push(h);
            }
        }
    }
    hosts
}

/// Tries the same share-relative path of an unreachable UNC hit on the user's other known
/// servers; returns the first one that the policy allows and whose size matches.
fn try_alternate_servers(full_path: &Path, expected_size: u64) -> Option<std::path::PathBuf> {
    let s = full_path.to_string_lossy();
    let server = unc_server(&s)?;
    let rest = s.trim_start_matches(['\\', '/']).get(server.len()..)?.trim_start_matches(['\\', '/']).to_string();
    for host in known_unc_hosts() {
        if host.eq_ignore_ascii_case(server) || host_recently_unreachable(&host) {
            continue;
        }
        let cand = std::path::PathBuf::from(format!("\\\\{host}\\{rest}"));
        if !is_allowed_network_path(&cand) {
            continue;
        }
        match cand.metadata() {
            Ok(m) if m.is_file() && m.len() == expected_size => return Some(cand),
            Err(e) if is_unreachable_host_error(&e) => mark_host_unreachable(&host),
            _ => {}
        }
    }
    None
}

/// Validates one Everything hit; the error explains why it cannot be used (for the monitor).
fn check_everything_result(full_path: &Path, expected_base_name: &str, expected_size: u64) -> Result<(), String> {
    let shown = full_path.display().to_string();
    let s = full_path.to_string_lossy();
    let network_ok = is_allowed_network_path(full_path);
    if !network_ok && is_network_path(full_path) {
        note_network_rejection(full_path);
        return Err(String::new()); // the network note above is the reason
    }
    if (s.starts_with("\\\\") || s.starts_with("//")) && !network_ok {
        return Err(crate::i18n::t_fmt("reject.path_form", &[("path", &shown)]));
    }
    if s.char_indices().any(|(idx, ch)| ch == ':' && idx != 1)
        || full_path.components().any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(crate::i18n::t_fmt("reject.path_form", &[("path", &shown)]));
    }
    let Some(actual_name) = full_path.file_name().and_then(|n| n.to_str()) else {
        return Err(crate::i18n::t_fmt("reject.name_mismatch", &[("path", &shown)]));
    };
    if !actual_name.eq_ignore_ascii_case(expected_base_name) {
        return Err(crate::i18n::t_fmt("reject.name_mismatch", &[("path", &shown)]));
    }
    if !network_ok && !is_local_fixed_or_removable_drive(full_path) {
        return Err(crate::i18n::t_fmt("reject.not_local", &[("path", &shown)]));
    }
    let host = unc_server(&s).map(str::to_string);
    if let Some(h) = host.as_deref() {
        if host_recently_unreachable(h) {
            return Err(crate::i18n::t_fmt("reject.host_unreachable", &[("server", h)]));
        }
    }
    match full_path.metadata() {
        Ok(m) if m.is_file() => {
            if m.len() == expected_size {
                Ok(())
            } else {
                Err(crate::i18n::t_fmt(
                    "reject.size",
                    &[("path", &shown), ("expected", &expected_size.to_string()), ("actual", &m.len().to_string())],
                ))
            }
        }
        Ok(_) => Err(crate::i18n::t_fmt("reject.not_file", &[("path", &shown), ("err", "not a regular file")])),
        Err(e) => {
            if let Some(h) = host.as_deref() {
                if is_unreachable_host_error(&e) {
                    mark_host_unreachable(h);
                    return Err(crate::i18n::t_fmt("reject.host_unreachable", &[("server", h)]));
                }
            }
            Err(crate::i18n::t_fmt("reject.not_file", &[("path", &shown), ("err", &e.to_string())]))
        }
    }
}

/// Orders Everything hits for validation: local disks first, network shares / mapped network
/// drives last; within each group a hit in the last directory that resolved comes first
/// (folder batches), otherwise Everything's order is kept (stable sort).
fn order_everything_candidates(candidates: &mut [std::path::PathBuf], last_dir: Option<&Path>) {
    candidates.sort_by_key(|p| {
        let in_last_dir = last_dir.is_some_and(|d| p.parent().is_some_and(|pp| same_dir_ignore_case(pp, d)));
        (is_network_path(p), !in_last_dir)
    });
}

fn same_dir_ignore_case(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
    norm(a) == norm(b)
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
        set_resolve_note(crate::i18n::t("trace.everything_unavailable"));
        return None;
    }

    let Ok(client) = everything_ipc::wm::EverythingClient::new() else {
        set_resolve_note(crate::i18n::t("trace.everything_unavailable"));
        return None;
    };
    // `wfn:` (whole file name) exists in Everything 1.4 and 1.5; `exact:` is 1.5-only and makes
    // Everything 1.4 search for the literal text "exact:", so nothing was ever found there.
    let query_str = format!("wfn:\"{}\"", base_name);
    let Ok(list) = client
        .query_wait(&query_str)
        .request_flags(everything_ipc::wm::RequestFlags::FileName | everything_ipc::wm::RequestFlags::Path | everything_ipc::wm::RequestFlags::Size)
        .max_results(8)
        .call()
    else {
        set_resolve_note(crate::i18n::t("trace.everything_unavailable"));
        return None;
    };

    // Collect every hit first, then validate local copies before network ones: the same file
    // often exists both on a local disk and on a NAS share (same name and size), and Everything's
    // result order is arbitrary. Opening the local copy is far faster and validating it does not
    // touch the network at all.
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    for item in list.iter() {
        if let (Some(path_display), Some(name)) = (
            item.get_str(everything_ipc::wm::RequestFlags::Path),
            item.get_string(everything_ipc::wm::RequestFlags::FileName),
        ) {
            candidates.push(std::path::PathBuf::from(path_display.to_string_lossy()).join(name));
        }
    }
    let hits = candidates.len();
    let last_dir = LAST_FOUND_DIR.lock().ok().and_then(|g| g.clone());
    order_everything_candidates(&mut candidates, last_dir.as_deref());

    let mut first_reason = String::new();
    for mut full_path in candidates {
        let mut verdict = check_everything_result(&full_path, base_name, expected_size);
        if verdict.is_err() && unc_server(&full_path.to_string_lossy()).is_some() {
            // Same share under another known server name / address (e.g. the NAS's IP changed
            // since Everything indexed it).
            if let Some(alt) = try_alternate_servers(&full_path, expected_size) {
                if check_everything_result(&alt, base_name, expected_size).is_ok() {
                    full_path = alt;
                    verdict = Ok(());
                }
            }
        }
        if let Err(reason) = &verdict {
            if first_reason.is_empty() && !reason.is_empty() {
                first_reason = reason.clone();
            }
        }
        if verdict.is_ok() {
            if let Some(parent) = full_path.parent() {
                update_last_found_dir(parent);
            }
            return Some(full_path);
        }
    }

    // Keep a more specific reason (e.g. network policy) recorded by the validation above.
    let no_reason_yet = RESOLVE_NOTE.with(|n| n.borrow().is_empty());
    if no_reason_yet {
        if hits == 0 {
            set_resolve_note(crate::i18n::t("trace.everything_no_hit"));
        } else {
            set_resolve_note(crate::i18n::t_fmt(
                "trace.everything_rejected",
                &[("count", &hits.to_string()), ("reason", &first_reason)],
            ));
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
    set_resolve_note(String::new());
    if fname.is_empty() {
        return None;
    }
    let p = std::path::Path::new(fname);
    if fname.starts_with("\\\\") || fname.starts_with("//") {
        // Network shares only when the user opted in (see `NetworkSharePolicy`).
        if is_allowed_network_path(p) && p.is_file() {
            return Some((p.to_path_buf(), ResolveSource::DirectPath));
        }
        if !is_allowed_network_path(p) {
            note_network_rejection(p);
        }
        return None;
    }
    if is_openable_media_path(p) && p.is_file() {
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
        set_resolve_note(crate::i18n::t("trace.no_stream_size"));
        return None;
    }

    // 1. High-speed cache: Check last successfully accessed directory (0.01ms hit rate for folder batch requests!)
    // A cached *network* directory is only tried after Everything: when the same file also
    // exists on a local disk, the local copy must win (and probing the share first would cost
    // an SMB round-trip for every local thumbnail after browsing the NAS).
    let cached_dir = LAST_FOUND_DIR
        .lock()
        .ok()
        .and_then(|g| g.clone())
        // Re-checked on use: the network share policy may have been switched off since.
        .filter(|d| is_openable_media_path(d));
    let try_cached = |dir: &Path| -> Option<std::path::PathBuf> {
        let cand = dir.join(fname);
        (cand.is_file() && size_matches(&cand, expected_size)).then_some(cand)
    };
    if let Some(dir) = cached_dir.as_deref().filter(|d| !is_network_path(d)) {
        if let Some(cand) = try_cached(dir) {
            return Some((cand, ResolveSource::LastDirCache));
        }
    }

    // 2. Query installed Everything instance if available (instant index lookup across NTFS drives)
    if let Some(path) = resolve_via_installed_everything(fname, expected_size) {
        return Some((path, ResolveSource::EverythingIpc));
    }
    if let Some(dir) = cached_dir.as_deref().filter(|d| is_network_path(d)) {
        if let Some(cand) = try_cached(dir) {
            return Some((cand, ResolveSource::LastDirCache));
        }
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
            // Network entries are used only when the user allowed network shares (and the policy's
            // LAN restriction passes).
            let local_ok = is_trusted_local_abs_path(&pb) && is_local_fixed_or_removable_drive(&pb);
            if !local_ok && !is_allowed_network_path(&pb) {
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

    #[test]
    fn everything_hits_prefer_local_copies() {
        use std::path::PathBuf;
        let mut c = vec![
            PathBuf::from(r"\\192.168.137.92\Video\clip.flv"),
            PathBuf::from(r"D:\Other\clip.flv"),
            PathBuf::from(r"D:\Workspace\clip.flv"),
        ];
        order_everything_candidates(&mut c, Some(std::path::Path::new(r"d:\workspace\")));
        assert_eq!(c[0], PathBuf::from(r"D:\Workspace\clip.flv"));
        assert_eq!(c[1], PathBuf::from(r"D:\Other\clip.flv"));
        assert_eq!(c[2], PathBuf::from(r"\\192.168.137.92\Video\clip.flv"));
    }
}
