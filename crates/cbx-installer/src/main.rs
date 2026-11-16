#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use cbx_core::icon::{create_app_hicon, generate_app_ico_bytes};
use cbx_core::registry::{
    associate_extension, clear_thumbnail_cache, disassociate_extension,
    notify_shell_associations_changed, read_reg_sz, write_reg_sz,
};
use cbx_core::settings::{to_wide, Settings, UiLang, APP_REG_KEY};
use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use windows::core::{w, Interface, GUID, HRESULT, PCSTR, PCWSTR};
use windows::Win32::Foundation::{COLORREF, FreeLibrary, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW, CreatePen,
    CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, InvalidateRect,
    LineTo, MoveToEx, RoundRect, SelectObject, SetBkColor, SetBkMode, SetTextColor,
    CLEARTYPE_QUALITY, DEFAULT_CHARSET, DEFAULT_PITCH, DT_CENTER, DT_LEFT, DT_NOPREFIX,
    DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, FF_DONTCARE, FW_BOLD, FW_NORMAL, HBRUSH, HDC, HFONT,
    PAINTSTRUCT, PS_SOLID, SRCCOPY, TRANSPARENT,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{
    RegDeleteTreeW, RegGetValueW, RegSetKeyValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
    REG_DWORD, RRF_RT_REG_DWORD,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForSystem, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW, GetClientRect,
    GetDlgItem, GetMessageW, GetSystemMetrics, GetWindowTextW, LoadCursorW, MessageBoxW,
    PostQuitMessage, RegisterClassW, SendMessageW, SetCursor, SetWindowTextW, ShowWindow,
    TranslateMessage, ES_AUTOHSCROLL, IDC_ARROW, IDC_HAND, IDYES, MB_ICONERROR,
    MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO, MSG, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW,
    WM_COMMAND, WM_CREATE, WM_CTLCOLOREDIT, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WM_SETCURSOR, WM_SETFONT, WNDCLASSW, WS_CAPTION,
    WS_CHILD, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};

const CLSID_SHELL_LINK: GUID = GUID::from_u128(0x00021401_0000_0000_c000_000000000046);

const PAYLOAD_ZIP: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.zip"));

const UNINSTALL_REG_KEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\CBXShell";

const ID_EDIT_DIR: i32 = 100;
const ID_CB_CAT_ARCHIVE: i32 = 110;
const ID_CB_CAT_VIDEO: i32 = 111;
const ID_CB_CAT_AUDIO: i32 = 112;
const ID_LBL_CAT_HINT: i32 = 113;
const ID_CB_CAT_EBOOK: i32 = 114;
const ID_CB_CAT_OTHER: i32 = 115;

const ID_CB_BADGE_ANIM: i32 = 201;
const ID_CB_BADGE_STATIC: i32 = 202;
const ID_CB_SHORTCUT: i32 = 203;
const ID_CB_CLEAR_CACHE: i32 = 204;
const ID_CB_LAUNCH_MGR: i32 = 205;
const ID_CB_ANOMALY: i32 = 206;
const ID_CB_HDR_TONEMAP: i32 = 211;
const ID_CB_INFOTIP: i32 = 212;
const ID_LBL_FFMPEG_STATUS: i32 = 213;
const ID_LBL_FFMPEG_DESC: i32 = 214;
const ID_BTN_FFMPEG_URL: i32 = 215;
const ID_LBL_EVERYTHING_STATUS: i32 = 216;
const ID_LBL_EVERYTHING_DESC: i32 = 217;
const ID_BTN_EVERYTHING_URL: i32 = 218;

const ID_GRP_DIR: i32 = 300;
const ID_GRP_TYPES: i32 = 301;
const ID_GRP_OPTS: i32 = 302;
const ID_GRP_COMPANIONS: i32 = 303;

const ID_BTN_INSTALL: i32 = 1;
const ID_BTN_CANCEL: i32 = 2;
const ID_BTN_LANG: i32 = 3;

const BST_UNCHECKED: usize = 0;
const BST_CHECKED: usize = 1;

static CURRENT_LANG: AtomicU8 = AtomicU8::new(0);

fn current_lang() -> UiLang {
    match CURRENT_LANG.load(Ordering::Relaxed) {
        1 => UiLang::ZhTw,
        2 => UiLang::En,
        _ => UiLang::load_or_detect(),
    }
}

fn set_current_lang(lang: UiLang) {
    let code = match lang {
        UiLang::ZhTw => 1,
        UiLang::En => 2,
    };
    CURRENT_LANG.store(code, Ordering::Relaxed);
}

fn tr(zh: &'static str, en: &'static str) -> &'static str {
    match current_lang() {
        UiLang::ZhTw => zh,
        UiLang::En => en,
    }
}

fn main() {
    cbx_core::registry::harden_process_dll_search();
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }

    let args: Vec<String> = std::env::args().collect();
    let mut lang = UiLang::load_or_detect();
    if let Some(pair) = args
        .windows(2)
        .find(|w| w[0].eq_ignore_ascii_case("--lang") || w[0].eq_ignore_ascii_case("/lang"))
    {
        if pair[1].to_ascii_lowercase().starts_with("zh") {
            lang = UiLang::ZhTw;
        } else if pair[1].to_ascii_lowercase().starts_with("en") {
            lang = UiLang::En;
        }
    }
    set_current_lang(lang);

    let exe_name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|s| s.to_string_lossy().to_ascii_lowercase()))
        .unwrap_or_default();

    let is_uninstall = exe_name.contains("uninstall")
        || args.iter().any(|a| a.eq_ignore_ascii_case("--uninstall") || a.eq_ignore_ascii_case("/u"));
    let is_quiet = args.iter().any(|a| {
        a.eq_ignore_ascii_case("--silent")
            || a.eq_ignore_ascii_case("--quiet")
            || a.eq_ignore_ascii_case("/s")
            || a.eq_ignore_ascii_case("/quiet")
    });
    let custom_dir = args
        .windows(2)
        .find(|w| w[0].eq_ignore_ascii_case("--dir"))
        .map(|w| PathBuf::from(&w[1]));

    let code = if is_uninstall {
        run_uninstaller(custom_dir, is_quiet)
    } else if is_quiet {
        let target_dir = custom_dir.unwrap_or_else(default_install_dir);
        let opts = InstallOptions::default_with_dir(target_dir);
        match perform_install(&opts) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("Silent installation failed: {e}");
                1
            }
        }
    } else {
        run_gui_installer(custom_dir)
    };

    unsafe {
        CoUninitialize();
    }
    std::process::exit(code);
}

fn detect_media_preview_installed() -> bool {
    read_reg_sz(HKEY_LOCAL_MACHINE, r"Software\BabelSoft\Media Preview", None).is_some()
        || read_reg_sz(HKEY_CURRENT_USER, r"Software\BabelSoft\Media Preview", None).is_some()
        || read_reg_sz(HKEY_LOCAL_MACHINE, r"Software\BabelSoft\Media Preview", Some("InstallLocation")).is_some()
}

#[derive(Clone)]
struct InstallOptions {
    install_dir: PathBuf,
    enable_image_archives: bool,
    enable_ebook_formats: bool,
    enable_video_formats: bool,
    enable_audio_formats: bool,
    enable_other_formats: bool,
    badge_animated: bool,
    badge_static: bool,
    badge_video: bool,
    install_ffmpeg: bool,
    anomaly_detection: bool,
    hdr_tonemap: bool,
    enable_infotip: bool,
    create_shortcut: bool,
    clear_cache: bool,
    launch_manager: bool,
}

impl InstallOptions {
    fn default_with_dir(install_dir: PathBuf) -> Self {
        Self {
            install_dir,
            enable_image_archives: true,
            enable_ebook_formats: true,
            enable_video_formats: true,
            enable_audio_formats: true,
            enable_other_formats: true,
            badge_animated: true,
            badge_static: true,
            badge_video: false,
            install_ffmpeg: true,
            anomaly_detection: true,
            hdr_tonemap: true,
            enable_infotip: true,
            create_shortcut: true,
            clear_cache: true,
            launch_manager: false,
        }
    }
}

fn default_install_dir() -> PathBuf {
    if let Some(existing) = read_reg_sz(HKEY_CURRENT_USER, UNINSTALL_REG_KEY, Some("InstallLocation"))
    {
        if !existing.trim().is_empty() {
            return PathBuf::from(existing);
        }
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        return PathBuf::from(local).join("Programs").join("CBXShell");
    }
    std::env::var_os("ProgramFiles")
        .map(|p| PathBuf::from(p).join("CBXShell"))
        .unwrap_or_else(|| PathBuf::from("CBXShell"))
}

fn start_menu_shortcut_path() -> Option<PathBuf> {
    std::env::var("APPDATA").ok().map(|appdata| {
        PathBuf::from(appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
            .join("CBX Shell Manager.lnk")
    })
}

fn current_yyyymmdd() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Convert to the machine's *actual* local time zone (no hard-coded offset).
    let local_secs = cbx_core::monitor::utc_epoch_secs_to_local(secs);
    let days = (local_secs / 86400) as i64;
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}{m:02}{d:02}")
}

fn write_reg_dword(root: HKEY, subkey: &str, name: &str, val: u32) {
    let subkey_w = to_wide(subkey);
    let name_w = to_wide(name);
    unsafe {
        let _ = RegSetKeyValueW(
            root,
            PCWSTR(subkey_w.as_ptr()),
            PCWSTR(name_w.as_ptr()),
            REG_DWORD.0,
            Some((&val as *const u32).cast()),
            4,
        );
    }
}

fn call_dll_entry(dll_path: &Path, proc_name: &[u8]) -> Result<(), String> {
    let dll_w = to_wide(&dll_path.to_string_lossy());
    unsafe {
        let hdll = LoadLibraryW(PCWSTR(dll_w.as_ptr()))
            .map_err(|e| format!("Failed to load {}: {e}", dll_path.display()))?;
        let proc = GetProcAddress(hdll, PCSTR::from_raw(proc_name.as_ptr()));
        let res = if let Some(f) = proc {
            type EntryFn = unsafe extern "system" fn() -> HRESULT;
            let func: EntryFn = std::mem::transmute(f);
            let hr = func();
            if hr.is_ok() {
                Ok(())
            } else {
                Err(format!("DLL entry returned HRESULT 0x{:08X}", hr.0 as u32))
            }
        } else {
            Err("Exported function not found in cbxshell.dll".into())
        };
        let _ = FreeLibrary(hdll);
        res
    }
}

fn create_shell_shortcut(lnk_path: &Path, target_exe: &Path, work_dir: &Path, ico_path: &Path) {
    unsafe {
        let Ok(sl): Result<IShellLinkW, _> =
            CoCreateInstance(&CLSID_SHELL_LINK, None, CLSCTX_INPROC_SERVER)
        else {
            return;
        };
        let target_w = to_wide(&target_exe.to_string_lossy());
        let work_w = to_wide(&work_dir.to_string_lossy());
        let ico_w = to_wide(&ico_path.to_string_lossy());
        let desc_w = to_wide("CBX Shell Manager - Configure comic & e-book thumbnail settings");

        let _ = sl.SetPath(PCWSTR(target_w.as_ptr()));
        let _ = sl.SetWorkingDirectory(PCWSTR(work_w.as_ptr()));
        let _ = sl.SetIconLocation(PCWSTR(ico_w.as_ptr()), 0);
        let _ = sl.SetDescription(PCWSTR(desc_w.as_ptr()));

        if let Ok(pf) = sl.cast::<IPersistFile>() {
            let lnk_w = to_wide(&lnk_path.to_string_lossy());
            let _ = pf.Save(PCWSTR(lnk_w.as_ptr()), true);
        }
    }
}

fn perform_install(opts: &InstallOptions) -> Result<(), String> {
    let dir = &opts.install_dir;
    fs::create_dir_all(dir).map_err(|e| format!("Cannot create directory '{}': {e}", dir.display()))?;

    // If an older cbxshell.dll is already in the target folder, unregister it first
    let dll_target = dir.join("cbxshell.dll");
    if dll_target.exists() {
        let _ = call_dll_entry(&dll_target, b"DllUnregisterServer\0");
    }

    // Extract embedded payload.zip into the target directory
    let mut zip = zip::ZipArchive::new(Cursor::new(PAYLOAD_ZIP))
        .map_err(|e| format!("Corrupt installer payload: {e}"))?;

    let mut total_bytes: u64 = 0;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().to_string();
        // Harden payload extraction against path traversal, absolute path overwrites, device names, and colons
        // (Enforces strict containment against CVE-2026-61725 and CVE-2026-47699 patterns)
        if name.is_empty()
            || name.contains("..")
            || name.contains('/')
            || name.contains('\\')
            || name.contains(':')
            || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
        {
            continue;
        }
        let dest = dir.join(&name);
        if !dest.starts_with(dir) {
            continue;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        total_bytes += buf.len() as u64;

        // If target file is temporarily locked, rename the old file to .old before writing
        if dest.exists() && fs::remove_file(&dest).is_err() {
            let old = dir.join(format!("{name}.old"));
            let _ = fs::remove_file(&old);
            let _ = fs::rename(&dest, &old);
        }

        let mut out =
            File::create(&dest).map_err(|e| format!("Failed to write '{}': {e}", dest.display()))?;
        out.write_all(&buf).map_err(|e| e.to_string())?;
    }

    // Write application icon (`app.ico`)
    let ico_path = dir.join("app.ico");
    let ico_bytes = generate_app_ico_bytes();
    total_bytes += ico_bytes.len() as u64;
    let _ = fs::write(&ico_path, &ico_bytes);

    // Copy current installer binary as `uninstall.exe`
    let uninst_path = dir.join("uninstall.exe");
    if let Ok(cur_exe) = std::env::current_exe() {
        if cur_exe != uninst_path {
            if let Ok(exe_bytes) = fs::read(&cur_exe) {
                total_bytes += exe_bytes.len() as u64;
                let _ = fs::write(&uninst_path, &exe_bytes);
            }
        }
    }

    // If FFmpeg installation was selected and ffmpeg.exe is found alongside installer, deploy it
    if opts.install_ffmpeg {
        if let Ok(cur_exe) = std::env::current_exe() {
            if let Some(parent) = cur_exe.parent() {
                let local_ffmpeg = parent.join("ffmpeg.exe");
                if local_ffmpeg.is_file() {
                    let bin_dir = dir.join("bin");
                    let _ = fs::create_dir_all(&bin_dir);
                    let target_ff = bin_dir.join("ffmpeg.exe");
                    let _ = fs::copy(&local_ffmpeg, &target_ff);
                }
            }
        }
    }

    // Save user badge, language & general settings before registering DLL
    current_lang().save();
    let mut settings = Settings::load();
    settings.badge_epub = false;
    settings.badge_animated = opts.badge_animated;
    settings.badge_static = opts.badge_static;
    settings.badge_video = opts.badge_video;
    settings.anomaly_detection = opts.anomaly_detection;
    settings.enable_infotip = opts.enable_infotip;
    settings.position_seek = 25;
    settings.hdr_tonemap_enabled = opts.hdr_tonemap;
    settings.save();

    // Register cbxshell.dll if present in payload
    if dll_target.exists() {
        call_dll_entry(&dll_target, b"DllRegisterServer\0")?;
    }

    // Apply selected file type associations for all categories
    for fmt in cbx_core::media::media_types::SUPPORTED_MEDIA_FORMATS {
        let want = match fmt.category {
            cbx_core::media::media_types::MediaCategory::ImageArchive => {
                opts.enable_image_archives && fmt.default_on
            }
            cbx_core::media::media_types::MediaCategory::Ebook => {
                opts.enable_ebook_formats && fmt.default_on
            }
            cbx_core::media::media_types::MediaCategory::Video => {
                opts.enable_video_formats && fmt.default_on
            }
            cbx_core::media::media_types::MediaCategory::Audio => {
                opts.enable_audio_formats && fmt.default_on
            }
            cbx_core::media::media_types::MediaCategory::Other => {
                opts.enable_other_formats && fmt.default_on
            }
            cbx_core::media::media_types::MediaCategory::Syntax => false,
        };
        if want {
            associate_extension(
                HKEY_CURRENT_USER,
                fmt.ext,
                settings.enable_infotip
                    && fmt.category == cbx_core::media::media_types::MediaCategory::ImageArchive,
            );
        } else {
            disassociate_extension(HKEY_CURRENT_USER, fmt.ext);
        }
    }

    // Register in Windows 10 / 11 "Installed Apps" (設定 > 應用程式 > 已安裝的應用程式)
    let dir_str = dir.to_string_lossy().into_owned();
    let mgr_path = dir.join("cbx-manager.exe");
    let uninst_cmd = format!("\"{}\" --uninstall", uninst_path.display());
    let quiet_uninst_cmd = format!("\"{}\" --uninstall --quiet", uninst_path.display());
    let modify_cmd = format!("\"{}\"", mgr_path.display());
    let display_icon = ico_path.to_string_lossy().into_owned();
    let est_kb = ((total_bytes + 1023) / 1024).max(64) as u32;
    let install_date = current_yyyymmdd();

    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        if write_reg_sz(root, UNINSTALL_REG_KEY, Some("DisplayName"), "CBX Shell") {
            write_reg_sz(
                root,
                UNINSTALL_REG_KEY,
                Some("DisplayVersion"),
                env!("CARGO_PKG_VERSION"),
            );
            write_reg_sz(
                root,
                UNINSTALL_REG_KEY,
                Some("Publisher"),
                "CBXShell Project",
            );
            write_reg_sz(root, UNINSTALL_REG_KEY, Some("DisplayIcon"), &display_icon);
            write_reg_sz(root, UNINSTALL_REG_KEY, Some("InstallDate"), &install_date);
            write_reg_sz(root, UNINSTALL_REG_KEY, Some("InstallLocation"), &dir_str);
            write_reg_sz(root, UNINSTALL_REG_KEY, Some("UninstallString"), &uninst_cmd);
            write_reg_sz(
                root,
                UNINSTALL_REG_KEY,
                Some("QuietUninstallString"),
                &quiet_uninst_cmd,
            );
            write_reg_sz(root, UNINSTALL_REG_KEY, Some("ModifyPath"), &modify_cmd);
            write_reg_dword(root, UNINSTALL_REG_KEY, "EstimatedSize", est_kb);
            write_reg_dword(root, UNINSTALL_REG_KEY, "NoModify", 0);
            write_reg_dword(root, UNINSTALL_REG_KEY, "NoRepair", 1);
        }
    }

    // Create Start Menu shortcut if requested
    if opts.create_shortcut {
        if let Some(lnk) = start_menu_shortcut_path() {
            create_shell_shortcut(&lnk, &mgr_path, dir, &ico_path);
        }
    }

    if opts.clear_cache {
        clear_thumbnail_cache();
    } else {
        notify_shell_associations_changed();
    }

    if opts.launch_manager && mgr_path.exists() {
        let _ = Command::new(&mgr_path).current_dir(dir).spawn();
    }

    Ok(())
}

fn run_uninstaller(custom_dir: Option<PathBuf>, quiet: bool) -> i32 {
    let install_dir = custom_dir
        .or_else(|| {
            read_reg_sz(HKEY_CURRENT_USER, UNINSTALL_REG_KEY, Some("InstallLocation"))
                .or_else(|| read_reg_sz(HKEY_LOCAL_MACHINE, UNINSTALL_REG_KEY, Some("InstallLocation")))
                .map(PathBuf::from)
        })
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        })
        .unwrap_or_else(default_install_dir);

    let cur_exe = std::env::current_exe().unwrap_or_default();
    let is_from_temp = std::env::args().any(|a| a.eq_ignore_ascii_case("--from-temp"));

    // If running directly as uninstall.exe inside install_dir, copy to %TEMP% and relaunch so install_dir isn't locked
    if !is_from_temp && cur_exe.parent().map(|p| p == install_dir).unwrap_or(false) {
        // Copy into a *fresh, uniquely named* sub-directory rather than a fixed name directly in
        // %TEMP%: a fixed path can be pre-planted/raced, and an exe launched from %TEMP% itself
        // would pick up any DLLs dropped there (binary planting).
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let temp_dir = std::env::temp_dir().join(format!(
            "cbxshell-uninst-{}-{:x}",
            std::process::id(),
            nonce
        ));
        let temp_uninst = temp_dir.join("cbxshell_uninst.exe");
        if fs::create_dir(&temp_dir).is_ok() && fs::copy(&cur_exe, &temp_uninst).is_ok() {
            let mut spawn_cmd = Command::new(&temp_uninst);
            spawn_cmd
                .arg("--uninstall")
                .arg("--from-temp")
                .arg("--dir")
                .arg(&install_dir);
            if quiet {
                spawn_cmd.arg("--quiet");
            }
            if let Ok(child) = spawn_cmd.spawn() {
                drop(child);
                return 0;
            }
        }
    }

    if is_from_temp {
        // Brief pause to allow the launching process to exit and unlock uninstall.exe
        std::thread::sleep(std::time::Duration::from_millis(300));
    }

    if !quiet {
        let prompt = to_wide(tr(
            "確定要解除安裝 CBX Shell 並還原原有的檔案縮圖關聯嗎？",
            "Are you sure you want to uninstall CBX Shell and restore previous file associations?",
        ));
        let title = to_wide(tr("解除安裝 CBX Shell", "Uninstall CBX Shell"));
        let ans = unsafe {
            MessageBoxW(
                HWND::default(),
                PCWSTR(prompt.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_YESNO | MB_ICONQUESTION,
            )
        };
        if ans != IDYES {
            return 0;
        }
    }

    // 1. Unregister COM DLL and restore previous associations
    let dll_path = install_dir.join("cbxshell.dll");
    if dll_path.exists() {
        let _ = call_dll_entry(&dll_path, b"DllUnregisterServer\0");
    }
    for fmt in cbx_core::media::media_types::SUPPORTED_MEDIA_FORMATS {
        disassociate_extension(HKEY_CURRENT_USER, fmt.ext);
        disassociate_extension(HKEY_LOCAL_MACHINE, fmt.ext);
    }

    // 2. Remove Uninstall & App settings registry keys
    let uninst_w = to_wide(UNINSTALL_REG_KEY);
    let app_w = to_wide(APP_REG_KEY);
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        unsafe {
            let _ = RegDeleteTreeW(root, PCWSTR(uninst_w.as_ptr()));
            let _ = RegDeleteTreeW(root, PCWSTR(app_w.as_ptr()));
        }
    }

    // 3. Remove Start Menu shortcut
    if let Some(lnk) = start_menu_shortcut_path() {
        let _ = fs::remove_file(lnk);
    }

    // 4. Delete installed files
    let known_files = [
        "cbxshell.dll",
        "cbxshell.dll.old",
        "cbx-manager.exe",
        "cbx-cli.exe",
        "app.ico",
        "LICENSE",
        "README.md",
        "README.zh-TW.md",
        "uninstall.exe",
    ];
    for name in known_files {
        let p = install_dir.join(name);
        if p.exists() {
            let _ = fs::remove_file(&p);
        }
    }

    let _ = fs::remove_dir(&install_dir);

    notify_shell_associations_changed();

    if !quiet {
        let done = to_wide(tr(
            "CBX Shell 已成功解除安裝。",
            "CBX Shell has been uninstalled successfully.",
        ));
        let title = to_wide(tr("解除安裝 CBX Shell", "Uninstall CBX Shell"));
        unsafe {
            MessageBoxW(
                HWND::default(),
                PCWSTR(done.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONINFORMATION,
            );
        }
    }

    0
}

// ---------------------------------------------------------------------------
// GUI Setup Wizard
// ---------------------------------------------------------------------------

static INITIAL_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

struct SetupUiState {
    opts: InstallOptions,
    dpi: u32,
    hover_id: Option<i32>,
    pressed_id: Option<i32>,
    mp_detected: bool,
    ff_detected: bool,
    ev_ver: Option<String>,
    ev_installed: bool,
    dark_mode: bool,
    accent_bgr: u32,
    font_title: HFONT,
    font_bold: HFONT,
    font_regular: HFONT,
    font_small: HFONT,
    edit_brush: HBRUSH,
}

#[derive(Clone, Copy)]
struct SetupTheme {
    win_bg: u32,
    card_bg: u32,
    card_border: u32,
    input_bg: u32,
    input_border: u32,
    hover_row: u32,
    chk_fill: u32,
    chk_border: u32,
    chk_hover_fill: u32,
    chk_hover_border: u32,
    text_primary: u32,
    text_secondary: u32,
    text_muted: u32,
    btn_bg: u32,
    btn_border: u32,
    btn_text: u32,
    btn_hover_bg: u32,
    btn_hover_border: u32,
    btn_hover_text: u32,
    btn_press_bg: u32,
    btn_press_border: u32,
    btn_press_text: u32,
    status_ok: u32,
    status_warn: u32,
    status_info: u32,
    warn_banner: u32,
}

impl SetupTheme {
    fn for_mode(dark: bool) -> Self {
        if dark {
            Self {
                win_bg: 0x001C1919,
                card_bg: 0x00262222,
                card_border: 0x00363232,
                input_bg: 0x001B1B1E,
                input_border: 0x00403C3C,
                hover_row: 0x002E2A2A,
                chk_fill: 0x001D1B1B,
                chk_border: 0x005A5555,
                chk_hover_fill: 0x002D2A2A,
                chk_hover_border: 0x008A8585,
                text_primary: 0x00F2F0F0,
                text_secondary: 0x00B8B4B4,
                text_muted: 0x009E9999,
                btn_bg: 0x002E2A2A,
                btn_border: 0x00444040,
                btn_text: 0x00EAE8E8,
                btn_hover_bg: 0x00383434,
                btn_hover_border: 0x00585353,
                btn_hover_text: 0x00FFFFFF,
                btn_press_bg: 0x00201E1E,
                btn_press_border: 0x00454242,
                btn_press_text: 0x00D8D4D4,
                status_ok: 0x0078D888,
                status_warn: 0x0060B8FF,
                status_info: 0x00D8C8B8,
                warn_banner: 0x0044AAFF,
            }
        } else {
            // Windows 11 Fluent Light Mode palette (BGR)
            Self {
                win_bg: 0x00F3F3F3,          // #F3F3F3 Layer / Mica Light
                card_bg: 0x00FFFFFF,         // #FFFFFF CardBackgroundFillColorDefault
                card_border: 0x00E5E5E5,     // #E5E5E5 CardStrokeColorDefault
                input_bg: 0x00FFFFFF,        // #FFFFFF ControlFillColorInputActive
                input_border: 0x00D1D1D1,    // #D1D1D1 ControlStrokeColorDefault
                hover_row: 0x00F0F0F0,       // #F0F0F0 SubtleFillColorSecondary
                chk_fill: 0x00FBFBFB,        // #FBFBFB ControlAltFillColorSecondary
                chk_border: 0x008A8A8A,      // #8A8A8A ControlStrongStrokeColorDefault
                chk_hover_fill: 0x00F0F0F0,  // #F0F0F0
                chk_hover_border: 0x005C5C5C,// #5C5C5C
                text_primary: 0x001A1A1A,    // #1A1A1A TextFillColorPrimary
                text_secondary: 0x005C5C5C,  // #5C5C5C TextFillColorSecondary
                text_muted: 0x006E6E6E,      // #6E6E6E TextFillColorTertiary
                btn_bg: 0x00FBFBFB,          // #FBFBFB ControlFillColorDefault
                btn_border: 0x00DCDCDC,      // #DCDCDC ControlStrokeColorDefault
                btn_text: 0x001A1A1A,        // #1A1A1A
                btn_hover_bg: 0x00F3F3F3,    // #F3F3F3 ControlFillColorSecondary
                btn_hover_border: 0x00CCCCCC,// #CCCCCC
                btn_hover_text: 0x001A1A1A,  // #1A1A1A
                btn_press_bg: 0x00EBEBEB,    // #EBEBEB ControlFillColorTertiary
                btn_press_border: 0x00D6D6D6,// #D6D6D6
                btn_press_text: 0x005C5C5C,  // #5C5C5C
                status_ok: 0x000F7B0F,       // #0F7B0F SystemFillColorSuccess
                status_warn: 0x00005D9D,     // #9D5D00 SystemFillColorCaution
                status_info: 0x005C5C5C,     // #5C5C5C
                warn_banner: 0x00005D9D,     // #9D5D00
            }
        }
    }
}

// UI state lives behind a `Mutex` (previously a `static mut`, whose aliasing references were
// undefined behaviour). All access uses `try_lock`: the window procedure is re-entered from
// modal loops (`MessageBoxW`) and from `SendMessageW` / `DestroyWindow`, and a blocking lock
// would dead-lock the UI thread. Re-entrant calls see `None`; a missed paint is re-queued.
/// Side effects of a WM_COMMAND that must run *after* the state lock is released.
enum SetupAction {
    Install(InstallOptions),
    Close,
}

struct SetupCell(SetupUiState);

// SAFETY: `SetupUiState` holds GDI handles and is only touched on the installer's UI thread;
// `Send` is required solely to store it in a `static Mutex`, which serialises every access.
unsafe impl Send for SetupCell {}

static SETUP_STATE: std::sync::Mutex<Option<SetupCell>> = std::sync::Mutex::new(None);
static SETUP_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static SETUP_REPAINT_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct SetupGuard(std::sync::MutexGuard<'static, Option<SetupCell>>);

impl std::ops::Deref for SetupGuard {
    type Target = SetupUiState;
    fn deref(&self) -> &SetupUiState {
        &self.0.as_ref().expect("setup_lock only returns a guard when state exists").0
    }
}

impl std::ops::DerefMut for SetupGuard {
    fn deref_mut(&mut self) -> &mut SetupUiState {
        &mut self.0.as_mut().expect("setup_lock only returns a guard when state exists").0
    }
}

impl Drop for SetupGuard {
    fn drop(&mut self) {
        if SETUP_REPAINT_PENDING.swap(false, Ordering::AcqRel) {
            let h = SETUP_HWND.load(Ordering::Acquire);
            if h != 0 {
                // SAFETY: plain Win32 call on our own window; it only queues WM_PAINT.
                unsafe {
                    let _ = InvalidateRect(HWND(h as *mut _), None, false);
                }
            }
        }
    }
}

fn setup_lock() -> Option<SetupGuard> {
    let guard = match SETUP_STATE.try_lock() {
        Ok(g) => g,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            SETUP_REPAINT_PENDING.store(true, Ordering::Release);
            return None;
        }
    };
    if guard.is_some() {
        Some(SetupGuard(guard))
    } else {
        None
    }
}

fn setup_install(hwnd: HWND, st: SetupUiState) {
    SETUP_HWND.store(hwnd.0 as isize, Ordering::Release);
    let mut guard = match SETUP_STATE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    *guard = Some(SetupCell(st));
}

fn setup_take() -> Option<SetupUiState> {
    let mut guard = match SETUP_STATE.try_lock() {
        Ok(g) => g,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return None,
    };
    SETUP_HWND.store(0, Ordering::Release);
    guard.take().map(|c| c.0)
}

fn read_windows_accent_bgr() -> u32 {
    let key_w = to_wide(r"Software\Microsoft\Windows\DWM");
    let val_w = to_wide("AccentColor");
    let mut data: u32 = 0;
    let mut cb: u32 = 4;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key_w.as_ptr()),
            PCWSTR(val_w.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut _),
            Some(&mut cb),
        )
    };
    if status.is_ok() && (data & 0x00FF_FFFF) != 0 {
        data & 0x00FF_FFFF
    } else {
        0x00D47800 // Win11 default blue (BGR)
    }
}

#[inline]
fn s_px(px: i32, dpi: u32) -> i32 {
    (px * dpi as i32 + 48) / 96
}

fn run_gui_installer(custom_dir: Option<PathBuf>) -> i32 {
    let _ = INITIAL_DIR.set(custom_dir.unwrap_or_else(default_install_dir));

    let hinst: HINSTANCE = unsafe { GetModuleHandleW(None).unwrap_or_default().into() };
    let class_name = w!("CbxShellInstallerFluentWnd");
    let hicon = create_app_hicon(32);

    let wc = WNDCLASSW {
        lpfnWndProc: Some(setup_wnd_proc),
        hInstance: hinst,
        hIcon: hicon,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
        lpszClassName: class_name,
        ..Default::default()
    };

    unsafe {
        RegisterClassW(&wc);
    }

    let sys_dpi = unsafe { GetDpiForSystem() }.max(96);
    let width = s_px(560, sys_dpi);
    let height = s_px(844, sys_dpi);
    let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let x = ((screen_w - width) / 2).max(0);
    let y = ((screen_h - height) / 2).max(0);

    let title_w = to_wide(tr("CBX Shell 安裝精靈", "CBX Shell Setup"));
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            class_name,
            PCWSTR(title_w.as_ptr()),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            x,
            y,
            width,
            height,
            None,
            None,
            hinst,
            None,
        )
    }
    .expect("CreateWindowExW failed");

    unsafe {
        let dark_mode: i32 = if cbx_core::settings::is_system_dark_mode() { 1 } else { 0 };
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark_mode as *const _ as _,
            4,
        );
        let mica_alt: i32 = 4;
        if DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &mica_alt as *const _ as _,
            4,
        )
        .is_err()
        {
            let mica: i32 = 2;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_SYSTEMBACKDROP_TYPE,
                &mica as *const _ as _,
                4,
            );
        }

        let _ = ShowWindow(hwnd, SW_SHOW);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let ico = hicon;
        if !ico.is_invalid() {
            let _ = DestroyIcon(ico);
        }
    }
    0
}

unsafe fn open_url_in_browser(url: &str) {
    let verb_w = to_wide("open");
    let url_w = to_wide(url);
    let _ = ShellExecuteW(
        None,
        PCWSTR(verb_w.as_ptr()),
        PCWSTR(url_w.as_ptr()),
        None,
        None,
        windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
    );
}

fn companion_ffmpeg_status_text(detected: bool) -> String {
    if detected {
        tr("✓ FFmpeg：已偵測到可用元件", "✓ FFmpeg: Detected and ready").to_string()
    } else {
        tr("⚠️ FFmpeg：未偵測到（建議安裝）", "⚠️ FFmpeg: Not detected (recommended)").to_string()
    }
}

fn companion_everything_status_text(ver: &Option<String>, installed: bool) -> String {
    if let Some(v) = ver {
        match current_lang() {
            UiLang::ZhTw => format!("✓ Everything：運作中 (v{v})"),
            UiLang::En => format!("✓ Everything: Running (v{v})"),
        }
    } else if installed {
        tr(
            "✓ Everything：已安裝（建議常駐開啟）",
            "✓ Everything: Installed (keep running in background)",
        )
        .to_string()
    } else {
        tr(
            "ℹ️ Everything：未偵測到（建議安裝）",
            "ℹ️ Everything: Not detected (recommended)",
        )
        .to_string()
    }
}

struct HitItem {
    id: i32,
    rect: RECT,
}

fn build_hit_items(st: &SetupUiState, client_w: i32) -> Vec<HitItem> {
    let d = st.dpi;
    let margin = s_px(16, d);
    let card_w = client_w - margin * 2;
    let mut items = Vec::new();

    // Card 2: Categories (5 checkboxes)
    let card2_y = s_px(98, d);
    let cat_ids = [
        ID_CB_CAT_ARCHIVE,
        ID_CB_CAT_EBOOK,
        ID_CB_CAT_VIDEO,
        ID_CB_CAT_AUDIO,
        ID_CB_CAT_OTHER,
    ];
    for (i, &id) in cat_ids.iter().enumerate() {
        let iy = card2_y + s_px(38 + (i as i32) * 24, d);
        items.push(HitItem {
            id,
            rect: RECT {
                left: margin + s_px(16, d),
                top: iy,
                right: margin + card_w - s_px(16, d),
                bottom: iy + s_px(22, d),
            },
        });
    }

    // Card 3: Display & Integration Options (8 checkboxes)
    let card3_y = s_px(314, d);
    let opt_ids = [
        ID_CB_BADGE_ANIM,
        ID_CB_BADGE_STATIC,
        ID_CB_ANOMALY,
        ID_CB_HDR_TONEMAP,
        ID_CB_INFOTIP,
        ID_CB_SHORTCUT,
        ID_CB_CLEAR_CACHE,
        ID_CB_LAUNCH_MGR,
    ];
    for (i, &id) in opt_ids.iter().enumerate() {
        let iy = card3_y + s_px(38 + (i as i32) * 24, d);
        items.push(HitItem {
            id,
            rect: RECT {
                left: margin + s_px(16, d),
                top: iy,
                right: margin + card_w - s_px(16, d),
                bottom: iy + s_px(22, d),
            },
        });
    }

    // Card 4: Companion URL buttons
    let card4_y = s_px(562, d) + if st.mp_detected { s_px(22, d) } else { 0 };
    let link_w = s_px(148, d);
    let link_h = s_px(26, d);
    let link_x = margin + card_w - s_px(16, d) - link_w;
    items.push(HitItem {
        id: ID_BTN_FFMPEG_URL,
        rect: RECT {
            left: link_x,
            top: card4_y + s_px(38, d),
            right: link_x + link_w,
            bottom: card4_y + s_px(38, d) + link_h,
        },
    });
    items.push(HitItem {
        id: ID_BTN_EVERYTHING_URL,
        rect: RECT {
            left: link_x,
            top: card4_y + s_px(108, d),
            right: link_x + link_w,
            bottom: card4_y + s_px(108, d) + link_h,
        },
    });

    // Bottom buttons
    let btn_y = card4_y + s_px(188, d);
    let btn_h = s_px(34, d);
    let lang_w = s_px(142, d);
    let act_w = s_px(108, d);
    let gap = s_px(10, d);
    items.push(HitItem {
        id: ID_BTN_LANG,
        rect: RECT {
            left: margin,
            top: btn_y,
            right: margin + lang_w,
            bottom: btn_y + btn_h,
        },
    });
    items.push(HitItem {
        id: ID_BTN_INSTALL,
        rect: RECT {
            left: margin + card_w - act_w * 2 - gap,
            top: btn_y,
            right: margin + card_w - act_w - gap,
            bottom: btn_y + btn_h,
        },
    });
    items.push(HitItem {
        id: ID_BTN_CANCEL,
        rect: RECT {
            left: margin + card_w - act_w,
            top: btn_y,
            right: margin + card_w,
            bottom: btn_y + btn_h,
        },
    });

    items
}

unsafe fn draw_card(hdc: HDC, theme: &SetupTheme, x: i32, y: i32, w: i32, h: i32, r: i32) {
    let bg = CreateSolidBrush(COLORREF(theme.card_bg));
    let pen = CreatePen(PS_SOLID, 1, COLORREF(theme.card_border));
    let ob = SelectObject(hdc, bg);
    let op = SelectObject(hdc, pen);
    let _ = RoundRect(hdc, x, y, x + w, y + h, r, r);
    SelectObject(hdc, ob);
    SelectObject(hdc, op);
    let _ = DeleteObject(bg);
    let _ = DeleteObject(pen);
}

unsafe fn draw_checkbox_row(
    hdc: HDC,
    st: &SetupUiState,
    rect: &RECT,
    label: &str,
    checked: bool,
    hovered: bool,
) {
    let d = st.dpi;
    let theme = SetupTheme::for_mode(st.dark_mode);
    if hovered {
        let hb = CreateSolidBrush(COLORREF(theme.hover_row));
        let hp = CreatePen(PS_SOLID, 1, COLORREF(theme.hover_row));
        let ob = SelectObject(hdc, hb);
        let op = SelectObject(hdc, hp);
        let r = s_px(5, d);
        let _ = RoundRect(hdc, rect.left - s_px(4, d), rect.top, rect.right, rect.bottom, r, r);
        SelectObject(hdc, ob);
        SelectObject(hdc, op);
        let _ = DeleteObject(hb);
        let _ = DeleteObject(hp);
    }

    let cb = s_px(16, d);
    let cx = rect.left;
    let cy = rect.top + (rect.bottom - rect.top - cb) / 2;
    let (fill, border) = if checked {
        (st.accent_bgr, st.accent_bgr)
    } else if hovered {
        (theme.chk_hover_fill, theme.chk_hover_border)
    } else {
        (theme.chk_fill, theme.chk_border)
    };
    let b = CreateSolidBrush(COLORREF(fill));
    let p = CreatePen(PS_SOLID, 1, COLORREF(border));
    let ob = SelectObject(hdc, b);
    let op = SelectObject(hdc, p);
    let _ = RoundRect(hdc, cx, cy, cx + cb, cy + cb, s_px(4, d), s_px(4, d));
    SelectObject(hdc, ob);
    SelectObject(hdc, op);
    let _ = DeleteObject(b);
    let _ = DeleteObject(p);

    if checked {
        let cp = CreatePen(PS_SOLID, s_px(2, d).max(2), COLORREF(0x00FFFFFF));
        let ocp = SelectObject(hdc, cp);
        let _ = MoveToEx(hdc, cx + s_px(3, d), cy + s_px(8, d), None);
        let _ = LineTo(hdc, cx + s_px(7, d), cy + s_px(12, d));
        let _ = LineTo(hdc, cx + s_px(13, d), cy + s_px(4, d));
        SelectObject(hdc, ocp);
        let _ = DeleteObject(cp);
    }

    SelectObject(hdc, st.font_regular);
    SetTextColor(hdc, COLORREF(if checked { theme.text_primary } else { theme.text_secondary }));
    let mut trc = RECT {
        left: cx + cb + s_px(10, d),
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
    };
    let mut lw = to_wide(label);
    let _ = DrawTextW(hdc, &mut lw, &mut trc, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
}

unsafe fn draw_pill_btn(
    hdc: HDC,
    st: &SetupUiState,
    rect: &RECT,
    label: &str,
    primary: bool,
    hovered: bool,
    pressed: bool,
) {
    let d = st.dpi;
    let theme = SetupTheme::for_mode(st.dark_mode);
    let (fill, border, text_col) = if primary {
        if pressed {
            (0x009E5600, 0x009E5600, 0x00FFFFFF)
        } else if hovered {
            (st.accent_bgr, if st.dark_mode { 0x00FFFFFF } else { st.accent_bgr }, 0x00FFFFFF)
        } else {
            (st.accent_bgr, st.accent_bgr, 0x00FFFFFF)
        }
    } else if pressed {
        (theme.btn_press_bg, theme.btn_press_border, theme.btn_press_text)
    } else if hovered {
        (theme.btn_hover_bg, theme.btn_hover_border, theme.btn_hover_text)
    } else {
        (theme.btn_bg, theme.btn_border, theme.btn_text)
    };

    let b = CreateSolidBrush(COLORREF(fill));
    let p = CreatePen(PS_SOLID, 1, COLORREF(border));
    let ob = SelectObject(hdc, b);
    let op = SelectObject(hdc, p);
    let r = s_px(6, d);
    let _ = RoundRect(hdc, rect.left, rect.top, rect.right, rect.bottom, r, r);
    SelectObject(hdc, ob);
    SelectObject(hdc, op);
    let _ = DeleteObject(b);
    let _ = DeleteObject(p);

    SelectObject(hdc, if primary { st.font_bold } else { st.font_regular });
    SetTextColor(hdc, COLORREF(text_col));
    let mut trc = *rect;
    let mut lw = to_wide(label);
    let _ = DrawTextW(hdc, &mut lw, &mut trc, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
}

unsafe fn render_setup_ui(hwnd: HWND, hdc: HDC) {
    let Some(st_guard) = setup_lock() else { return };
    let st: &SetupUiState = &st_guard;
    let mut cr = RECT::default();
    let _ = GetClientRect(hwnd, &mut cr);
    let w = (cr.right - cr.left).max(1);
    let h = (cr.bottom - cr.top).max(1);

    let mem_dc = CreateCompatibleDC(hdc);
    let mem_bmp = CreateCompatibleBitmap(hdc, w, h);
    let old_bmp = SelectObject(mem_dc, mem_bmp);
    SetBkMode(mem_dc, TRANSPARENT);

    let theme = SetupTheme::for_mode(st.dark_mode);
    let bg = CreateSolidBrush(COLORREF(theme.win_bg));
    FillRect(mem_dc, &cr, bg);
    let _ = DeleteObject(bg);

    let d = st.dpi;
    let margin = s_px(16, d);
    let card_w = w - margin * 2;
    let card_r = s_px(10, d);

    // Card 1: Installation Folder
    let card1_y = s_px(14, d);
    let card1_h = s_px(74, d);
    draw_card(mem_dc, &theme, margin, card1_y, card_w, card1_h, card_r);
    SelectObject(mem_dc, st.font_bold);
    SetTextColor(mem_dc, COLORREF(theme.text_primary));
    let mut rc_t1 = RECT {
        left: margin + s_px(16, d),
        top: card1_y + s_px(12, d),
        right: margin + card_w - s_px(16, d),
        bottom: card1_y + s_px(32, d),
    };
    let mut t1 = to_wide(tr("安裝位置", "Installation Folder"));
    let _ = DrawTextW(mem_dc, &mut t1, &mut rc_t1, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX);

    let box_b = CreateSolidBrush(COLORREF(theme.input_bg));
    let box_p = CreatePen(PS_SOLID, 1, COLORREF(theme.input_border));
    let ob = SelectObject(mem_dc, box_b);
    let op = SelectObject(mem_dc, box_p);
    let _ = RoundRect(
        mem_dc,
        margin + s_px(16, d),
        card1_y + s_px(36, d),
        margin + card_w - s_px(16, d),
        card1_y + s_px(64, d),
        s_px(6, d),
        s_px(6, d),
    );
    SelectObject(mem_dc, ob);
    SelectObject(mem_dc, op);
    let _ = DeleteObject(box_b);
    let _ = DeleteObject(box_p);

    // Card 2: File Categories
    let card2_y = s_px(98, d);
    let card2_h = s_px(206, d);
    draw_card(mem_dc, &theme, margin, card2_y, card_w, card2_h, card_r);
    SelectObject(mem_dc, st.font_bold);
    SetTextColor(mem_dc, COLORREF(theme.text_primary));
    let mut rc_t2 = RECT {
        left: margin + s_px(16, d),
        top: card2_y + s_px(12, d),
        right: margin + card_w - s_px(16, d),
        bottom: card2_y + s_px(32, d),
    };
    let mut t2 = to_wide(tr("勾選要啟用縮圖的檔案類別", "Enable Thumbnails For Categories"));
    let _ = DrawTextW(mem_dc, &mut t2, &mut rc_t2, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX);

    let items = build_hit_items(st, w);
    let find_rect = |id: i32| -> RECT {
        items.iter().find(|it| it.id == id).map(|it| it.rect).unwrap_or_default()
    };

    let cat_rows = [
        (
            ID_CB_CAT_ARCHIVE,
            tr(
                "圖片壓縮檔 (CBZ, CBR, CB7, CBT, ZIP, RAR, 7Z, TAR)",
                "Image Archives (CBZ, CBR, CB7, CBT, ZIP, RAR, 7Z, TAR)",
            ),
            st.opts.enable_image_archives,
        ),
        (
            ID_CB_CAT_EBOOK,
            tr(
                "電子書封面 (EPUB, MOBI, IBA, AZW)",
                "E-Book Covers (EPUB, MOBI, IBA, AZW)",
            ),
            st.opts.enable_ebook_formats,
        ),
        (
            ID_CB_CAT_VIDEO,
            tr(
                "影片格式 (MP4, MKV, WebM, AVI, MOV, TS, WMV, FLV...)",
                "Video Formats (MP4, MKV, WebM, AVI, MOV, TS, WMV, FLV...)",
            ),
            st.opts.enable_video_formats,
        ),
        (
            ID_CB_CAT_AUDIO,
            tr(
                "音訊檔案封面 (MP3, FLAC, M4A, OGG, OPUS, WAV, APE...)",
                "Audio Cover Art (MP3, FLAC, M4A, OGG, OPUS, WAV, APE...)",
            ),
            st.opts.enable_audio_formats,
        ),
        (
            ID_CB_CAT_OTHER,
            tr(
                "文件、PSD 與純文字 (DOCX, XLSX, PSD, TXT, MD, TORRENT)",
                "Documents, PSD & Plain Text (DOCX, XLSX, PSD, TXT, MD, TORRENT)",
            ),
            st.opts.enable_other_formats,
        ),
    ];
    for (id, label, checked) in cat_rows {
        let rc = find_rect(id);
        draw_checkbox_row(mem_dc, st, &rc, label, checked, st.hover_id == Some(id));
    }

    SelectObject(mem_dc, st.font_small);
    SetTextColor(mem_dc, COLORREF(theme.text_muted));
    let mut rc_hint = RECT {
        left: margin + s_px(16, d),
        top: card2_y + s_px(164, d),
        right: margin + card_w - s_px(16, d),
        bottom: card2_y + card2_h - s_px(8, d),
    };
    let mut hint_w = to_wide(tr(
        "支援 Windows Media Foundation 硬體加速與安全隔離解碼；語法檔可於控制中心啟用",
        "Supports Media Foundation DXVA & isolated decoding; syntax files configurable in Manager",
    ));
    let _ = DrawTextW(mem_dc, &mut hint_w, &mut rc_hint, DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);

    // Card 3: Display Options & System Integration
    let card3_y = s_px(314, d);
    let card3_h = s_px(238, d) + if st.mp_detected { s_px(22, d) } else { 0 };
    draw_card(mem_dc, &theme, margin, card3_y, card_w, card3_h, card_r);
    SelectObject(mem_dc, st.font_bold);
    SetTextColor(mem_dc, COLORREF(theme.text_primary));
    let mut rc_t3 = RECT {
        left: margin + s_px(16, d),
        top: card3_y + s_px(12, d),
        right: margin + card_w - s_px(16, d),
        bottom: card3_y + s_px(32, d),
    };
    let mut t3 = to_wide(tr("顯示選項與系統整合", "Display Options & System Integration"));
    let _ = DrawTextW(mem_dc, &mut t3, &mut rc_t3, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX);

    let opt_rows = [
        (
            ID_CB_BADGE_ANIM,
            tr(
                "顯示動態圖片角標 (WebP / GIF / APNG)",
                "Show animated image badge (WebP / GIF / APNG)",
            ),
            st.opts.badge_animated,
        ),
        (
            ID_CB_BADGE_STATIC,
            tr(
                "顯示壓縮檔格式角標 (CBZ / ZIP / CBR / RAR / 7Z...)",
                "Show archive format badge (CBZ / ZIP / CBR / RAR / 7Z...)",
            ),
            st.opts.badge_static,
        ),
        (
            ID_CB_ANOMALY,
            tr(
                "啟用壓縮檔安全性異常檢測與風險角標",
                "Enable archive security anomaly detection & risk badge",
            ),
            st.opts.anomaly_detection,
        ),
        (
            ID_CB_HDR_TONEMAP,
            tr(
                "啟用 HDR 影片與圖片 ACES 色調映射",
                "Enable ACES tone mapping for HDR video & images",
            ),
            st.opts.hdr_tonemap,
        ),
        (
            ID_CB_INFOTIP,
            tr(
                "啟用檔案總管滑鼠懸停資訊",
                "Enable Explorer hover info tooltip",
            ),
            st.opts.enable_infotip,
        ),
        (
            ID_CB_SHORTCUT,
            tr("建立開始功能表捷徑", "Create Start Menu shortcut"),
            st.opts.create_shortcut,
        ),
        (
            ID_CB_CLEAR_CACHE,
            tr(
                "安裝後清理並重新整理縮圖快取",
                "Clear thumbnail cache after installation",
            ),
            st.opts.clear_cache,
        ),
        (
            ID_CB_LAUNCH_MGR,
            tr(
                "安裝完成後開啟 CBX Shell 控制中心",
                "Launch CBX Shell Control Center when finished",
            ),
            st.opts.launch_manager,
        ),
    ];
    for (id, label, checked) in opt_rows {
        let rc = find_rect(id);
        draw_checkbox_row(mem_dc, st, &rc, label, checked, st.hover_id == Some(id));
    }

    if st.mp_detected {
        SelectObject(mem_dc, st.font_small);
        SetTextColor(mem_dc, COLORREF(theme.warn_banner));
        let mut rc_mp = RECT {
            left: margin + s_px(16, d),
            top: card3_y + s_px(234, d),
            right: margin + card_w - s_px(16, d),
            bottom: card3_y + card3_h - s_px(6, d),
        };
        let mut mp_w = to_wide(tr(
            "⚠️ 系統檢測到已安裝 Media Preview（功能重疊，建議僅保留其一）",
            "⚠️ Media Preview detected (capabilities overlap; keeping only one is recommended)",
        ));
        let _ = DrawTextW(mem_dc, &mut mp_w, &mut rc_mp, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX);
    }

    // Card 4: Companion Tools Detection
    let card4_y = s_px(562, d) + if st.mp_detected { s_px(22, d) } else { 0 };
    let card4_h = s_px(176, d);
    draw_card(mem_dc, &theme, margin, card4_y, card_w, card4_h, card_r);
    SelectObject(mem_dc, st.font_bold);
    SetTextColor(mem_dc, COLORREF(theme.text_primary));
    let mut rc_t4 = RECT {
        left: margin + s_px(16, d),
        top: card4_y + s_px(12, d),
        right: margin + card_w - s_px(16, d),
        bottom: card4_y + s_px(32, d),
    };
    let mut t4 = to_wide(tr("建議搭配元件檢測", "Companion Tools Detection"));
    let _ = DrawTextW(mem_dc, &mut t4, &mut rc_t4, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX);

    // FFmpeg row
    SelectObject(mem_dc, st.font_bold);
    SetTextColor(mem_dc, COLORREF(if st.ff_detected { theme.status_ok } else { theme.status_warn }));
    let mut rc_ff = RECT {
        left: margin + s_px(16, d),
        top: card4_y + s_px(40, d),
        right: margin + card_w - s_px(174, d),
        bottom: card4_y + s_px(62, d),
    };
    let mut ff_w = to_wide(&companion_ffmpeg_status_text(st.ff_detected));
    let _ = DrawTextW(mem_dc, &mut ff_w, &mut rc_ff, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);

    draw_pill_btn(
        mem_dc,
        st,
        &find_rect(ID_BTN_FFMPEG_URL),
        tr("FFmpeg 官網下載", "FFmpeg Website"),
        false,
        st.hover_id == Some(ID_BTN_FFMPEG_URL),
        st.pressed_id == Some(ID_BTN_FFMPEG_URL),
    );

    SelectObject(mem_dc, st.font_small);
    SetTextColor(mem_dc, COLORREF(theme.text_muted));
    let mut rc_ffd = RECT {
        left: margin + s_px(16, d),
        top: card4_y + s_px(67, d),
        right: margin + card_w - s_px(16, d),
        bottom: card4_y + s_px(102, d),
    };
    let mut ffd_w = to_wide(tr(
        "優點：支援 10-bit H.264/Hi10P、AV1、VP9、ProRes 等 Windows 原生解碼器不支援的影片縮圖擷取。",
        "Benefit: Enables thumbnails for 10-bit H.264/Hi10P, AV1, VP9, ProRes, and other codecs.",
    ));
    let _ = DrawTextW(mem_dc, &mut ffd_w, &mut rc_ffd, DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);

    // Everything row
    SelectObject(mem_dc, st.font_bold);
    SetTextColor(
        mem_dc,
        COLORREF(if st.ev_ver.is_some() || st.ev_installed { theme.status_ok } else { theme.status_info }),
    );
    let mut rc_ev = RECT {
        left: margin + s_px(16, d),
        top: card4_y + s_px(110, d),
        right: margin + card_w - s_px(174, d),
        bottom: card4_y + s_px(132, d),
    };
    let mut ev_w = to_wide(&companion_everything_status_text(&st.ev_ver, st.ev_installed));
    let _ = DrawTextW(mem_dc, &mut ev_w, &mut rc_ev, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);

    draw_pill_btn(
        mem_dc,
        st,
        &find_rect(ID_BTN_EVERYTHING_URL),
        tr("Everything 官網下載", "Everything Website"),
        false,
        st.hover_id == Some(ID_BTN_EVERYTHING_URL),
        st.pressed_id == Some(ID_BTN_EVERYTHING_URL),
    );

    SelectObject(mem_dc, st.font_small);
    SetTextColor(mem_dc, COLORREF(theme.text_muted));
    let mut rc_evd = RECT {
        left: margin + s_px(16, d),
        top: card4_y + s_px(137, d),
        right: margin + card_w - s_px(16, d),
        bottom: card4_y + card4_h - s_px(6, d),
    };
    let mut evd_w = to_wide(tr(
        "優點：檔案總管以隔離串流模式呼叫時，可查詢實體路徑以啟用原生解碼與檔案關聯（僅適用 NTFS 磁碟）。",
        "Benefit: Resolves real file paths in Explorer isolation mode for native decoding (NTFS drives only).",
    ));
    let _ = DrawTextW(mem_dc, &mut evd_w, &mut rc_evd, DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);

    // Bottom Action Buttons
    draw_pill_btn(
        mem_dc,
        st,
        &find_rect(ID_BTN_LANG),
        tr("English", "繁體中文"),
        false,
        st.hover_id == Some(ID_BTN_LANG),
        st.pressed_id == Some(ID_BTN_LANG),
    );
    draw_pill_btn(
        mem_dc,
        st,
        &find_rect(ID_BTN_INSTALL),
        tr("安裝", "Install"),
        true,
        st.hover_id == Some(ID_BTN_INSTALL),
        st.pressed_id == Some(ID_BTN_INSTALL),
    );
    draw_pill_btn(
        mem_dc,
        st,
        &find_rect(ID_BTN_CANCEL),
        tr("取消", "Cancel"),
        false,
        st.hover_id == Some(ID_BTN_CANCEL),
        st.pressed_id == Some(ID_BTN_CANCEL),
    );

    let _ = BitBlt(hdc, 0, 0, w, h, mem_dc, 0, 0, SRCCOPY);
    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(mem_bmp);
    let _ = DeleteDC(mem_dc);
}

unsafe fn create_setup_ui(hwnd: HWND) {
    let dpi = GetDpiForWindow(hwnd).max(96);
    let mk_font = |px: i32, bold: bool| -> HFONT {
        CreateFontW(
            -s_px(px, dpi),
            0,
            0,
            0,
            if bold { FW_BOLD.0 } else { FW_NORMAL.0 } as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            0,
            0,
            CLEARTYPE_QUALITY.0 as u32,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            w!("Microsoft JhengHei UI"),
        )
    };
    let font_title = mk_font(15, true);
    let font_bold = mk_font(13, true);
    let font_regular = mk_font(13, false);
    let font_small = mk_font(11, false);
    let dark_mode = cbx_core::settings::is_system_dark_mode();
    let theme = SetupTheme::for_mode(dark_mode);
    let edit_brush = CreateSolidBrush(COLORREF(theme.input_bg));

    let init_dir = INITIAL_DIR
        .get()
        .cloned()
        .unwrap_or_else(default_install_dir);

    let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
    let mut cr = RECT::default();
    let _ = GetClientRect(hwnd, &mut cr);
    let client_w = (cr.right - cr.left).max(s_px(540, dpi));
    let margin = s_px(16, dpi);
    let card_w = client_w - margin * 2;
    let init_dir_w = to_wide(&init_dir.to_string_lossy());
    let edit_hwnd = CreateWindowExW(
        Default::default(),
        w!("EDIT"),
        PCWSTR(init_dir_w.as_ptr()),
        windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(
            WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32,
        ),
        margin + s_px(24, dpi),
        s_px(14 + 41, dpi),
        card_w - s_px(48, dpi),
        s_px(19, dpi),
        hwnd,
        windows::Win32::UI::WindowsAndMessaging::HMENU(ID_EDIT_DIR as isize as *mut _),
        hinst,
        None,
    )
    .unwrap_or_default();
    SendMessageW(edit_hwnd, WM_SETFONT, WPARAM(font_regular.0 as usize), LPARAM(1));

    setup_install(hwnd, SetupUiState {
        opts: InstallOptions::default_with_dir(init_dir),
        dpi,
        hover_id: None,
        pressed_id: None,
        mp_detected: detect_media_preview_installed(),
        ff_detected: cbx_core::media::video_mf::find_ffmpeg_path().is_some(),
        ev_ver: cbx_core::media::get_everything_version(),
        ev_installed: cbx_core::media::is_everything_installed(),
        dark_mode,
        accent_bgr: read_windows_accent_bgr(),
        font_title,
        font_bold,
        font_regular,
        font_small,
        edit_brush,
    });
}

unsafe extern "system" fn setup_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            create_setup_ui(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_CTLCOLOREDIT => {
            let hdc = HDC(wparam.0 as *mut _);
            if let Some(st_guard) = setup_lock() {
                let st: &SetupUiState = &st_guard;
                let theme = SetupTheme::for_mode(st.dark_mode);
                SetTextColor(hdc, COLORREF(theme.text_primary));
                SetBkColor(hdc, COLORREF(theme.input_bg));
                LRESULT(st.edit_brush.0 as isize)
            } else {
                LRESULT(0)
            }
        }
        0x0320 /* WM_DWMCOLORIZATIONCOLORCHANGED */ | 0x001A /* WM_SETTINGCHANGE */ => {
            if let Some(mut st_guard) = setup_lock() {
                let st: &mut SetupUiState = &mut st_guard;
                let new_dark = cbx_core::settings::is_system_dark_mode();
                st.accent_bgr = read_windows_accent_bgr();
                if new_dark != st.dark_mode {
                    st.dark_mode = new_dark;
                    let theme = SetupTheme::for_mode(new_dark);
                    if !st.edit_brush.is_invalid() {
                        let _ = DeleteObject(st.edit_brush);
                    }
                    st.edit_brush = CreateSolidBrush(COLORREF(theme.input_bg));
                    let dark_val: i32 = if new_dark { 1 } else { 0 };
                    let _ = DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_USE_IMMERSIVE_DARK_MODE,
                        &dark_val as *const _ as _,
                        4,
                    );
                }
                let _ = InvalidateRect(hwnd, None, true);
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            if let Some(st_guard) = setup_lock() {
                let st: &SetupUiState = &st_guard;
                if st.hover_id.is_some() {
                    if let Ok(h) = LoadCursorW(None, IDC_HAND) {
                        SetCursor(h);
                        return LRESULT(1);
                    }
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            if let Some(mut st_guard) = setup_lock() {
                let st: &mut SetupUiState = &mut st_guard;
                let mut cr = RECT::default();
                let _ = GetClientRect(hwnd, &mut cr);
                let pt = POINT { x, y };
                let hit = build_hit_items(st, cr.right - cr.left)
                    .into_iter()
                    .find(|it| pt.x >= it.rect.left && pt.x <= it.rect.right && pt.y >= it.rect.top && pt.y <= it.rect.bottom)
                    .map(|it| it.id);
                if hit != st.hover_id {
                    st.hover_id = hit;
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            if let Some(mut st_guard) = setup_lock() {
                let st: &mut SetupUiState = &mut st_guard;
                let mut cr = RECT::default();
                let _ = GetClientRect(hwnd, &mut cr);
                let hit = build_hit_items(st, cr.right - cr.left)
                    .into_iter()
                    .find(|it| x >= it.rect.left && x <= it.rect.right && y >= it.rect.top && y <= it.rect.bottom)
                    .map(|it| it.id);
                st.pressed_id = hit;
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let mut action_id: Option<i32> = None;
            if let Some(mut st_guard) = setup_lock() {
                let st: &mut SetupUiState = &mut st_guard;
                st.pressed_id = None;
                let mut cr = RECT::default();
                let _ = GetClientRect(hwnd, &mut cr);
                action_id = build_hit_items(st, cr.right - cr.left)
                    .into_iter()
                    .find(|it| x >= it.rect.left && x <= it.rect.right && y >= it.rect.top && y <= it.rect.bottom)
                    .map(|it| it.id);
            }
            if let Some(id) = action_id {
                let _ = SendMessageW(hwnd, WM_COMMAND, WPARAM(id as usize), LPARAM(0));
            }
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            render_setup_ui(hwnd, hdc);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as i32;
            let mut deferred: Option<SetupAction> = None;
            if let Some(mut st_guard) = setup_lock() {
                let st: &mut SetupUiState = &mut st_guard;
                match id {
                    ID_CB_CAT_ARCHIVE => st.opts.enable_image_archives = !st.opts.enable_image_archives,
                    ID_CB_CAT_EBOOK => st.opts.enable_ebook_formats = !st.opts.enable_ebook_formats,
                    ID_CB_CAT_VIDEO => st.opts.enable_video_formats = !st.opts.enable_video_formats,
                    ID_CB_CAT_AUDIO => st.opts.enable_audio_formats = !st.opts.enable_audio_formats,
                    ID_CB_CAT_OTHER => st.opts.enable_other_formats = !st.opts.enable_other_formats,
                    ID_CB_BADGE_ANIM => st.opts.badge_animated = !st.opts.badge_animated,
                    ID_CB_BADGE_STATIC => st.opts.badge_static = !st.opts.badge_static,
                    ID_CB_ANOMALY => st.opts.anomaly_detection = !st.opts.anomaly_detection,
                    ID_CB_HDR_TONEMAP => st.opts.hdr_tonemap = !st.opts.hdr_tonemap,
                    ID_CB_INFOTIP => st.opts.enable_infotip = !st.opts.enable_infotip,
                    ID_CB_SHORTCUT => st.opts.create_shortcut = !st.opts.create_shortcut,
                    ID_CB_CLEAR_CACHE => st.opts.clear_cache = !st.opts.clear_cache,
                    ID_CB_LAUNCH_MGR => st.opts.launch_manager = !st.opts.launch_manager,
                    ID_BTN_LANG => {
                        let next = current_lang().toggle();
                        set_current_lang(next);
                        next.save();
                        let title_w = to_wide(tr("CBX Shell 安裝精靈", "CBX Shell Setup"));
                        let _ = SetWindowTextW(hwnd, PCWSTR(title_w.as_ptr()));
                    }
                    ID_BTN_FFMPEG_URL => {
                        open_url_in_browser("https://ffmpeg.org/download.html");
                        return LRESULT(0);
                    }
                    ID_BTN_EVERYTHING_URL => {
                        open_url_in_browser("https://www.voidtools.com/");
                        return LRESULT(0);
                    }
                    ID_BTN_INSTALL => {
                        let mut dir_str = String::new();
                        if let Ok(edit) = GetDlgItem(hwnd, ID_EDIT_DIR) {
                            let mut buf = [0u16; 520];
                            let len = GetWindowTextW(edit, &mut buf) as usize;
                            dir_str = String::from_utf16_lossy(&buf[..len]).trim().to_string();
                        }
                        if !dir_str.is_empty() {
                            st.opts.install_dir = PathBuf::from(dir_str);
                        }
                        // Run the install (and its MessageBox / DestroyWindow, which re-enter
                        // this window procedure) only after the state lock has been released.
                        deferred = Some(SetupAction::Install(st.opts.clone()));
                    }
                    ID_BTN_CANCEL => {
                        deferred = Some(SetupAction::Close);
                    }
                    _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
                }
                if deferred.is_none() {
                    let _ = InvalidateRect(hwnd, None, false);
                    return LRESULT(0);
                }
            }
            match deferred {
                Some(SetupAction::Install(opts)) => {
                    match perform_install(&opts) {
                        Ok(()) => {
                            let ok_text = match current_lang() {
                                UiLang::ZhTw => format!(
                                    "CBX Shell 已成功安裝！\n\n\
                                     安裝路徑：{}\n\n\
                                     您可隨時從「Windows 設定 > 應用程式 > 已安裝的應用程式」中修改設定或解除安裝。",
                                    opts.install_dir.display()
                                ),
                                UiLang::En => format!(
                                    "CBX Shell has been installed successfully!\n\n\
                                     Location: {}\n\n\
                                     You can modify settings or uninstall anytime from Windows Settings > Apps > Installed apps.",
                                    opts.install_dir.display()
                                ),
                            };
                            let ok_msg = to_wide(&ok_text);
                            let title = to_wide(tr("CBX Shell 安裝精靈", "CBX Shell Setup"));
                            MessageBoxW(
                                hwnd,
                                PCWSTR(ok_msg.as_ptr()),
                                PCWSTR(title.as_ptr()),
                                MB_OK | MB_ICONINFORMATION,
                            );
                            let _ = DestroyWindow(hwnd);
                        }
                        Err(e) => {
                            let err_text = match current_lang() {
                                UiLang::ZhTw => format!("安裝失敗：\n{e}"),
                                UiLang::En => format!("Installation failed:\n{e}"),
                            };
                            let err_msg = to_wide(&err_text);
                            let title = to_wide(tr("CBX Shell 安裝錯誤", "CBX Shell Setup Error"));
                            MessageBoxW(
                                hwnd,
                                PCWSTR(err_msg.as_ptr()),
                                PCWSTR(title.as_ptr()),
                                MB_OK | MB_ICONERROR,
                            );
                        }
                    }
                    LRESULT(0)
                }
                Some(SetupAction::Close) => {
                    let _ = DestroyWindow(hwnd);
                    LRESULT(0)
                }
                None => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
        WM_DESTROY => {
            if let Some(st) = setup_take() {
                for f in [st.font_title, st.font_bold, st.font_regular, st.font_small] {
                    if !f.is_invalid() {
                        let _ = DeleteObject(f);
                    }
                }
                if !st.edit_brush.is_invalid() {
                    let _ = DeleteObject(st.edit_brush);
                }
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
