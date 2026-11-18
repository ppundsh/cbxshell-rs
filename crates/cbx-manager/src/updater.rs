//! Version check and auto-update.
//!
//! Flow (modelled on the Tauri updater used by WinNUT-Client-RS):
//! 1. GET `https://github.com/ppundsh/cbxshell-rs/releases/latest/download/latest.json`
//!    (WinHTTP, system proxy, TLS validated by Windows) and compare its version with ours.
//! 2. On "Install": download the installer named in the manifest — only release assets of this
//!    repository are accepted — and verify its minisign (Ed25519) signature against the public
//!    key compiled into the manager (`assets/updater.pub`). Nothing unsigned is ever executed.
//! 3. Write it to `%TEMP%\CBXShell-Update\`, start it with `--update --dir <install dir>
//!    --wait-pid <our pid>` and close the manager. The installer replaces the files in place,
//!    keeps every setting and file association, and starts the manager again.
//!
//! All network work runs on a background thread; the window is told to repaint with
//! [`WM_APP_UPDATE`].

use crate::update_core::{
    approval_statement, hex, is_newer, is_openpgp_approval, normalize_public_key, parse_approver_keys,
    parse_manifest, parse_openpgp_approver_keys, prepare_approval_check, prepare_openpgp_approval_checks,
    sanitize_version_for_file, sha256, split_https_url, Ed25519Check, Manifest,
};
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Posted to the manager window whenever the update state changes.
pub const WM_APP_UPDATE: u32 = 0x8000 + 0x31;

const MANIFEST_HOST: &str = "github.com";
const MANIFEST_PATH: &str = "/ppundsh/cbxshell-rs/releases/latest/download/latest.json";
pub const RELEASES_PAGE: &str = "https://github.com/ppundsh/cbxshell-rs/releases";
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_INSTALLER_BYTES: u64 = 256 * 1024 * 1024;

/// minisign public key used to verify installers (see doc/updates.md for key generation).
const PUBLIC_KEY_FILE: &str = include_str!("../../../assets/updater.pub");
/// Maintainer YubiKey (FIDO2 sk-ssh-ed25519) keys whose approval every release needs once at
/// least one key is listed. The CI signing key alone can then no longer ship an update.
const APPROVERS_FILE: &str = include_str!("../../../assets/release-approvers.pub");

#[derive(Clone, Debug)]
pub enum Phase {
    Idle,
    Checking,
    UpToDate,
    Available(Manifest),
    Downloading { done: u64, total: u64, version: String },
    Launching,
    Error(String),
}

static PHASE: Mutex<Phase> = Mutex::new(Phase::Idle);
static BUSY: AtomicBool = AtomicBool::new(false);
/// Set when a check finished successfully (the window persists the timestamp).
static CHECK_COMPLETED: AtomicBool = AtomicBool::new(false);

pub fn phase() -> Phase {
    PHASE.lock().map(|p| p.clone()).unwrap_or(Phase::Idle)
}

/// True once per successful check; the UI thread stores `last_update_check` when it sees it.
pub fn take_check_completed() -> bool {
    CHECK_COMPLETED.swap(false, Ordering::AcqRel)
}

pub fn is_configured() -> bool {
    normalize_public_key(PUBLIC_KEY_FILE).is_some()
}

pub fn is_busy() -> bool {
    BUSY.load(Ordering::Acquire)
}

fn set_phase(hwnd: usize, p: Phase) {
    if let Ok(mut g) = PHASE.lock() {
        *g = p;
    }
    // SAFETY: plain message post to our own window; a stale handle just fails.
    unsafe {
        post_message_w(hwnd as *mut c_void, WM_APP_UPDATE, 0, 0);
    }
}

/// Starts a version check on a background thread (no-op while another update task runs).
pub fn start_check(hwnd: *mut c_void) {
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    let h = hwnd as usize;
    std::thread::spawn(move || {
        set_phase(h, Phase::Checking);
        let result = fetch_manifest();
        let next = match result {
            Ok(m) if is_newer(&m.version, env!("CARGO_PKG_VERSION")) => Phase::Available(m),
            Ok(_) => Phase::UpToDate,
            Err(e) => Phase::Error(e),
        };
        if !matches!(next, Phase::Error(_)) {
            CHECK_COMPLETED.store(true, Ordering::Release);
        }
        BUSY.store(false, Ordering::Release);
        set_phase(h, next);
    });
}

/// Downloads, verifies and launches the installer for `manifest`, then asks the window to close.
pub fn start_install(hwnd: *mut c_void, manifest: Manifest) {
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    let h = hwnd as usize;
    std::thread::spawn(move || {
        let result = download_verify_launch(h, &manifest);
        BUSY.store(false, Ordering::Release);
        match result {
            Ok(()) => {
                set_phase(h, Phase::Launching);
                // SAFETY: closes our own window; the installer waits for this process to exit.
                unsafe {
                    post_message_w(h as *mut c_void, 0x0010 /* WM_CLOSE */, 0, 0);
                }
            }
            Err(e) => set_phase(h, Phase::Error(e)),
        }
    });
}

fn fetch_manifest() -> Result<Manifest, String> {
    if !is_configured() {
        return Err(crate::tr(
            "此版本未設定更新簽章金鑰，無法自動更新",
            "This build has no update signing key; automatic updates are disabled",
        )
        .into());
    }
    let body = http_get(MANIFEST_HOST, MANIFEST_PATH, MAX_MANIFEST_BYTES, |_, _| {})?;
    let text = String::from_utf8(body).map_err(|_| "manifest is not UTF-8".to_string())?;
    parse_manifest(&text)
}

fn verify_signature(data: &[u8], signature_text: &str) -> Result<(), String> {
    let key = normalize_public_key(PUBLIC_KEY_FILE).ok_or("update signing key missing")?;
    let pk = minisign_verify::PublicKey::from_base64(&key).map_err(|e| format!("public key: {e}"))?;
    let sig = minisign_verify::Signature::decode(signature_text).map_err(|e| format!("signature: {e}"))?;
    pk.verify(data, &sig, true).map_err(|e| {
        format!(
            "{}: {e}",
            crate::tr("安裝檔簽章驗證失敗，已中止更新", "Installer signature verification failed; update aborted")
        )
    })
}

/// Second, independent signature: the maintainer's YubiKey approval of `version + sha256`,
/// made either with a FIDO2 SSH key (`ssh-keygen -Y sign`) or with the GnuPG key on the
/// YubiKey's OpenPGP applet (`gpg --detach-sign --armor`).
fn verify_approval(data: &[u8], m: &Manifest) -> Result<(), String> {
    let sk_keys = parse_approver_keys(APPROVERS_FILE);
    let pgp_keys = parse_openpgp_approver_keys(APPROVERS_FILE);
    if sk_keys.is_empty() && pgp_keys.is_empty() {
        return Ok(()); // approval not configured for this build
    }
    let fail = |e: &str| {
        format!(
            "{}: {e}",
            crate::tr(
                "缺少維護者 YubiKey 核准簽章或驗證失敗，已中止更新",
                "Maintainer YubiKey approval missing or invalid; update aborted"
            )
        )
    };
    if m.approval.trim().is_empty() {
        return Err(fail("no approval in latest.json"));
    }
    let statement = approval_statement(&m.version, &hex(&sha256(data)));
    let checks: Vec<Ed25519Check> = if is_openpgp_approval(&m.approval) {
        prepare_openpgp_approval_checks(&m.approval, statement.as_bytes(), &pgp_keys).map_err(|e| fail(&e))?
    } else {
        vec![prepare_approval_check(&m.approval, statement.as_bytes(), &sk_keys).map_err(|e| fail(&e))?]
    };
    let verified = checks.iter().any(|c| {
        match (
            ed25519_compact::PublicKey::from_slice(&c.public_key),
            ed25519_compact::Signature::from_slice(&c.signature),
        ) {
            (Ok(pk), Ok(sig)) => pk.verify(&c.message, &sig).is_ok(),
            _ => false,
        }
    });
    if verified {
        Ok(())
    } else {
        Err(fail("signature does not match an approver key"))
    }
}

/// True when this build requires a YubiKey approval for updates.
pub fn requires_approval() -> bool {
    !parse_approver_keys(APPROVERS_FILE).is_empty() || !parse_openpgp_approver_keys(APPROVERS_FILE).is_empty()
}

fn download_verify_launch(hwnd: usize, m: &Manifest) -> Result<(), String> {
    let (host, path) = split_https_url(&m.url).ok_or("invalid installer URL")?;
    set_phase(hwnd, Phase::Downloading { done: 0, total: 0, version: m.version.clone() });
    let mut last_pct = u64::MAX;
    let data = http_get(host, path, MAX_INSTALLER_BYTES, |done, total| {
        let pct = if total > 0 { done * 100 / total } else { done >> 20 };
        if pct != last_pct {
            last_pct = pct;
            set_phase(hwnd, Phase::Downloading { done, total, version: m.version.clone() });
        }
    })?;
    if data.len() < 2 || &data[..2] != b"MZ" {
        return Err("downloaded file is not a Windows executable".into());
    }
    verify_signature(&data, &m.signature)?;
    verify_approval(&data, m)?;

    let dir = std::env::temp_dir().join("CBXShell-Update");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Leave nothing from earlier attempts lying around.
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let exe = dir.join(format!("CBXShell-Setup-{}.exe", sanitize_version_for_file(&m.version)));
    std::fs::write(&exe, &data).map_err(|e| format!("{}: {e}", exe.display()))?;

    let install_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .ok_or("cannot locate the install folder")?;
    let args = format!(
        "--update --dir \"{}\" --wait-pid {}",
        install_dir.display(),
        std::process::id()
    );
    shell_execute(&exe, &args)
}

/// Opens the release page (for the given version, or the list of releases) in the browser.
pub fn open_release_page(version: Option<&str>) {
    let url = match version {
        Some(v) => format!("{RELEASES_PAGE}/tag/v{}", sanitize_version_for_file(v.trim_start_matches(['v', 'V']))),
        None => RELEASES_PAGE.to_string(),
    };
    let op = wide("open");
    let file = wide(&url);
    // SAFETY: NUL-terminated buffers that outlive the call.
    unsafe {
        shell_execute_w(std::ptr::null_mut(), op.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), 1);
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn shell_execute(exe: &std::path::Path, args: &str) -> Result<(), String> {
    let op = wide("open");
    let file = wide(&exe.to_string_lossy());
    let params = wide(args);
    // SAFETY: all strings are NUL-terminated and outlive the call.
    let r = unsafe {
        shell_execute_w(
            std::ptr::null_mut(),
            op.as_ptr(),
            file.as_ptr(),
            params.as_ptr(),
            std::ptr::null(),
            1, /* SW_SHOWNORMAL */
        )
    };
    if r as isize > 32 {
        Ok(())
    } else {
        Err(format!("failed to start the installer (code {})", r as isize))
    }
}

// ---------------------------------------------------------------------------------------------
// WinHTTP (declared locally to keep the dependency surface small)

type HInternet = *mut c_void;

#[link(name = "winhttp")]
extern "system" {
    fn WinHttpOpen(agent: *const u16, access: u32, proxy: *const u16, bypass: *const u16, flags: u32) -> HInternet;
    fn WinHttpConnect(session: HInternet, server: *const u16, port: u16, reserved: u32) -> HInternet;
    fn WinHttpOpenRequest(
        connect: HInternet,
        verb: *const u16,
        object: *const u16,
        version: *const u16,
        referrer: *const u16,
        accept: *const *const u16,
        flags: u32,
    ) -> HInternet;
    fn WinHttpSendRequest(
        request: HInternet,
        headers: *const u16,
        headers_len: u32,
        optional: *const c_void,
        optional_len: u32,
        total_len: u32,
        context: usize,
    ) -> i32;
    fn WinHttpReceiveResponse(request: HInternet, reserved: *mut c_void) -> i32;
    fn WinHttpQueryHeaders(
        request: HInternet,
        info_level: u32,
        name: *const u16,
        buffer: *mut c_void,
        buffer_len: *mut u32,
        index: *mut u32,
    ) -> i32;
    fn WinHttpReadData(request: HInternet, buffer: *mut c_void, to_read: u32, read: *mut u32) -> i32;
    fn WinHttpSetTimeouts(handle: HInternet, resolve: i32, connect: i32, send: i32, receive: i32) -> i32;
    fn WinHttpCloseHandle(handle: HInternet) -> i32;
}

#[link(name = "user32")]
extern "system" {
    #[link_name = "PostMessageW"]
    fn post_message_w(hwnd: *mut c_void, msg: u32, wparam: usize, lparam: isize) -> i32;
}

#[link(name = "shell32")]
extern "system" {
    #[link_name = "ShellExecuteW"]
    fn shell_execute_w(
        hwnd: *mut c_void,
        op: *const u16,
        file: *const u16,
        params: *const u16,
        dir: *const u16,
        show: i32,
    ) -> *mut c_void;
}

const WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
const WINHTTP_ACCESS_TYPE_DEFAULT_PROXY: u32 = 0;
const WINHTTP_FLAG_SECURE: u32 = 0x0080_0000;
const WINHTTP_QUERY_STATUS_CODE: u32 = 19;
const WINHTTP_QUERY_CONTENT_LENGTH: u32 = 5;
const WINHTTP_QUERY_FLAG_NUMBER: u32 = 0x2000_0000;

struct Handle(HInternet);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: handle came from WinHttp* and is closed exactly once.
            unsafe {
                WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn last_err(what: &str) -> String {
    format!("{what}: {}", std::io::Error::last_os_error())
}

/// HTTPS GET with redirects followed by WinHTTP (GitHub release assets redirect to a CDN).
fn http_get(host: &str, path: &str, max_bytes: u64, mut progress: impl FnMut(u64, u64)) -> Result<Vec<u8>, String> {
    let agent = wide(concat!("CBXShell-Updater/", env!("CARGO_PKG_VERSION")));
    let host_w = wide(host);
    let path_w = wide(path);
    let verb = wide("GET");
    // SAFETY: every pointer passed below is a live NUL-terminated buffer or null; handles are
    // wrapped in `Handle` so they are closed on every path.
    unsafe {
        let mut session = WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if session.is_null() {
            // Windows 8.0 and older do not know AUTOMATIC_PROXY.
            session = WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_DEFAULT_PROXY, std::ptr::null(), std::ptr::null(), 0);
        }
        let session = Handle(session);
        if session.0.is_null() {
            return Err(last_err("WinHttpOpen"));
        }
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);
        let connect = Handle(WinHttpConnect(session.0, host_w.as_ptr(), 443, 0));
        if connect.0.is_null() {
            return Err(last_err("WinHttpConnect"));
        }
        let request = Handle(WinHttpOpenRequest(
            connect.0,
            verb.as_ptr(),
            path_w.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return Err(last_err("WinHttpOpenRequest"));
        }
        if WinHttpSendRequest(request.0, std::ptr::null(), 0, std::ptr::null(), 0, 0, 0) == 0 {
            return Err(last_err(crate::tr("無法連線到 GitHub", "Cannot reach GitHub")));
        }
        if WinHttpReceiveResponse(request.0, std::ptr::null_mut()) == 0 {
            return Err(last_err(crate::tr("無法連線到 GitHub", "Cannot reach GitHub")));
        }
        let mut status: u32 = 0;
        let mut len = 4u32;
        if WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            &mut status as *mut u32 as *mut c_void,
            &mut len,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(last_err("HTTP status"));
        }
        if status == 404 {
            return Err(crate::tr("找不到發行版本資訊（latest.json）", "Release manifest (latest.json) not found").into());
        }
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }
        let mut total: u32 = 0;
        let mut len = 4u32;
        let _ = WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            &mut total as *mut u32 as *mut c_void,
            &mut len,
            std::ptr::null_mut(),
        );
        if total as u64 > max_bytes {
            return Err("response too large".into());
        }
        let mut out: Vec<u8> = Vec::with_capacity((total as usize).min(max_bytes as usize));
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut read: u32 = 0;
            if WinHttpReadData(request.0, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read) == 0 {
                return Err(last_err("download"));
            }
            if read == 0 {
                break;
            }
            out.extend_from_slice(&buf[..read as usize]);
            if out.len() as u64 > max_bytes {
                return Err("response too large".into());
            }
            progress(out.len() as u64, total as u64);
        }
        if total != 0 && out.len() as u64 != total as u64 {
            return Err("download incomplete".into());
        }
        Ok(out)
    }
}
