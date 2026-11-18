#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod render_aa;
mod i18n;
mod update_core;
mod updater;

use cbx_core::icon::create_app_hicon;
use cbx_core::media::{
    extract_embedded_cover, is_audio_extension, is_ebook_extension,
    is_other_or_custom_text_extension, is_video_extension, MediaCategory, MediaFormatInfo,
    SUPPORTED_MEDIA_FORMATS,
};
use cbx_core::registry::{
    apply_execution_mode, associate_extension, clear_thumbnail_cache, disassociate_extension,
    is_associated, notify_shell_associations_changed, repair_all_associations,
};
use cbx_core::settings::{is_media_preview_installed, to_wide, Settings, UiLang};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU8, Ordering};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{ERROR_SUCCESS, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmDefWindowProc, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_SYSTEMBACKDROP_TYPE,
    DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW,
    CreatePen, CreateRectRgn, CreateRoundRectRgn, CreateSolidBrush, DeleteDC, DeleteObject,
    DrawTextW, EndPaint, FillRect, HRGN, InvalidateRect, LineTo, MoveToEx,
    Polygon, RoundRect, SelectClipRgn, SelectObject, SetBkMode, SetStretchBltMode, SetTextColor,
    StretchBlt, TextOutW, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLEARTYPE_QUALITY,
    DEFAULT_CHARSET, DEFAULT_PITCH, DIB_RGB_COLORS, DT_CALCRECT, DT_CENTER, DT_END_ELLIPSIS,
    DT_LEFT, DT_NOPREFIX, DT_PATH_ELLIPSIS, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK,
    DT_EDITCONTROL,
    FF_DONTCARE, FW_NORMAL, HALFTONE, HBITMAP, HDC, HFONT, PAINTSTRUCT,
    PS_SOLID, SRCCOPY, TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    CloseThemeData, DrawThemeTextEx, OpenThemeData, DTTOPTS, DTT_COMPOSITED, DTT_TEXTCOLOR,
    HTHEME, MARGINS,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RegGetValueW, RRF_RT_REG_BINARY, RRF_RT_REG_DWORD,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForWindow, GetSystemMetricsForDpi, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Shell::{
    DragAcceptFiles, DragFinish, DragQueryFileW, HDROP,
    ShellExecuteExW, SHELLEXECUTEINFOW, SEE_MASK_NOCLOSEPROCESS,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, IsZoomed, SetWindowPos, SetWindowTextW,
    NCCALCSIZE_PARAMS, SM_CXPADDEDBORDER, SM_CYCAPTION, SM_CYSIZEFRAME, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    GetMessageW, GetSystemMetrics, KillTimer, LoadCursorW, PostQuitMessage,
    RegisterClassW, SetTimer, ShowWindow, TranslateMessage, SetCursor, IDC_ARROW, IDC_HAND, IDC_SIZENS, MSG,
    SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, WM_CREATE, WM_DESTROY, WM_DROPFILES,
    WM_ERASEBKGND, WM_GETMINMAXINFO, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WM_SIZE, WM_TIMER, WM_CHAR, WM_SETCURSOR,
    MINMAXINFO, WNDCLASSW, WS_CAPTION, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_THICKFRAME,
};

// Non-client messages and hit-test codes used by the custom title bar.
const WM_NCCALCSIZE: u32 = 0x0083;
const WM_KEYDOWN_MSG: u32 = 0x0100;
const WM_NCHITTEST: u32 = 0x0084;
const HTCLIENT: isize = 1;
const HTCAPTION: isize = 2;
const HTTOP: isize = 12;

#[repr(C)]
#[allow(non_snake_case)]
struct OPENFILENAMEW {
    lStructSize: u32,
    hwndOwner: HWND,
    hInstance: HINSTANCE,
    lpstrFilter: PCWSTR,
    lpstrCustomFilter: windows::core::PWSTR,
    nMaxCustFilter: u32,
    nFilterIndex: u32,
    lpstrFile: windows::core::PWSTR,
    nMaxFile: u32,
    lpstrFileTitle: windows::core::PWSTR,
    nMaxFileTitle: u32,
    lpstrInitialDir: PCWSTR,
    lpstrTitle: PCWSTR,
    Flags: u32,
    nFileOffset: u16,
    nFileExtension: u16,
    lpstrDefExt: PCWSTR,
    lCustData: isize,
    lpfnHook: Option<unsafe extern "system" fn() -> usize>,
    lpTemplateName: PCWSTR,
    pvReserved: *mut std::ffi::c_void,
    dwReserved: u32,
    FlagsEx: u32,
}

#[link(name = "comdlg32")]
extern "system" {
    fn GetOpenFileNameW(lpofn: *mut OPENFILENAMEW) -> windows::Win32::Foundation::BOOL;
    fn GetSaveFileNameW(lpofn: *mut OPENFILENAMEW) -> windows::Win32::Foundation::BOOL;
}

/// "Save as" dialog for the activity-log export; returns the chosen path.
unsafe fn prompt_save_log(hwnd: HWND, default_name: &str) -> Option<String> {
    let mut file_buf = [0u16; 1024];
    for (i, c) in default_name.encode_utf16().take(file_buf.len() - 1).enumerate() {
        file_buf[i] = c;
    }
    let filter = "Text (*.txt)\0*.txt\0All Files (*.*)\0*.*\0\0";
    let filter_w: Vec<u16> = filter.encode_utf16().collect();
    let title_w = to_wide(tr("匯出運作紀錄", "Export Activity Log"));
    let def_ext = to_wide("txt");
    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        hInstance: HINSTANCE::default(),
        lpstrFilter: PCWSTR(filter_w.as_ptr()),
        lpstrCustomFilter: windows::core::PWSTR::null(),
        nMaxCustFilter: 0,
        nFilterIndex: 1,
        lpstrFile: windows::core::PWSTR(file_buf.as_mut_ptr()),
        nMaxFile: file_buf.len() as u32,
        lpstrFileTitle: windows::core::PWSTR::null(),
        nMaxFileTitle: 0,
        lpstrInitialDir: PCWSTR::null(),
        lpstrTitle: PCWSTR(title_w.as_ptr()),
        Flags: 0x00000002 | 0x00000800 | 0x00000008, // OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR
        nFileOffset: 0,
        nFileExtension: 0,
        lpstrDefExt: PCWSTR(def_ext.as_ptr()),
        lCustData: 0,
        lpfnHook: None,
        lpTemplateName: PCWSTR::null(),
        pvReserved: std::ptr::null_mut(),
        dwReserved: 0,
        FlagsEx: 0,
    };
    if GetSaveFileNameW(&mut ofn).as_bool() {
        let len = file_buf.iter().position(|&c| c == 0).unwrap_or(file_buf.len());
        if len > 0 {
            return Some(String::from_utf16_lossy(&file_buf[..len]));
        }
    }
    None
}

/// Plain-text activity log for troubleshooting: environment summary, then every entry of the
/// monitor history (newest first) with its pipeline, resolution trace and details.
fn format_activity_log(state: &AppState, snap: &cbx_core::monitor::MonitorSnapshot) -> String {
    use std::fmt::Write as _;
    let st = &state.settings;
    let on_off = |b: bool| if b { "on" } else { "off" };
    let mut out = String::new();
    let _ = writeln!(out, "CBX Shell {APP_VERSION} - activity log");
    let ev = cbx_core::media::get_everything_version().unwrap_or_else(|| "not detected".to_string());
    let _ = writeln!(out, "Everything: {ev}");
    let _ = writeln!(out, "FFmpeg: {}", if st.custom_ffmpeg_path.trim().is_empty() { "auto-detect" } else { "custom path" });
    let _ = writeln!(out, "Network shares: {} (local network only: {})", on_off(st.allow_network_shares), on_off(st.network_lan_only));
    let _ = writeln!(out, "Custom video parameters: {}  watchdog: {} s", on_off(st.custom_video_settings), st.watchdog_timeout_sec);
    let _ = writeln!(
        out,
        "Processed: {}  average: {} ms  active: {}  entries: {}",
        snap.total_processed,
        snap.avg_duration_ms,
        snap.active_tasks,
        snap.history.len()
    );
    if !snap.current_file.is_empty() {
        let _ = writeln!(out, "In progress: {} ({}, {} ms)", snap.current_file, snap.current_stage, snap.current_elapsed_ms);
    }
    let _ = writeln!(out);
    for item in &snap.history {
        let _ = writeln!(
            out,
            "[{}] {:<5} {:>7} ms  {}",
            item.time_str,
            item.ext.trim_start_matches('.').to_uppercase(),
            item.duration_ms,
            if item.success { "OK" } else { "FAIL" }
        );
        let _ = writeln!(out, "  file:     {}", item.file_name);
        let _ = writeln!(out, "  pipeline: {}", item.pipeline);
        let _ = writeln!(out, "  trace:    {}", item.search_trace);
        let _ = writeln!(out, "  details:  {}", item.details.replace('\n', " / "));
        let _ = writeln!(out);
    }
    out
}

/// Export button action: asks for a file and writes the log there (UTF-8 with BOM).
unsafe fn export_activity_log(hwnd: HWND, state: &mut AppState) {
    let Some(snap) = cbx_core::monitor::fetch_snapshot() else {
        state.cache_status_kind = CacheStatusKind::Custom(format!("✗ {}", tr("目前沒有可匯出的運作紀錄", "No activity log to export")));
        state.cache_status_msg = state.get_cache_status_display();
        return;
    };
    let Some(path) = prompt_save_log(hwnd, "cbxshell-activity-log.txt") else {
        return;
    };
    let text = format_activity_log(state, &snap);
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(text.replace('\n', "\r\n").as_bytes());
    state.cache_status_kind = CacheStatusKind::Custom(match std::fs::write(&path, bytes) {
        Ok(()) => format!("✓ {} {}", tr("已匯出運作紀錄：", "Activity log exported:"), path),
        Err(e) => format!("✗ {e}"),
    });
    state.cache_status_msg = state.get_cache_status_display();
}

unsafe fn prompt_select_ffmpeg(hwnd: HWND) -> Option<String> {
    let mut file_buf = [0u16; 1024];
    let filter = "FFmpeg (ffmpeg.exe)\0ffmpeg.exe\0Executables (*.exe)\0*.exe\0All Files (*.*)\0*.*\0\0";
    let filter_w: Vec<u16> = filter.encode_utf16().collect();
    let title_w = to_wide(tr("選擇 FFmpeg 執行檔", "Select FFmpeg Executable"));

    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: hwnd,
        hInstance: HINSTANCE::default(),
        lpstrFilter: PCWSTR(filter_w.as_ptr()),
        lpstrCustomFilter: windows::core::PWSTR::null(),
        nMaxCustFilter: 0,
        nFilterIndex: 1,
        lpstrFile: windows::core::PWSTR(file_buf.as_mut_ptr()),
        nMaxFile: file_buf.len() as u32,
        lpstrFileTitle: windows::core::PWSTR::null(),
        nMaxFileTitle: 0,
        lpstrInitialDir: PCWSTR::null(),
        lpstrTitle: PCWSTR(title_w.as_ptr()),
        Flags: 0x00001000 | 0x00000800 | 0x00000008, // OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR
        nFileOffset: 0,
        nFileExtension: 0,
        lpstrDefExt: PCWSTR::null(),
        lCustData: 0,
        lpfnHook: None,
        lpTemplateName: PCWSTR::null(),
        pvReserved: std::ptr::null_mut(),
        dwReserved: 0,
        FlagsEx: 0,
    };

    let ok = GetOpenFileNameW(&mut ofn);
    if ok.as_bool() {
        let len = file_buf.iter().position(|&c| c == 0).unwrap_or(file_buf.len());
        if len > 0 {
            let path_str = String::from_utf16_lossy(&file_buf[..len]);
            return Some(path_str);
        }
    }
    None
}

#[link(name = "user32")]
extern "system" {
    fn OpenClipboard(hWndNewOwner: HWND) -> windows::Win32::Foundation::BOOL;
    fn CloseClipboard() -> windows::Win32::Foundation::BOOL;
    fn EmptyClipboard() -> windows::Win32::Foundation::BOOL;
    fn SetClipboardData(uFormat: u32, hMem: windows::Win32::Foundation::HANDLE) -> windows::Win32::Foundation::HANDLE;
    fn GetClipboardData(uFormat: u32) -> windows::Win32::Foundation::HANDLE;
}

#[link(name = "imm32")]
extern "system" {
    fn ImmAssociateContext(hwnd: HWND, himc: isize) -> isize;
}

#[link(name = "kernel32")]
extern "system" {
    fn GlobalAlloc(uFlags: u32, dwBytes: usize) -> windows::Win32::Foundation::HGLOBAL;
    fn GlobalLock(hMem: windows::Win32::Foundation::HGLOBAL) -> *mut std::ffi::c_void;
    fn GlobalUnlock(hMem: windows::Win32::Foundation::HGLOBAL) -> windows::Win32::Foundation::BOOL;
}

unsafe fn read_clipboard_text(hwnd: HWND) -> Option<String> {
    if !OpenClipboard(hwnd).as_bool() {
        return None;
    }
    let mut out = None;
    let handle = GetClipboardData(13); // CF_UNICODETEXT
    if !handle.is_invalid() {
        let ptr = GlobalLock(windows::Win32::Foundation::HGLOBAL(handle.0));
        if !ptr.is_null() {
            let wptr = ptr.cast::<u16>();
            let mut len = 0usize;
            while len < 512 && *wptr.add(len) != 0 {
                len += 1;
            }
            out = Some(String::from_utf16_lossy(std::slice::from_raw_parts(wptr, len)));
            let _ = GlobalUnlock(windows::Win32::Foundation::HGLOBAL(handle.0));
        }
    }
    let _ = CloseClipboard();
    out
}

unsafe fn copy_to_clipboard(hwnd: HWND, text: &str) -> bool {
    if !OpenClipboard(hwnd).as_bool() {
        return false;
    }
    let _ = EmptyClipboard();
    let wide = cbx_core::settings::to_wide(text);
    let bytes_len = wide.len() * 2;
    // GHND = GMEM_MOVEABLE (0x0002) | GMEM_ZEROINIT (0x0040)
    let h_mem = GlobalAlloc(0x0042, bytes_len);
    if !h_mem.is_invalid() {
        let ptr = GlobalLock(h_mem);
        if !ptr.is_null() {
            std::ptr::copy_nonoverlapping(wide.as_ptr().cast::<u8>(), ptr.cast::<u8>(), bytes_len);
            let _ = GlobalUnlock(h_mem);
            // CF_UNICODETEXT = 13
            let _ = SetClipboardData(13, windows::Win32::Foundation::HANDLE(h_mem.0));
        }
    }
    let _ = CloseClipboard();
    true
}

static CURRENT_LANG: AtomicU8 = AtomicU8::new(0);
const SIDEBAR_BASE_WIDTH: i32 = 195;
/// Width of the collapsed navigation rail (96-DPI px): 10 px margins around a 40 px pill.
const SIDEBAR_COLLAPSED_WIDTH: i32 = 60;
/// Height of the custom title bar strip (96-DPI px), Task Manager style.
const TITLEBAR_BASE_H: i32 = 48;
/// Navigation-rail toggle ("hamburger") button: top and size (96-DPI px), below the title bar.
const HAMBURGER_Y: i32 = 2;
const HAMBURGER_W: i32 = 40;
const HAMBURGER_H: i32 = 36;
/// Rail collapse / expand animation length (seconds).
const SIDEBAR_ANIM_SECS: f32 = 0.20;
/// Extra left / right inset (96-DPI px) of rows inside an expanded expander card.
const EXPANDER_CONTENT_INDENT: i32 = 16;
/// Language button height (96-DPI px): same as a navigation pill.
const LANG_BTN_H: i32 = 40;
/// Width of the icon column shared by the rail toggle, nav items and the language button, so
/// every glyph is centred on the same vertical line.
const RAIL_ICON_COL_W: i32 = 40;
/// Top of the first navigation item (96-DPI px); items are 44 px tall.
const NAV_START_Y: i32 = 48;
/// Product version, single source of truth: `[workspace.package] version` in Cargo.toml.
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

// Dynamic Windows Personalization > Colors Accent Palette (stored as 0x00RRGGBB)
static ACCENT_BASE_RGB: AtomicU32 = AtomicU32::new(0x000078D4);
static ACCENT_LIGHT1_RGB: AtomicU32 = AtomicU32::new(0x004CA0E0);
static ACCENT_LIGHT2_RGB: AtomicU32 = AtomicU32::new(0x0060CDFF);
static ACCENT_DARK1_RGB: AtomicU32 = AtomicU32::new(0x00005A9E);
static ACCENT_DARK2_RGB: AtomicU32 = AtomicU32::new(0x00004578);

fn lerp_rgb(c1: u32, c2: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let r1 = ((c1 >> 16) & 0xFF) as f32;
    let g1 = ((c1 >> 8) & 0xFF) as f32;
    let b1 = (c1 & 0xFF) as f32;

    let r2 = ((c2 >> 16) & 0xFF) as f32;
    let g2 = ((c2 >> 8) & 0xFF) as f32;
    let b2 = (c2 & 0xFF) as f32;

    let r = (r1 + (r2 - r1) * t).round().clamp(0.0, 255.0) as u32;
    let g = (g1 + (g2 - g1) * t).round().clamp(0.0, 255.0) as u32;
    let b = (b1 + (b2 - b1) * t).round().clamp(0.0, 255.0) as u32;

    (r << 16) | (g << 8) | b
}

#[inline]
fn rgb_to_bgr(rgb: u32) -> u32 {
    let r = (rgb >> 16) & 0xFF;
    let g = (rgb >> 8) & 0xFF;
    let b = rgb & 0xFF;
    (b << 16) | (g << 8) | r
}

#[inline]
fn abgr_dword_to_rgb(abgr: u32) -> u32 {
    let r = abgr & 0xFF;
    let g = (abgr >> 8) & 0xFF;
    let b = (abgr >> 16) & 0xFF;
    (r << 16) | (g << 8) | b
}

fn read_windows_accent_palette() -> (u32, u32, u32, u32, u32) {
    unsafe {
        let mut buf = [0u8; 32];
        let mut size = buf.len() as u32;
        let subkey_accent = to_wide("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent");
        let val_palette = to_wide("AccentPalette");
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey_accent.as_ptr()),
            PCWSTR(val_palette.as_ptr()),
            RRF_RT_REG_BINARY,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        );
        if status == ERROR_SUCCESS && size >= 24 {
            let entry_rgb = |idx: usize| -> u32 {
                let off = idx * 4;
                ((buf[off] as u32) << 16) | ((buf[off + 1] as u32) << 8) | (buf[off + 2] as u32)
            };
            let light2 = entry_rgb(1);
            let light1 = entry_rgb(2);
            let base = entry_rgb(3);
            let dark1 = entry_rgb(4);
            let dark2 = entry_rgb(5);
            return (base, light1, light2, dark1, dark2);
        }

        let subkey_dwm = to_wide("Software\\Microsoft\\Windows\\DWM");
        let val_dwm = to_wide("AccentColor");
        let mut dwm_val: u32 = 0;
        let mut dwm_size: u32 = std::mem::size_of::<u32>() as u32;
        if RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey_dwm.as_ptr()),
            PCWSTR(val_dwm.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut dwm_val as *mut u32).cast()),
            Some(&mut dwm_size),
        ) == ERROR_SUCCESS
        {
            let base = abgr_dword_to_rgb(dwm_val);
            return (
                base,
                lerp_rgb(base, 0x00FFFFFF, 0.28),
                lerp_rgb(base, 0x00FFFFFF, 0.45),
                lerp_rgb(base, 0x00000000, 0.18),
                lerp_rgb(base, 0x00000000, 0.34),
            );
        }
    }
    let base = 0x000078D4;
    (base, 0x004CA0E0, 0x0060CDFF, 0x00005A9E, 0x00004578)
}

pub(crate) fn refresh_system_accent_palette() {
    let (base, light1, light2, dark1, dark2) = read_windows_accent_palette();
    ACCENT_BASE_RGB.store(base, Ordering::Relaxed);
    ACCENT_LIGHT1_RGB.store(light1, Ordering::Relaxed);
    ACCENT_LIGHT2_RGB.store(light2, Ordering::Relaxed);
    ACCENT_DARK1_RGB.store(dark1, Ordering::Relaxed);
    ACCENT_DARK2_RGB.store(dark2, Ordering::Relaxed);
}

#[inline]
pub(crate) fn accent_base_rgb() -> u32 {
    ACCENT_BASE_RGB.load(Ordering::Relaxed)
}

#[inline]
pub(crate) fn accent_base_bgr() -> u32 {
    rgb_to_bgr(accent_base_rgb())
}

#[inline]
pub(crate) fn accent_checkbox_hover_rgb() -> u32 {
    lerp_rgb(accent_base_rgb(), 0x00000000, 0.16)
}

#[inline]
pub(crate) fn accent_hover_bgr() -> u32 {
    rgb_to_bgr(lerp_rgb(accent_base_rgb(), 0x00FFFFFF, 0.14))
}

#[inline]
pub(crate) fn accent_pressed_bgr() -> u32 {
    rgb_to_bgr(ACCENT_DARK1_RGB.load(Ordering::Relaxed))
}

static SYSTEM_DARK_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

#[inline]
pub(crate) fn is_dark_theme() -> bool {
    SYSTEM_DARK_MODE.load(Ordering::Relaxed)
}

pub(crate) fn refresh_system_theme(hwnd: Option<HWND>) {
    let dark = cbx_core::settings::is_system_dark_mode();
    SYSTEM_DARK_MODE.store(dark, Ordering::Relaxed);
    refresh_system_accent_palette();
    if let Some(h) = hwnd {
        unsafe {
            let dark_mode: i32 = if dark { 1 } else { 0 };
            let _ = DwmSetWindowAttribute(
                h,
                DWMWA_USE_IMMERSIVE_DARK_MODE,
                &dark_mode as *const _ as _,
                4,
            );
        }
    }
}

// -----------------------------------------------------------------------------
// Win11 frosted-glass (Acrylic) backdrop
// -----------------------------------------------------------------------------
//
// The navigation rail and the content panel's rounded corner are painted with alpha = 0 so
// that DWM shows the window backdrop there. If no blur backdrop is actually active (older
// Windows builds, API failure, "Transparency effects" turned off) those pixels would be fully
// see-through and show the desktop/windows behind *unblurred*. `apply_frosted_backdrop`
// therefore tries, in order:
//   1. Windows 11 22H2+ : DWMWA_SYSTEMBACKDROP_TYPE = DWMSBT_MAINWINDOW (Mica: a heavily blurred,
//      tinted copy of the desktop wallpaper that stays fixed relative to the screen)
//   2. Windows 11 21H2  : the original DWMWA_MICA_EFFECT attribute (1029)
//   3. Windows 10 1803+ : SetWindowCompositionAttribute acrylic blur-behind
// and records whether one succeeded. When none did, `render_fluent_ui` flattens every
// translucent pixel onto an opaque Mica-like base colour, so the window never becomes clear glass.

/// DWMSBT_MAINWINDOW (Mica). 3 = Acrylic (blurs whatever is behind), 4 = Mica Alt.
const FROSTED_BACKDROP_TYPE: i32 = 2;
/// Pre-22H2 Windows 11 Mica switch (undocumented but stable in build 22000).
const DWMWA_MICA_EFFECT_21H2: u32 = 1029;
/// Opacity of the page background (`win_bg`) laid over the Mica backdrop outside the cards, so
/// the content area shows the same wallpaper glow as the rail, only deeper.
const CONTENT_MICA_TINT_ALPHA: u32 = 0xB4;

static BACKDROP_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[inline]
fn is_backdrop_active() -> bool {
    BACKDROP_ACTIVE.load(Ordering::Acquire)
}

/// Real OS build number (`RtlGetVersion` is not affected by manifest version shims).
fn windows_build_number() -> u32 {
    #[repr(C)]
    struct OsVersionInfoW {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }
    type RtlGetVersionFn = unsafe extern "system" fn(*mut OsVersionInfoW) -> i32;
    // SAFETY: ntdll is always loaded; the function pointer type matches RtlGetVersion's ABI and
    // the out-structure is correctly sized and initialised.
    unsafe {
        let Ok(ntdll) = GetModuleHandleW(w!("ntdll.dll")) else {
            return 0;
        };
        let Some(proc) =
            windows::Win32::System::LibraryLoader::GetProcAddress(ntdll, windows::core::s!("RtlGetVersion"))
        else {
            return 0;
        };
        let f: RtlGetVersionFn = std::mem::transmute(proc);
        let mut info = OsVersionInfoW {
            size: std::mem::size_of::<OsVersionInfoW>() as u32,
            major: 0,
            minor: 0,
            build: 0,
            platform: 0,
            csd: [0; 128],
        };
        if f(&mut info) == 0 {
            info.build
        } else {
            0
        }
    }
}

/// `false` when the user switched off Settings > Personalization > Colors > Transparency effects.
fn transparency_effects_enabled() -> bool {
    let mut val: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: valid key/value names and a correctly sized DWORD buffer.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("EnableTransparency"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut val as *mut u32).cast()),
            Some(&mut size),
        )
    };
    status != ERROR_SUCCESS || val != 0
}

/// Undocumented but long-stable `SetWindowCompositionAttribute(WCA_ACCENT_POLICY)` acrylic blur,
/// used only where the documented system backdrop API does not exist.
unsafe fn apply_legacy_acrylic(hwnd: HWND, dark: bool) -> bool {
    #[repr(C)]
    struct AccentPolicy {
        accent_state: u32,
        accent_flags: u32,
        gradient_color: u32, // 0xAABBGGRR tint
        animation_id: u32,
    }
    #[repr(C)]
    struct WinCompAttrData {
        attrib: u32,
        pv_data: *mut std::ffi::c_void,
        cb_data: usize,
    }
    type SwcaFn = unsafe extern "system" fn(HWND, *mut WinCompAttrData) -> windows::Win32::Foundation::BOOL;
    const WCA_ACCENT_POLICY: u32 = 19;
    const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

    let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else {
        return false;
    };
    let Some(proc) = windows::Win32::System::LibraryLoader::GetProcAddress(
        user32,
        windows::core::s!("SetWindowCompositionAttribute"),
    ) else {
        return false;
    };
    let f: SwcaFn = std::mem::transmute(proc);
    // Win11-like tints: dark #202020 / light #F3F3F3 at ~80 % opacity.
    let tint = if dark { 0xCC20_2020 } else { 0xCCF3_F3F3 };
    let mut policy = AccentPolicy {
        accent_state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
        accent_flags: 0,
        gradient_color: tint,
        animation_id: 0,
    };
    let mut data = WinCompAttrData {
        attrib: WCA_ACCENT_POLICY,
        pv_data: (&mut policy as *mut AccentPolicy).cast(),
        cb_data: std::mem::size_of::<AccentPolicy>(),
    };
    f(hwnd, &mut data).as_bool()
}

/// Applies the best available frosted backdrop and records whether one is active.
fn apply_frosted_backdrop(hwnd: HWND) {
    let active = if !transparency_effects_enabled() {
        false
    } else {
        let build = windows_build_number();
        // SAFETY: Win32/DWM calls on our own top-level window with correctly sized arguments.
        unsafe {
            if build >= 22621
                && DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_SYSTEMBACKDROP_TYPE,
                    &FROSTED_BACKDROP_TYPE as *const _ as _,
                    4,
                )
                .is_ok()
            {
                true
            } else if build >= 22000
                && DwmSetWindowAttribute(
                    hwnd,
                    windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(DWMWA_MICA_EFFECT_21H2 as i32),
                    &1i32 as *const _ as _,
                    4,
                )
                .is_ok()
            {
                true
            } else if build >= 17134 {
                apply_legacy_acrylic(hwnd, is_dark_theme())
            } else {
                false
            }
        }
    };
    BACKDROP_ACTIVE.store(active, Ordering::Release);
}

#[derive(Clone, Copy)]
pub(crate) struct ThemePalette {
    pub dark: bool,
    pub win_bg: u32,
    pub sidebar_bg: u32,
    pub sidebar_hover_bg: u32,
    pub card_bg: u32,
    pub card_border: u32,
    pub sub_card_bg: u32,
    pub sub_card_border: u32,
    pub divider: u32,
    pub input_bg: u32,
    pub input_border: u32,
    pub text_primary: u32,
    pub text_bright: u32,
    pub text_secondary: u32,
    pub text_muted: u32,
    pub text_dim: u32,
    pub text_disabled: u32,
    pub btn_bg: u32,
    pub btn_hover_bg: u32,
    pub btn_hover_border: u32,
    pub btn_border: u32,
    pub btn_disabled_bg: u32,
    pub btn_disabled_border: u32,
    pub sel_unselected_bg: u32,
    pub sel_unselected_hover_bg: u32,
    pub sel_unselected_border: u32,
    pub sel_unselected_text: u32,
    pub status_ok: u32,
    pub status_warn: u32,
    pub status_err: u32,
    pub caution_bg: u32,
    pub caution_border: u32,
    pub caution_text: u32,
    pub modal_backdrop: u32,
    pub table_header_bg: u32,
    pub table_row_alt_bg: u32,
    pub scrollbar_track: u32,
    pub scrollbar_thumb: u32,
    pub accent_text: u32,
}

#[inline]
pub(crate) fn theme() -> ThemePalette {
    if is_dark_theme() {
        ThemePalette {
            dark: true,
            win_bg: 0x001C1919,
            sidebar_bg: 0x001B1B1E,
            sidebar_hover_bg: 0x002B2B2E,
            card_bg: 0x00242020,
            card_border: 0x00332D2D,
            sub_card_bg: 0x001B1717,
            sub_card_border: 0x002F2828,
            divider: 0x002F2828,
            input_bg: 0x001E1B1B,
            input_border: 0x00454040,
            text_primary: 0x00F0F0F0,
            text_bright: 0x00FFFFFF,
            text_secondary: 0x00D0D0D0,
            text_muted: 0x00A0A0A0,
            text_dim: 0x00888080,
            text_disabled: 0x00888888,
            btn_bg: 0x00202022,
            btn_hover_bg: 0x002B2B2E,
            btn_hover_border: 0x00666666,
            btn_border: 0x003D3D44,
            btn_disabled_bg: 0x0036363B,
            btn_disabled_border: 0x004E4E56,
            sel_unselected_bg: 0x00202022,
            sel_unselected_hover_bg: 0x002B2B2E,
            sel_unselected_border: 0x003D3D44,
            sel_unselected_text: 0x00D0D0D0,
            status_ok: 0x006CCB5F,
            status_warn: 0x0033B8FF,
            status_err: 0x005555EE,
            caution_bg: 0x00182C3D,
            caution_border: 0x0030649E,
            caution_text: 0x009CE0FF,
            modal_backdrop: 0x00141212,
            table_header_bg: 0x002B2626,
            table_row_alt_bg: 0x001F1B1B,
            scrollbar_track: 0x00201C1C,
            scrollbar_thumb: 0x004A4444,
            accent_text: accent_light_bgr(),
        }
    } else {
        let dark1 = ACCENT_DARK1_RGB.load(Ordering::Relaxed);
        ThemePalette {
            dark: false,
            win_bg: 0x00F3F3F3,
            sidebar_bg: 0x00ECECEC,
            sidebar_hover_bg: 0x00E0E0E0,
            card_bg: 0x00FFFFFF,
            card_border: 0x00E5E5E5,
            sub_card_bg: 0x00F9F9F9,
            sub_card_border: 0x00E5E5E5,
            divider: 0x00E5E5E5,
            input_bg: 0x00FFFFFF,
            input_border: 0x00CCCCCC,
            text_primary: 0x001A1A1A,
            text_bright: 0x00111111,
            text_secondary: 0x003A3A3A,
            text_muted: 0x005C5C5C,
            text_dim: 0x00757575,
            text_disabled: 0x009E9E9E,
            btn_bg: 0x00FBFBFB,
            btn_hover_bg: 0x00F2F2F2,
            btn_hover_border: 0x00BDBDBD,
            btn_border: 0x00DCDCDC,
            btn_disabled_bg: 0x00F3F3F3,
            btn_disabled_border: 0x00E0E0E0,
            sel_unselected_bg: 0x00FBFBFB,
            sel_unselected_hover_bg: 0x00F0F0F0,
            sel_unselected_border: 0x00DCDCDC,
            sel_unselected_text: 0x001A1A1A,
            status_ok: 0x000F7B0F,
            status_warn: 0x00005D9D,
            status_err: 0x001C2BC4,
            caution_bg: 0x00CEF4FF,
            caution_border: 0x0000B8E6,
            caution_text: 0x00004466,
            modal_backdrop: 0x00E6E6E6,
            table_header_bg: 0x00F0F0F0,
            table_row_alt_bg: 0x00F7F7F7,
            scrollbar_track: 0x00EBEBEB,
            scrollbar_thumb: 0x00B8B8B8,
            accent_text: rgb_to_bgr(dark1),
        }
    }
}

#[inline]
pub(crate) fn accent_light_bgr() -> u32 {
    if !is_dark_theme() {
        return rgb_to_bgr(ACCENT_DARK1_RGB.load(Ordering::Relaxed));
    }
    let light1 = ACCENT_LIGHT1_RGB.load(Ordering::Relaxed);
    let r = (light1 >> 16) & 0xFF;
    let g = (light1 >> 8) & 0xFF;
    let b = light1 & 0xFF;
    let lum = (299 * r + 587 * g + 114 * b) / 1000;
    let rgb = if lum < 120 {
        ACCENT_LIGHT2_RGB.load(Ordering::Relaxed)
    } else {
        light1
    };
    rgb_to_bgr(rgb)
}

#[inline]
pub(crate) fn accent_subtle_bg_bgr() -> u32 {
    if is_dark_theme() {
        rgb_to_bgr(lerp_rgb(0x00202020, accent_base_rgb(), 0.22))
    } else {
        rgb_to_bgr(lerp_rgb(0x00FFFFFF, accent_base_rgb(), 0.12))
    }
}

#[inline]
pub(crate) fn accent_contrast_text_rgb() -> u32 {
    let rgb = accent_base_rgb();
    let r = (rgb >> 16) & 0xFF;
    let g = (rgb >> 8) & 0xFF;
    let b = rgb & 0xFF;
    let yiq = (299 * r + 587 * g + 114 * b) / 1000;
    if yiq >= 150 { 0x00111111 } else { 0x00FFFFFF }
}

#[inline]
pub(crate) fn accent_contrast_text_bgr() -> u32 {
    rgb_to_bgr(accent_contrast_text_rgb())
}

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
    i18n::init_locale(lang);
}

#[derive(Debug, Clone)]
enum CacheStatusKind {
    Idle,
    Cleared(usize, u64),
    PreFill,
    Repaired(usize),
    ElevatedSuccess(usize),
    ElevatedUserOnly,
    Custom(String),
}

#[derive(Debug, Clone)]
enum LastTestResult {
    None,
    Video {
        fname: String,
        elapsed_ms: u32,
        attempts: u32,
        time_sec: f64,
        duration_sec: f64,
        orig_w: u32,
        orig_h: u32,
        out_w: u32,
        out_h: u32,
        lum: f32,
        entropy: f32,
        score: f32,
        is_accepted: bool,
    },
    Audio {
        fname: String,
        elapsed_ms: u32,
        has_cover: bool,
        cover_len: usize,
    },
    Archive {
        fname: String,
        elapsed_ms: u32,
        entry_name: String,
        out_w: u32,
        out_h: u32,
        fmt: String,
        is_simple: bool,
    },
    Ebook {
        fname: String,
        elapsed_ms: u32,
        entry_name: String,
        out_w: u32,
        out_h: u32,
        fmt: String,
    },
    Other {
        fname: String,
        elapsed_ms: u32,
        entry_name: String,
        out_w: u32,
        out_h: u32,
        fmt: String,
    },
    Error {
        fname: String,
        err_msg: String,
    },
}

#[derive(Debug, Clone, Copy)]
struct AvailableLocale {
    id: &'static str,
    display_name: &'static str,
    lang: UiLang,
}

const AVAILABLE_LOCALES: &[AvailableLocale] = &[
    AvailableLocale { id: "zh-TW", display_name: "繁體中文", lang: UiLang::ZhTw },
    AvailableLocale { id: "en-US", display_name: "English", lang: UiLang::En },
];

fn tr(zh: &'static str, en: &'static str) -> &'static str {
    match current_lang() {
        UiLang::ZhTw => zh,
        UiLang::En => en,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagerTab {
    Archives = 0,
    Ebook = 1,
    Video = 2,
    Audio = 3,
    Other = 4,
    Advanced = 5,
    About = 6,
}

impl ManagerTab {
    fn from_index(i: usize) -> Self {
        match i {
            0 => Self::Archives,
            1 => Self::Ebook,
            2 => Self::Video,
            3 => Self::Audio,
            4 => Self::Other,
            5 => Self::Advanced,
            _ => Self::About,
        }
    }

    fn to_index(&self) -> usize {
        *self as usize
    }

    fn title(&self) -> (&'static str, &'static str) {
        match self {
            Self::Archives => ("壓縮檔", "Archives"),
            Self::Ebook => ("電子書", "E-Books"),
            Self::Video => ("影片", "Video"),
            Self::Audio => ("音訊", "Audio"),
            Self::Other => ("其他", "Other"),
            Self::Advanced => ("進階", "Advanced"),
            Self::About => ("關於", "About"),
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            Self::Archives => "📚",
            Self::Ebook => "📖",
            Self::Video => "🎬",
            Self::Audio => "🎵",
            Self::Other => "📄",
            Self::Advanced => "⚙️",
            Self::About => "ℹ️",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorFilter {
    All = 0,
    Success = 1,
    Slow = 2,
    InterruptedOrSkipped = 3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorPipelineFilter {
    All = 0,
    DirectFile = 1,
    IpcHit = 2,
    SearchHit = 3,
    StreamRpc = 4,
}

/// Geometry of a painted, enabled text box.
#[derive(Clone, Copy)]
struct EditBoxGeom {
    target: EditTarget,
    rect: RECT,
    text_left: i32,
    text_right: i32,
    font: HFONT,
}

// user32 functions outside the enabled windows-rs feature set.
#[link(name = "user32")]
extern "system" {
    fn GetKeyState(vkey: i32) -> i16;
    fn SetCapture(hwnd: HWND) -> HWND;
    fn ReleaseCapture() -> windows::Win32::Foundation::BOOL;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditTarget {
    SeekFixedSec,
    SeekThresholdMin,
    StepProbeSec,
    MaxGopHops,
    ConsecutiveLimit,
    WatchdogTimeoutSec,
    CustomTextExtensions,
}

struct AppState {
    active_tab: ManagerTab,
    hover_tab: Option<usize>,
    settings: Settings,
    dpi: u32,

    // Fonts
    font_hero: HFONT,
    font_title: HFONT,
    font_bold: HFONT,
    font_regular: HFONT,
    font_small: HFONT,
    font_chevron: HFONT,

    // Mouse position & interactive hover tracking
    mouse_pos: (i32, i32),
    active_edit: Option<EditTarget>,
    /// Screen rectangle of the focused text box's clear ("X") button, recorded while painting.
    edit_clear_rect: std::cell::Cell<Option<RECT>>,
    edit_clear_pressed: bool,
    edit_buffer: String,
    /// Caret and selection anchor in `edit_buffer` (byte offsets; the buffer is ASCII-only).
    edit_caret: usize,
    edit_anchor: usize,
    /// Mouse-drag text selection in progress.
    edit_dragging: bool,
    /// Text boxes painted in the last frame (screen coordinates), for caret hit-testing.
    edit_boxes: std::cell::RefCell<Vec<EditBoxGeom>>,
    has_unapplied_text_edits: bool,
    applied_custom_timer: Option<std::time::Instant>,

    // Test tab live inspection state
    test_path: Option<PathBuf>,
    test_metrics_str: String,
    last_test_result: LastTestResult,
    test_thumb: Option<HBITMAP>,
    test_thumb_dims: (u32, u32),

    // Cache operations status
    cache_status_msg: String,
    cache_status_kind: CacheStatusKind,
    cache_include_network: bool,
    cache_force_refresh: bool,
    /// Folder thumbnail pre-generation (Advanced › 填充縮圖快取 / folder drop).
    prewarm: std::sync::Arc<cbx_core::prewarm::PrewarmProgress>,
    prewarm_size: u32,
    /// Show the pre-generation status instead of the cache status line.
    prewarm_shown: bool,
    prewarm_last_done: usize,
    prewarm_was_active: bool,

    // Pending extension associations
    pending_associations: HashMap<String, bool>,

    // Mouse slider drag tracking
    dragging_slider: Option<usize>, // 0: seek, 1: lum_low, 2: lum_high, 3: ent_low

    // Smooth switch animations
    toggle_anims: HashMap<u32, f32>,
    toggle_targets: HashMap<u32, f32>,

    // Real-time thumbnail monitor snapshot & scroll position
    monitor_snapshot: Option<cbx_core::monitor::MonitorSnapshot>,
    monitor_scroll: usize,
    /// Minutes left before unlocked logging switches off (as last painted).
    monitor_left_min: Option<u64>,
    monitor_filter: MonitorFilter,
    monitor_pipeline_filter: MonitorPipelineFilter,
    open_monitor_dropdown: Option<u8>,
    pressed_monitor_dropdown: Option<u8>,
    hover_monitor_dropdown_item: Option<usize>,
    monitor_dropdown_anchor: std::cell::Cell<(i32, i32, i32, i32)>, // (pipe_x, status_x, metrics_y, dpi_h)
    monitor_dd_anim: f32,
    monitor_dd_anim_start: Option<std::time::Instant>,
    hovered_monitor_row: Option<usize>,
    expanded_monitor_item: Option<cbx_core::monitor::SnapshotHistoryItem>,
    copied_path_timer: Option<std::time::Instant>,
    copied_details_timer: Option<std::time::Instant>,

    // Button tactile press and click-wave animation state
    pressed_btn: Option<u32>,
    btn_flash_anims: HashMap<u32, (f32, bool)>,

    // Vertical content scroll state
    scroll_y: i32,
    max_scroll_y: i32,
    is_dragging_scrollbar: bool,
    drag_scrollbar_start_y: i32,
    drag_scroll_start_val: i32,

    // Win11 Navigation Indicator & Page Entrance Transition
    indicator_y: f32,
    indicator_target_y: f32,
    indicator_h: f32,
    indicator_from_y: f32,
    indicator_anim_start: Option<std::time::Instant>,
    page_trans_start: Option<std::time::Instant>,

    // Resizable Monitor Log Window
    monitor_card_h: i32,
    is_dragging_resizer: bool,
    drag_resizer_start_y: i32,
    drag_resizer_start_h: i32,

    about_card1_h: std::cell::Cell<i32>,
    /// Height of the drag & drop experiment card, grown to fit wrapped result text.
    drop_card_h: std::cell::Cell<i32>,
    video_card_tools_h: std::cell::Cell<i32>,
    video_everything_btn_rel_y: std::cell::Cell<i32>,
    /// Top of the "network shares" sub-card, relative to the tools card (layout feedback).
    video_net_sub_rel_y: std::cell::Cell<i32>,
    hover_slider: Option<usize>,

    // Win11 Language Flyout Menu state
    is_lang_menu_open: bool,
    hover_lang_item: Option<usize>,
    /// Flyout item under a pressed (not yet released) mouse button; selection happens on release.
    pressed_lang_item: Option<usize>,
    pressed_monitor_dd_item: Option<usize>,
    /// Start of the current flyout item press (drives the selected-indicator squash).
    flyout_press_start: Option<std::time::Instant>,
    lang_btn_pressed: bool,
    /// Navigation rail collapsed to icons only (target state).
    sidebar_collapsed: bool,
    /// Eased collapse progress: 0 = expanded, 1 = collapsed.
    sidebar_anim: f32,
    sidebar_anim_from: f32,
    sidebar_anim_start: Option<std::time::Instant>,
    hamburger_pressed: bool,
    lang_menu_anim: f32,
    lang_menu_anim_start: Option<std::time::Instant>,
    pressed_tab: Option<usize>,
    pressed_tab_anim: f32,
    pressed_tab_anim_start: Option<std::time::Instant>,
    pressed_expander: Option<u8>,
    pressed_expander_anim: f32,
    pressed_expander_anim_start: Option<std::time::Instant>,

    // Win11 Expander Card for Security Risk Badges
    is_risk_card_expanded: bool,
    risk_card_expand_anim: f32,
    risk_card_anim_start: Option<(std::time::Instant, bool)>,
    is_risk_header_hovered: bool,

    // Live Activity Monitor collapse/expand animation
    monitor_expand_anim: f32,
    monitor_anim_start: Option<(std::time::Instant, bool)>,

    // Archive natural sort continuous-sequence sub-card expander
    is_archive_sort_expanded: bool,
    archive_sort_expand_anim: f32,
    archive_sort_anim_start: Option<(std::time::Instant, bool)>,
    is_sort_header_hovered: bool,

    // Video custom algorithm parameters collapsible sub-card
    is_video_params_expanded: bool,
    video_params_expand_anim: f32,
    video_params_anim_start: Option<(std::time::Instant, bool)>,
    is_video_params_header_hovered: bool,
    /// "Everything search helper" expander card on the Video page.
    is_ev_expanded: bool,
    ev_expand_anim: f32,
    ev_anim_start: Option<(std::time::Instant, bool)>,
    is_ev_header_hovered: bool,
    /// Top of the Everything card relative to the FFmpeg card (layout feedback).
    video_ev_card_rel_y: std::cell::Cell<i32>,

    // CheckBox left-to-right checkmark draw animation state
    checkbox_states: std::cell::RefCell<HashMap<u32, bool>>,
    checkbox_anims: std::cell::RefCell<HashMap<u32, (f32, std::time::Instant)>>,
    checkbox_anim_hwnd: HWND,

    // Cached Everything IPC version so WM_PAINT doesn't query IPC every frame
    cached_everything_version: std::cell::RefCell<Option<Option<String>>>,
}

impl AppState {
    fn new(dpi: u32) -> Self {
        refresh_system_accent_palette();
        let mut settings = Settings::load();
        // Unlocked logging that expired while the manager was closed is switched off for real.
        if settings.monitor_enabled && !settings.monitor_effective(unix_now()) {
            settings.monitor_enabled = false;
            settings.monitor_locked = false;
            let _ = settings.save();
        }
        // Keep HKCU UserChoiceLatest ProgId & TypeOverlay="" synced on launch for active formats
        {
            let hkcu = windows::Win32::System::Registry::HKEY_CURRENT_USER;
            for fmt in cbx_core::media::SUPPORTED_MEDIA_FORMATS {
                if cbx_core::registry::is_associated(hkcu, fmt.ext) {
                    let enable_tip = settings.enable_infotip && fmt.category == MediaCategory::ImageArchive;
                    cbx_core::registry::associate_extension(hkcu, fmt.ext, enable_tip);
                }
            }
            for ext in settings.parsed_custom_text_extensions() {
                if cbx_core::registry::is_associated(hkcu, &ext) {
                    cbx_core::registry::associate_extension(hkcu, &ext, false);
                }
            }
        }
        let scale = |pt: i32| -> i32 { -((pt * dpi as i32) / 72) };

        let font_hero = create_gdi_font(scale(18), 600);
        let font_title = create_gdi_font(scale(14), 600);
        let font_bold = create_gdi_font(scale(11), 600);
        let font_regular = create_gdi_font(scale(10), FW_NORMAL.0 as i32);
        let font_small = create_gdi_font(scale(9), FW_NORMAL.0 as i32);
        let font_chevron = create_chevron_font(scale(10));

        let item_h = scale_px(44, dpi) as f32;
        let start_y = scale_px(NAV_START_Y, dpi) as f32;
        let init_y = start_y + item_h / 2.0;
        let init_h = scale_px(20, dpi) as f32;
        let mon_init_anim = if settings.monitor_enabled { 1.0 } else { 0.0 };

        Self {
            active_tab: ManagerTab::Archives,
            hover_tab: None,
            settings,
            dpi,
            font_hero,
            font_title,
            font_bold,
            font_regular,
            font_small,
            font_chevron,
            mouse_pos: (0, 0),
            active_edit: None,
            edit_clear_rect: std::cell::Cell::new(None),
            edit_clear_pressed: false,
            edit_buffer: String::new(),
            edit_caret: 0,
            edit_anchor: 0,
            edit_dragging: false,
            edit_boxes: std::cell::RefCell::new(Vec::new()),
            has_unapplied_text_edits: false,
            applied_custom_timer: None,
            test_path: None,
            test_metrics_str: String::new(),
            last_test_result: LastTestResult::None,
            test_thumb: None,
            test_thumb_dims: (0, 0),
            cache_status_msg: String::from(tr("快取就緒", "Cache is idle and ready")),
            cache_status_kind: CacheStatusKind::Idle,
            cache_include_network: false,
            cache_force_refresh: false,
            prewarm: Default::default(),
            prewarm_size: 256,
            prewarm_shown: false,
            prewarm_last_done: 0,
            prewarm_was_active: false,
            pending_associations: HashMap::new(),
            dragging_slider: None,
            toggle_anims: HashMap::new(),
            toggle_targets: HashMap::new(),
            monitor_snapshot: cbx_core::monitor::fetch_snapshot(),
            monitor_scroll: 0,
            monitor_left_min: None,
            monitor_filter: MonitorFilter::All,
            monitor_pipeline_filter: MonitorPipelineFilter::All,
            open_monitor_dropdown: None,
            pressed_monitor_dropdown: None,
            hover_monitor_dropdown_item: None,
            monitor_dropdown_anchor: std::cell::Cell::new((0, 0, 0, 0)),
            monitor_dd_anim: 0.0,
            monitor_dd_anim_start: None,
            hovered_monitor_row: None,
            expanded_monitor_item: None,
            copied_path_timer: None,
            copied_details_timer: None,
            pressed_btn: None,
            btn_flash_anims: HashMap::new(),
            scroll_y: 0,
            max_scroll_y: 0,
            is_dragging_scrollbar: false,
            drag_scrollbar_start_y: 0,
            drag_scroll_start_val: 0,

            indicator_y: init_y,
            indicator_target_y: init_y,
            indicator_h: init_h,
            indicator_from_y: init_y,
            indicator_anim_start: None,
            page_trans_start: None,

            monitor_card_h: scale_px(290, dpi),
            is_dragging_resizer: false,
            drag_resizer_start_y: 0,
            drag_resizer_start_h: 0,

            about_card1_h: std::cell::Cell::new(scale_px(190, dpi)),
            drop_card_h: std::cell::Cell::new(scale_px(175, dpi)),
            video_card_tools_h: std::cell::Cell::new(scale_px(210, dpi)),
            video_everything_btn_rel_y: std::cell::Cell::new(scale_px(120, dpi)),
            video_net_sub_rel_y: std::cell::Cell::new(0),
            hover_slider: None,

            is_lang_menu_open: false,
            hover_lang_item: None,
            pressed_lang_item: None,
            pressed_monitor_dd_item: None,
            flyout_press_start: None,
            lang_btn_pressed: false,
            sidebar_collapsed: false,
            sidebar_anim: 0.0,
            sidebar_anim_from: 0.0,
            sidebar_anim_start: None,
            hamburger_pressed: false,
            lang_menu_anim: 0.0,
            lang_menu_anim_start: None,
            pressed_tab: None,
            pressed_tab_anim: 0.0,
            pressed_tab_anim_start: None,
            pressed_expander: None,
            pressed_expander_anim: 0.0,
            pressed_expander_anim_start: None,

            is_risk_card_expanded: false,
            risk_card_expand_anim: 0.0,
            risk_card_anim_start: None,
            is_risk_header_hovered: false,

            monitor_expand_anim: mon_init_anim,
            monitor_anim_start: None,

            is_archive_sort_expanded: false,
            archive_sort_expand_anim: 0.0,
            archive_sort_anim_start: None,
            is_sort_header_hovered: false,

            is_video_params_expanded: false,
            video_params_expand_anim: 0.0,
            video_params_anim_start: None,
            is_video_params_header_hovered: false,
            is_ev_expanded: false,
            ev_expand_anim: 0.0,
            ev_anim_start: None,
            is_ev_header_hovered: false,
            video_ev_card_rel_y: std::cell::Cell::new(0),

            checkbox_states: std::cell::RefCell::new(HashMap::new()),
            checkbox_anims: std::cell::RefCell::new(HashMap::new()),
            checkbox_anim_hwnd: HWND::default(),

            cached_everything_version: std::cell::RefCell::new(None),
        }
    }

    fn edit_fix(&self, i: usize) -> usize {
        let mut i = i.min(self.edit_buffer.len());
        while i > 0 && !self.edit_buffer.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    /// Ordered selection range (start, end); equal when there is only a caret.
    fn edit_sel(&self) -> (usize, usize) {
        let a = self.edit_fix(self.edit_anchor);
        let c = self.edit_fix(self.edit_caret);
        (a.min(c), a.max(c))
    }

    fn edit_caret_to_end(&mut self) {
        self.edit_caret = self.edit_buffer.len();
        self.edit_anchor = self.edit_caret;
    }

    fn edit_select_all_text(&mut self) {
        self.edit_anchor = 0;
        self.edit_caret = self.edit_buffer.len();
    }

    fn edit_delete_selection(&mut self) -> bool {
        let (s, e) = self.edit_sel();
        if s < e {
            self.edit_buffer.replace_range(s..e, "");
            self.edit_caret = s;
            self.edit_anchor = s;
            true
        } else {
            false
        }
    }

    /// Types `c` at the caret, replacing the selection.
    fn edit_insert(&mut self, c: char, max_len: usize) {
        self.edit_delete_selection();
        let at = self.edit_fix(self.edit_caret);
        if self.edit_buffer.len() + c.len_utf8() <= max_len {
            self.edit_buffer.insert(at, c);
            self.edit_caret = at + c.len_utf8();
        } else {
            self.edit_caret = at;
        }
        self.edit_anchor = self.edit_caret;
    }

    unsafe fn toggle_sidebar(&mut self, hwnd: HWND) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        self.sidebar_anim_from = self.sidebar_anim;
        self.sidebar_anim_start = Some(std::time::Instant::now());
        self.is_lang_menu_open = false;
        self.lang_menu_anim_start = None;
        let _ = SetTimer(hwnd, 1, 16, None);
        let _ = InvalidateRect(hwnd, None, false);
    }

    fn switch_tab(&mut self, hwnd: HWND, new_tab: ManagerTab) {
        if self.active_tab == new_tab {
            return;
        }
        if self.active_edit.is_some() || self.has_unapplied_text_edits {
            self.active_edit = None;
            self.edit_buffer.clear();
            self.edit_caret_to_end();
            self.has_unapplied_text_edits = false;
            let def = Settings::default();
            match self.active_tab {
                ManagerTab::Other => {
                    self.settings.custom_text_extensions = def.custom_text_extensions;
                }
                ManagerTab::Video => {
                    self.settings.seek_fixed_sec = def.seek_fixed_sec;
                    self.settings.seek_threshold_min = def.seek_threshold_min;
                    self.settings.step_probe_sec = def.step_probe_sec;
                    self.settings.max_gop_hops = def.max_gop_hops;
                    self.settings.consecutive_unacceptable_limit = def.consecutive_unacceptable_limit;
                    self.settings.watchdog_timeout_sec = def.watchdog_timeout_sec;
                }
                _ => {
                    self.settings.custom_text_extensions = def.custom_text_extensions;
                    self.settings.seek_fixed_sec = def.seek_fixed_sec;
                    self.settings.seek_threshold_min = def.seek_threshold_min;
                    self.settings.step_probe_sec = def.step_probe_sec;
                    self.settings.max_gop_hops = def.max_gop_hops;
                    self.settings.consecutive_unacceptable_limit = def.consecutive_unacceptable_limit;
                    self.settings.watchdog_timeout_sec = def.watchdog_timeout_sec;
                }
            }
            let _ = self.settings.save();
        }
        self.is_risk_header_hovered = false;
        self.is_sort_header_hovered = false;
        self.is_video_params_header_hovered = false;
        self.is_ev_header_hovered = false;
        self.active_tab = new_tab;
        self.scroll_y = 0;
        let item_h = scale_px(44, self.dpi) as f32;
        let start_y = scale_px(NAV_START_Y, self.dpi) as f32;
        let target_y = start_y + (new_tab.to_index() as f32 * item_h) + (item_h / 2.0);
        self.indicator_from_y = self.indicator_y;
        self.indicator_target_y = target_y;
        self.indicator_anim_start = Some(std::time::Instant::now());
        self.page_trans_start = Some(std::time::Instant::now());
        let _ = unsafe { SetTimer(hwnd, 1, 16, None) };
        let _ = unsafe { InvalidateRect(hwnd, None, false) };
    }

    fn trigger_button_anim(&mut self, hwnd: HWND, id: u32, is_accent: bool) {
        self.pressed_btn = Some(id);
        self.btn_flash_anims.insert(id, (1.0, is_accent));
        let _ = unsafe { SetTimer(hwnd, 1, 16, None) };
        let _ = unsafe { InvalidateRect(hwnd, None, false) };
    }

    fn get_toggle_anim(&self, id: u32, is_on: bool) -> f32 {
        *self.toggle_anims.get(&id).unwrap_or(&(if is_on { 1.0 } else { 0.0 }))
    }

    fn get_cache_status_display(&self) -> String {
        match &self.cache_status_kind {
            CacheStatusKind::Idle => i18n::t("cache.ready"),
            CacheStatusKind::Cleared(count, bytes) => {
                format!("{} ({} {}, {} MB)", i18n::t("cache.cleared"), count, i18n::t("cache.files"), bytes / (1024 * 1024))
            }
            CacheStatusKind::PreFill => {
                format!(
                    "{} ({}: {}, {}: {})",
                    tr("已觸發縮圖快取填充", "Cache pre-fill triggered"),
                    tr("網路設備", "Network"),
                    if self.cache_include_network { tr("是", "Yes") } else { tr("否", "No") },
                    tr("強制更新", "Force"),
                    if self.cache_force_refresh { tr("是", "Yes") } else { tr("否", "No") }
                )
            }
            CacheStatusKind::Repaired(repaired) => {
                format!("{} ({} {})", i18n::t("cache.repaired"), repaired, tr("格式", "formats"))
            }
            CacheStatusKind::ElevatedSuccess(count) => {
                format!("🛡️ {} ({} {})", tr("已全域套用副檔名關聯 (HKLM & HKCU)", "Global associations applied (HKLM & HKCU)"), count, tr("格式", "formats"))
            }
            CacheStatusKind::ElevatedUserOnly => {
                format!("⚠️ {}", tr("未獲管理員權限，已僅套用於目前使用者 (HKCU)", "UAC cancelled, applied to Current User only"))
            }
            CacheStatusKind::Custom(s) => s.clone(),
        }
    }

    fn get_test_metrics_display(&self) -> String {
        match &self.last_test_result {
            LastTestResult::None => {
                if !self.test_metrics_str.is_empty() {
                    self.test_metrics_str.clone()
                } else {
                    String::new()
                }
            }
            LastTestResult::Video { fname, elapsed_ms, attempts, time_sec, duration_sec, orig_w, orig_h, out_w, out_h, lum, entropy, score, is_accepted } => {
                format!(
                    "{}{}\n{}{} ms ({:.2} {}) | {}: {} {}\n{}{:.1} {} / {} {:.1} {}\n{}{}x{} -> {}: {}x{}\n{}{:.1}% | {}{:.1}% | {}{:.1}\n{}: {}",
                    i18n::t("experiment.video_probe"), fname,
                    i18n::t("experiment.latency"), elapsed_ms, *elapsed_ms as f64 / 1000.0, i18n::t("experiment.seconds"),
                    i18n::t("experiment.attempts"), attempts, i18n::t("experiment.times"),
                    i18n::t("experiment.pos"), time_sec, i18n::t("experiment.seconds"), i18n::t("experiment.total_length"), duration_sec, i18n::t("experiment.seconds"),
                    i18n::t("experiment.res"), orig_w, orig_h, i18n::t("experiment.output"), out_w, out_h,
                    i18n::t("experiment.metrics"), lum, i18n::t("experiment.entropy"), entropy, i18n::t("experiment.score"), score,
                    tr("狀態", "Status"), if *is_accepted { i18n::t("experiment.accepted") } else { i18n::t("experiment.fallback") }
                )
            }
            LastTestResult::Audio { fname, elapsed_ms, has_cover, cover_len } => {
                if *has_cover {
                    format!(
                        "{}{}\n{}{} ms\n{}{} bytes",
                        i18n::t("experiment.audio_probe"), fname,
                        i18n::t("experiment.latency"), elapsed_ms,
                        i18n::t("experiment.audio_cover_size"), cover_len
                    )
                } else {
                    format!(
                        "🎵 {}: {}\n{}{} ms\n{}",
                        tr("音訊檔案", "Audio file"), fname,
                        i18n::t("experiment.latency"), elapsed_ms,
                        i18n::t("experiment.no_cover_vinyl")
                    )
                }
            }
            LastTestResult::Archive { fname, elapsed_ms, entry_name, out_w, out_h, fmt, is_simple } => {
                format!(
                    "{}{}\n{}{} ms ({:.2} {})\n{}{}\n{}{}x{} | {}{}\n{}{}",
                    i18n::t("experiment.archive_probe"), fname,
                    i18n::t("experiment.latency"), elapsed_ms, *elapsed_ms as f64 / 1000.0, i18n::t("experiment.seconds"),
                    i18n::t("experiment.cover_entry"), entry_name,
                    i18n::t("experiment.dimensions"), out_w, out_h, i18n::t("experiment.format"), fmt,
                    i18n::t("experiment.complexity"), if *is_simple { i18n::t("experiment.too_simple") } else { i18n::t("experiment.detailed") }
                )
            }
            LastTestResult::Ebook { fname, elapsed_ms, entry_name, out_w, out_h, fmt } => {
                format!(
                    "📖 {}: {}\n{}{} ms ({:.2} {})\n{}{}\n{}{}x{} | {}{}",
                    tr("電子書解析", "E-Book"), fname,
                    i18n::t("experiment.latency"), elapsed_ms, *elapsed_ms as f64 / 1000.0, i18n::t("experiment.seconds"),
                    i18n::t("experiment.cover_entry"), entry_name,
                    i18n::t("experiment.dimensions"), out_w, out_h, i18n::t("experiment.format"), fmt
                )
            }
            LastTestResult::Other { fname, elapsed_ms, entry_name, out_w, out_h, fmt } => {
                format!(
                    "📄 {}: {}\n{}{} ms ({:.2} {})\n{}{}\n{}{}x{} | {}{}",
                    tr("文件與設計檔", "Document"), fname,
                    i18n::t("experiment.latency"), elapsed_ms, *elapsed_ms as f64 / 1000.0, i18n::t("experiment.seconds"),
                    tr("來源模式: ", "Mode: "), entry_name,
                    i18n::t("experiment.dimensions"), out_w, out_h, i18n::t("experiment.format"), fmt
                )
            }
            LastTestResult::Error { fname, err_msg } => {
                format!("{}{}\n{}{}", i18n::t("experiment.failed"), fname, i18n::t("experiment.error"), err_msg)
            }
        }
    }

    fn step_animations(&mut self) -> (bool, bool) {
        let mut active = false;
        let mut non_lang_active = false;

        // 1. Navigation indicator animation (Win11 squash & stretch + spring ease-out)
        if let Some(start_t) = self.indicator_anim_start {
            let elapsed = start_t.elapsed().as_secs_f32();
            let duration = 0.22; // 220ms
            if elapsed < duration {
                active = true;
                non_lang_active = true;
                let p = (elapsed / duration).clamp(0.0, 1.0);
                // Ease out cubic
                let t = 1.0 - (1.0 - p).powi(3);
                self.indicator_y = self.indicator_from_y + (self.indicator_target_y - self.indicator_from_y) * t;

                // Squash (shrink) in the middle, then restore to full length
                let base_h = scale_px(20, self.dpi) as f32;
                let shrink = (p * std::f32::consts::PI).sin() * 0.45; // shrinks up to 45%
                self.indicator_h = base_h * (1.0 - shrink);
            } else {
                self.indicator_y = self.indicator_target_y;
                self.indicator_h = scale_px(20, self.dpi) as f32;
                self.indicator_anim_start = None;
            }
        }

        // 1b. Navigation rail collapse / expand (width interpolation, ease-out cubic)
        if let Some(start_t) = self.sidebar_anim_start {
            let p = (start_t.elapsed().as_secs_f32() / SIDEBAR_ANIM_SECS).clamp(0.0, 1.0);
            let target = if self.sidebar_collapsed { 1.0 } else { 0.0 };
            let t = 1.0 - (1.0 - p).powi(3);
            self.sidebar_anim = self.sidebar_anim_from + (target - self.sidebar_anim_from) * t;
            if p < 1.0 {
                active = true;
                non_lang_active = true;
            } else {
                self.sidebar_anim = target;
                self.sidebar_anim_start = None;
            }
        }

        // 2. Page transition animation (Win11 slide up fade-in)
        if let Some(start_t) = self.page_trans_start {
            let elapsed = start_t.elapsed().as_secs_f32();
            if elapsed < 0.20 {
                active = true;
                non_lang_active = true;
            } else {
                self.page_trans_start = None;
            }
        }

        // 3. Toggle switch animations
        for (&id, &target) in self.toggle_targets.iter() {
            let cur = self.toggle_anims.entry(id).or_insert(target);
            let diff = target - *cur;
            if diff.abs() > 0.03 {
                *cur += diff * 0.35;
                active = true;
                non_lang_active = true;
            } else {
                *cur = target;
            }
        }

        // 4. Button press feedback fade-out
        let mut finished = Vec::new();
        for (&id, (val, _)) in self.btn_flash_anims.iter_mut() {
            if *val > 0.05 {
                *val *= 0.70;
                active = true;
                non_lang_active = true;
            } else {
                finished.push(id);
            }
        }
        for id in finished {
            self.btn_flash_anims.remove(&id);
        }

        // 5. Security Risk Expander Card animation (Win11 collapsible expand/collapse)
        if let Some((start_t, target_expanded)) = self.risk_card_anim_start {
            let elapsed = start_t.elapsed().as_secs_f32();
            let duration = 0.18; // 180ms ease-out
            let target = if target_expanded { 1.0 } else { 0.0 };
            let from = if target_expanded { 0.0 } else { 1.0 };
            if elapsed < duration {
                active = true;
                non_lang_active = true;
                let p = (elapsed / duration).clamp(0.0, 1.0);
                let t = 1.0 - (1.0 - p).powi(3);
                self.risk_card_expand_anim = from + (target - from) * t;
            } else {
                self.risk_card_expand_anim = target;
                self.risk_card_anim_start = None;
            }
        }

        // 6. Language Flyout Menu upward extend animation (accelerated 105ms quintic ease-out)
        if let Some(start_t) = self.lang_menu_anim_start {
            let elapsed = start_t.elapsed().as_secs_f32();
            let duration = 0.105; // 105ms snappy ease-out
            if elapsed < duration {
                active = true;
                let p = (elapsed / duration).clamp(0.0, 1.0);
                let t = 1.0 - (1.0 - p).powi(4);
                self.lang_menu_anim = t;
            } else {
                self.lang_menu_anim = 1.0;
                self.lang_menu_anim_start = None;
            }
        }

        // 6a. Flyout item press: selected-indicator squash
        if let Some(start_t) = self.flyout_press_start {
            if start_t.elapsed().as_secs_f32() < FLYOUT_PRESS_SECS {
                active = true;
                non_lang_active = true;
            }
        }

        // 6b. Monitor Filter Flyout Menu extend animation (same 105ms quartic ease-out as Language Switcher)
        if let Some(start_t) = self.monitor_dd_anim_start {
            let elapsed = start_t.elapsed().as_secs_f32();
            let duration = 0.105;
            if elapsed < duration {
                active = true;
                non_lang_active = true;
                let p = (elapsed / duration).clamp(0.0, 1.0);
                self.monitor_dd_anim = 1.0 - (1.0 - p).powi(4);
            } else {
                self.monitor_dd_anim = 1.0;
                self.monitor_dd_anim_start = None;
            }
        }

        if let Some(start_t) = self.pressed_tab_anim_start {
            let elapsed = start_t.elapsed().as_secs_f32();
            let duration = 0.14;
            if elapsed < duration {
                active = true;
                let p = (elapsed / duration).clamp(0.0, 1.0);
                self.pressed_tab_anim = 1.0 - (1.0 - p).powi(3);
            } else {
                self.pressed_tab_anim = 1.0;
                self.pressed_tab_anim_start = None;
            }
        }

        if let Some(start_t) = self.pressed_expander_anim_start {
            let elapsed = start_t.elapsed().as_secs_f32();
            let duration = 0.14;
            if elapsed < duration {
                active = true;
                non_lang_active = true;
                let p = (elapsed / duration).clamp(0.0, 1.0);
                self.pressed_expander_anim = 1.0 - (1.0 - p).powi(3);
            } else {
                self.pressed_expander_anim = 1.0;
                self.pressed_expander_anim_start = None;
            }
        }

        // 7. Live Activity Monitor collapse/expand animation
        if let Some((start_t, target_expanded)) = self.monitor_anim_start {
            let elapsed = start_t.elapsed().as_secs_f32();
            let duration = 0.20; // 200ms ease-out
            let target = if target_expanded { 1.0 } else { 0.0 };
            let from = if target_expanded { 0.0 } else { 1.0 };
            if elapsed < duration {
                active = true;
                non_lang_active = true;
                let p = (elapsed / duration).clamp(0.0, 1.0);
                let t = 1.0 - (1.0 - p).powi(3);
                self.monitor_expand_anim = from + (target - from) * t;
            } else {
                self.monitor_expand_anim = target;
                self.monitor_anim_start = None;
            }
        }

        // 8. Archive continuous-sequence sub-card animation
        if let Some((start_t, target_expanded)) = self.archive_sort_anim_start {
            let elapsed = start_t.elapsed().as_secs_f32();
            let duration = 0.16;
            let target = if target_expanded { 1.0 } else { 0.0 };
            let from = if target_expanded { 0.0 } else { 1.0 };
            if elapsed < duration {
                active = true;
                non_lang_active = true;
                let p = (elapsed / duration).clamp(0.0, 1.0);
                let t = 1.0 - (1.0 - p).powi(3);
                self.archive_sort_expand_anim = from + (target - from) * t;
            } else {
                self.archive_sort_expand_anim = target;
                self.archive_sort_anim_start = None;
            }
        }

        // 9. Video custom parameters collapsible sub-card animation
        if let Some((start_t, target_expanded)) = self.video_params_anim_start {
            let elapsed = start_t.elapsed().as_secs_f32();
            let duration = 0.18;
            let target = if target_expanded { 1.0 } else { 0.0 };
            let from = if target_expanded { 0.0 } else { 1.0 };
            if elapsed < duration {
                active = true;
                non_lang_active = true;
                let p = (elapsed / duration).clamp(0.0, 1.0);
                let t = 1.0 - (1.0 - p).powi(3);
                self.video_params_expand_anim = from + (target - from) * t;
            } else {
                self.video_params_expand_anim = target;
                self.video_params_anim_start = None;
            }
        }

        // 9b. Everything expander card
        if let Some((start_t, target_expanded)) = self.ev_anim_start {
            let p = (start_t.elapsed().as_secs_f32() / 0.18).clamp(0.0, 1.0);
            let (from, target) = if target_expanded { (0.0, 1.0) } else { (1.0, 0.0) };
            if p < 1.0 {
                active = true;
                non_lang_active = true;
                let t = 1.0 - (1.0 - p).powi(3);
                self.ev_expand_anim = from + (target - from) * t;
            } else {
                self.ev_expand_anim = target;
                self.ev_anim_start = None;
            }
        }

        // 10. CheckBox left-to-right checkmark draw animation
        {
            let mut cb_anims = self.checkbox_anims.borrow_mut();
            let mut cb_done = Vec::new();
            for (&id, (prog, start_t)) in cb_anims.iter_mut() {
                let elapsed = start_t.elapsed().as_secs_f32();
                let duration = 0.18;
                if elapsed < duration {
                    let p = (elapsed / duration).clamp(0.0, 1.0);
                    *prog = 1.0 - (1.0 - p).powi(3);
                    active = true;
                    non_lang_active = true;
                } else {
                    *prog = 1.0;
                    cb_done.push(id);
                }
            }
            for id in cb_done {
                cb_anims.remove(&id);
            }
        }

        (active, non_lang_active)
    }
}

fn archive_sort_card_height(dpi: u32, sort_anim: f32) -> i32 {
    scale_px(52, dpi) + ((scale_px(48, dpi) as f32) * sort_anim.clamp(0.0, 1.0)).round() as i32
}

fn video_card4_height(dpi: u32, params_anim: f32) -> i32 {
    scale_px(52, dpi) + ((scale_px(591, dpi) as f32) * params_anim.clamp(0.0, 1.0)).round() as i32
}

// -----------------------------------------------------------------------------
// UI state storage
// -----------------------------------------------------------------------------
//
// The whole window state lives behind a `Mutex` (previously a `static mut`, whose aliasing
// `&mut` references were undefined behaviour). Every access uses `try_lock`, never `lock`:
// Win32 re-enters the window procedure from modal loops (`GetOpenFileNameW`, `MessageBoxW`)
// and from `SendMessage`, and a blocking lock would dead-lock the UI thread. A re-entrant call
// simply gets `None`; if it was a paint, a repaint is scheduled for when the outer access ends.

/// Wrapper that lets `AppState` (which holds GDI handles and `RefCell`s) live in a `static`.
struct UiState(AppState);

// SAFETY: `AppState` is created, used and destroyed exclusively on the UI thread that owns the
// main window; no other thread ever calls `state_lock`. `Send` is only required so the value can
// be stored in a `static Mutex`, and the mutex itself serialises every access.
unsafe impl Send for UiState {}

static STATE: std::sync::Mutex<Option<UiState>> = std::sync::Mutex::new(None);
static MAIN_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static REPAINT_PENDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Exclusive access to the window state for the duration of one message handler.
struct StateGuard(std::sync::MutexGuard<'static, Option<UiState>>);

impl std::ops::Deref for StateGuard {
    type Target = AppState;
    fn deref(&self) -> &AppState {
        &self.0.as_ref().expect("state_lock only returns a guard when state exists").0
    }
}

impl std::ops::DerefMut for StateGuard {
    fn deref_mut(&mut self) -> &mut AppState {
        &mut self.0.as_mut().expect("state_lock only returns a guard when state exists").0
    }
}

impl Drop for StateGuard {
    fn drop(&mut self) {
        // A paint (or timer) arrived while this guard was held; repaint now that we're done.
        // `InvalidateRect` only queues WM_PAINT, so calling it while still holding the lock is safe.
        if REPAINT_PENDING.swap(false, Ordering::AcqRel) {
            let h = MAIN_HWND.load(Ordering::Acquire);
            if h != 0 {
                // SAFETY: plain Win32 call on our own window handle.
                unsafe {
                    let _ = InvalidateRect(HWND(h as *mut _), None, false);
                }
            }
        }
    }
}

fn state_lock() -> Option<StateGuard> {
    let guard = match STATE.try_lock() {
        Ok(g) => g,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            REPAINT_PENDING.store(true, Ordering::Release);
            return None;
        }
    };
    if guard.is_some() {
        Some(StateGuard(guard))
    } else {
        None
    }
}

fn state_install(hwnd: HWND, app: AppState) {
    MAIN_HWND.store(hwnd.0 as isize, Ordering::Release);
    let mut guard = match STATE.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    *guard = Some(UiState(app));
}

fn state_take() -> Option<AppState> {
    let mut guard = match STATE.try_lock() {
        Ok(g) => g,
        Err(std::sync::TryLockError::Poisoned(p)) => p.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return None,
    };
    MAIN_HWND.store(0, Ordering::Release);
    guard.take().map(|u| u.0)
}

fn create_gdi_font(height: i32, weight: i32) -> HFONT {
    let font_name = w!("Segoe UI Variable Text");
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            0,
            0,
            CLEARTYPE_QUALITY.0 as u32,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            PCWSTR(font_name.as_ptr()),
        )
    }
}

fn create_chevron_font(height: i32) -> HFONT {
    let font_name = w!("Segoe Fluent Icons");
    unsafe {
        CreateFontW(
            height,
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
            CLEARTYPE_QUALITY.0 as u32,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            PCWSTR(font_name.as_ptr()),
        )
    }
}

fn main() {
    cbx_core::registry::harden_process_dll_search();
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
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

    let hinst: HINSTANCE = unsafe { GetModuleHandleW(None).unwrap_or_default().into() };
    let class_name = w!("CbxFluentManagerWnd");
    let hicon = create_app_hicon(32);

    let wc = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: hinst,
        hIcon: hicon,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
        lpszClassName: class_name,
        ..Default::default()
    };

    unsafe {
        let _ = RegisterClassW(&wc);
    }

    let width = 1140;
    let height = 910;
    let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let x = ((screen_w - width) / 2).max(0);
    let y = ((screen_h - height) / 2).max(0);

    let title_w = to_wide(window_title());
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            class_name,
            PCWSTR(title_w.as_ptr()),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_THICKFRAME,
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
        refresh_system_theme(Some(hwnd));
        // Win11 frosted glass (Acrylic) with fallbacks; see `apply_frosted_backdrop`.
        apply_frosted_backdrop(hwnd);
        let margins = MARGINS {
            cxLeftWidth: -1,
            cxRightWidth: -1,
            cyTopHeight: -1,
            cyBottomHeight: -1,
        };
        let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
        // Re-run WM_NCCALCSIZE so the client area covers the caption (custom title bar).
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        );

        let _ = DragAcceptFiles(hwnd, true);
        let _ = ShowWindow(hwnd, SW_SHOW);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe fn apply_custom_text_associations(settings: &Settings) {
    let is_elevated = cbx_core::registry::is_elevated();
    for ext in settings.parsed_custom_text_extensions() {
        let dot_ext = format!(".{ext}");
        associate_extension(HKEY_CURRENT_USER, &dot_ext, false);
        if is_elevated {
            associate_extension(HKEY_LOCAL_MACHINE, &dot_ext, false);
        }
    }
    notify_shell_associations_changed();
}

unsafe fn apply_custom_text_associations_elevated(hwnd: HWND, settings: &Settings) {
    let exts = settings.parsed_custom_text_extensions();
    if exts.is_empty() {
        return;
    }
    if cbx_core::registry::is_elevated() {
        apply_custom_text_associations(settings);
        return;
    }
    let spec_items: Vec<String> = exts.iter().map(|ext| format!(".{ext}:1:0")).collect();
    let spec_arg = format!("--apply-associations {}", spec_items.join(";"));
    let cli_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("cbx-cli.exe")))
        .unwrap_or_else(|| PathBuf::from("cbx-cli.exe"));
    if !runas_elevated(hwnd, &cli_path, &spec_arg) {
        apply_custom_text_associations(settings);
    } else {
        notify_shell_associations_changed();
    }
}

unsafe fn commit_active_edit(state: &mut AppState) {
    if let Some(target) = state.active_edit.take() {
        if target == EditTarget::CustomTextExtensions {
            let trimmed = state.edit_buffer.trim().to_string();
            state.settings.custom_text_extensions = trimmed;
            state.settings.sanitize();
            state.edit_caret_to_end();
            state.edit_buffer.clear();
            state.has_unapplied_text_edits = true;
            return;
        }
        let parsed = state.edit_buffer.trim().parse::<u32>().unwrap_or_else(|_| match target {
            EditTarget::SeekFixedSec => state.settings.seek_fixed_sec,
            EditTarget::SeekThresholdMin => state.settings.seek_threshold_min,
            EditTarget::StepProbeSec => state.settings.step_probe_sec,
            EditTarget::MaxGopHops => state.settings.max_gop_hops,
            EditTarget::ConsecutiveLimit => state.settings.consecutive_unacceptable_limit,
            EditTarget::WatchdogTimeoutSec => state.settings.watchdog_timeout_sec,
            EditTarget::CustomTextExtensions => 0,
        });
        match target {
            EditTarget::SeekFixedSec => state.settings.seek_fixed_sec = parsed.clamp(0, 1800),
            EditTarget::SeekThresholdMin => state.settings.seek_threshold_min = parsed.clamp(1, 300),
            EditTarget::StepProbeSec => state.settings.step_probe_sec = parsed.clamp(1, 60),
            EditTarget::MaxGopHops => state.settings.max_gop_hops = parsed.clamp(0, 20),
            EditTarget::ConsecutiveLimit => state.settings.consecutive_unacceptable_limit = parsed.clamp(1, 20),
            EditTarget::WatchdogTimeoutSec => state.settings.watchdog_timeout_sec = parsed.clamp(3, 60),
            EditTarget::CustomTextExtensions => {}
        }
        state.settings.sanitize();
        state.edit_caret_to_end();
        state.edit_buffer.clear();
        state.has_unapplied_text_edits = true;
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Caption buttons (min / max / close) are drawn and hit-tested by DWM over the extended
    // frame; give it the first look at every message.
    let mut dwm_result = LRESULT(0);
    if DwmDefWindowProc(hwnd, msg, wparam, lparam, &mut dwm_result).as_bool() {
        return dwm_result;
    }
    match msg {
        // Custom title bar: keep the system's left / right / bottom resize borders but let the
        // client area start at the very top, so the caption strip is painted by us.
        WM_NCCALCSIZE if wparam.0 != 0 => {
            let params = &mut *(lparam.0 as *mut NCCALCSIZE_PARAMS);
            let top = params.rgrc[0].top;
            let result = DefWindowProcW(hwnd, msg, wparam, lparam);
            params.rgrc[0].top = top;
            if IsZoomed(hwnd).as_bool() {
                // A maximized window hangs its frame off-screen; keep the content on-screen.
                params.rgrc[0].top += resize_frame_h(hwnd);
            }
            result
        }
        WM_NCHITTEST => {
            let hit = DefWindowProcW(hwnd, msg, wparam, lparam);
            if hit.0 != HTCLIENT {
                return hit;
            }
            let mut pt = POINT {
                x: (lparam.0 & 0xFFFF) as i16 as i32,
                y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
            };
            let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
            if !IsZoomed(hwnd).as_bool() && pt.y < resize_frame_h(hwnd) {
                return LRESULT(HTTOP);
            }
            if pt.y < titlebar_h(hwnd) {
                return LRESULT(HTCAPTION);
            }
            hit
        }
        WM_CREATE => {
            let _ = ImmAssociateContext(hwnd, 0);
            let dpi = GetDpiForWindow(hwnd);
            let mut app = AppState::new(dpi);
            app.checkbox_anim_hwnd = hwnd;
            let auto_update_due = app.settings.auto_check_updates
                && updater::is_configured()
                && unix_now().saturating_sub(app.settings.last_update_check as u64) >= 86_400;
            state_install(hwnd, app);
            if auto_update_due {
                updater::start_check(hwnd.0);
            }
            let _ = SetTimer(hwnd, 2, 200, None);
            LRESULT(0)
        }
        WM_SIZE => {
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        0x0320 /* WM_DWMCOLORIZATIONCOLORCHANGED */ | 0x001A /* WM_SETTINGCHANGE */ => {
            refresh_system_theme(Some(hwnd));
            // Transparency toggle / light-dark switch changes which backdrop (and tint) applies.
            apply_frosted_backdrop(hwnd);
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_GETMINMAXINFO => {
            let mmi = &mut *(lparam.0 as *mut MINMAXINFO);
            mmi.ptMinTrackSize.x = 880;
            mmi.ptMinTrackSize.y = 620;
            LRESULT(0)
        }
        WM_TIMER => {
            let timer_id = wparam.0;
            if timer_id == 1 {
                if let Some(mut state_guard) = state_lock() {
                    let state: &mut AppState = &mut state_guard;
                    let was_lang_anim = state.lang_menu_anim_start.is_some();
                    let (still_anim, non_lang_active) = state.step_animations();
                    if was_lang_anim && !non_lang_active && state.expanded_monitor_item.is_none() {
                        let mut rect = RECT::default();
                        let _ = logical_client_rect(hwnd, &mut rect);
                        let sidebar_rect = RECT {
                            left: 0,
                            top: 0,
                            right: lang_invalidate_right(state),
                            bottom: rect.bottom + titlebar_h(hwnd),
                        };
                        let _ = InvalidateRect(hwnd, Some(&sidebar_rect), false);
                    } else {
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                    if !still_anim {
                        let _ = KillTimer(hwnd, 1);
                    }
                }
            } else if timer_id == 2 {
                if let Some(mut state_guard) = state_lock() {
                    let state: &mut AppState = &mut state_guard;
                    monitor_auto_off_tick(hwnd, state);
                    prewarm_tick(hwnd, state);
                    let snap = cbx_core::monitor::fetch_snapshot();
                    let changed = match (&state.monitor_snapshot, &snap) {
                        (Some(a), Some(b)) => {
                            a.active_tasks != b.active_tasks
                                || a.total_processed != b.total_processed
                                || a.current_stage != b.current_stage
                                || a.current_elapsed_ms != b.current_elapsed_ms
                        }
                        (None, None) => false,
                        _ => true,
                    };
                    state.monitor_snapshot = snap;
                    if changed {
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                }
            }
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            render_fluent_ui(hwnd, hdc, &ps.rcPaint);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 - titlebar_h(hwnd);
            handle_mouse_move(hwnd, x, y);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 - titlebar_h(hwnd);
            handle_mouse_down(hwnd, x, y);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = (lparam.0 & 0xFFFF) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32 - titlebar_h(hwnd);
            handle_mouse_up(hwnd, x, y);
            LRESULT(0)
        }
        WM_DROPFILES => {
            let hdrop = HDROP(wparam.0 as *mut _);
            handle_drop_files(hwnd, hdrop);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) as i16) as i32;
            handle_mouse_wheel(hwnd, delta);
            LRESULT(0)
        }
        WM_SETCURSOR => {
            if let Some(state_guard) = state_lock() {
                let state: &AppState = &state_guard;
                let hover_expander = (state.active_tab == ManagerTab::Archives
                    && (state.is_risk_header_hovered || state.is_sort_header_hovered))
                    || (state.active_tab == ManagerTab::Video && (state.is_video_params_header_hovered || state.is_ev_header_hovered));
                if hover_expander {
                    let cursor = LoadCursorW(None, IDC_HAND).unwrap_or_default();
                    let _ = SetCursor(cursor);
                    return LRESULT(1);
                }
                if state.active_tab == ManagerTab::Advanced {
                    let mut pt = windows::Win32::Foundation::POINT::default();
                    let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut pt);
                    let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut pt);
                    pt.y -= titlebar_h(hwnd);

                    let mut rect = RECT::default();
                    let _ = logical_client_rect(hwnd, &mut rect);
                    let win_w = rect.right - rect.left;
                    let sidebar_w = sidebar_width(state);
                    let content_x = sidebar_w + scale_px(25, state.dpi);
                    let content_w = (win_w - content_x - scale_px(30, state.dpi)).max(400);

                    let offset_banner = if is_media_preview_installed() { scale_px(45, state.dpi) } else { 0 };
                    let card1_y = scale_px(75, state.dpi) + offset_banner;
                    let card1_h = scale_px(125, state.dpi);
                    let card2_y = card1_y + card1_h + scale_px(14, state.dpi);
                    let resizer_y = card2_y + state.monitor_card_h - scale_px(10, state.dpi) - state.scroll_y;

                    let is_on_resizer = state.is_dragging_resizer
                        || (pt.x >= content_x && pt.x <= content_x + content_w && pt.y >= resizer_y && pt.y <= resizer_y + scale_px(16, state.dpi));

                    if is_on_resizer {
                        let cursor = LoadCursorW(None, IDC_SIZENS).unwrap_or_default();
                        let _ = SetCursor(cursor);
                        return LRESULT(1);
                    }
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_KEYDOWN_MSG => {
            if let Some(mut state_guard) = state_lock() {
                let state: &mut AppState = &mut state_guard;
                if state.active_edit.is_some() {
                    let shift = GetKeyState(0x10) < 0;
                    let len = state.edit_buffer.len();
                    let (sel_s, sel_e) = state.edit_sel();
                    let caret = state.edit_fix(state.edit_caret);
                    let handled = match wparam.0 as u32 {
                        0x25 => {
                            // Left
                            state.edit_caret = if !shift && sel_s < sel_e { sel_s } else { state.edit_fix(caret.saturating_sub(1)) };
                            true
                        }
                        0x27 => {
                            // Right
                            state.edit_caret = if !shift && sel_s < sel_e {
                                sel_e
                            } else {
                                let mut n = (caret + 1).min(len);
                                while n < len && !state.edit_buffer.is_char_boundary(n) {
                                    n += 1;
                                }
                                n
                            };
                            true
                        }
                        0x24 => {
                            state.edit_caret = 0; // Home
                            true
                        }
                        0x23 => {
                            state.edit_caret = len; // End
                            true
                        }
                        0x2E => {
                            // Delete: the selection, else the character after the caret
                            if !state.edit_delete_selection() && caret < len {
                                let mut n = caret + 1;
                                while n < len && !state.edit_buffer.is_char_boundary(n) {
                                    n += 1;
                                }
                                state.edit_buffer.replace_range(caret..n, "");
                                state.edit_caret = caret;
                            }
                            state.edit_anchor = state.edit_caret;
                            true
                        }
                        _ => false,
                    };
                    if handled {
                        if !shift {
                            state.edit_anchor = state.edit_caret;
                        }
                        let _ = InvalidateRect(hwnd, None, false);
                        return LRESULT(0);
                    }
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CHAR => {
            let ch = (wparam.0 & 0xFFFF) as u16;
            if let Some(mut state_guard) = state_lock() {
                let state: &mut AppState = &mut state_guard;
                if state.active_edit.is_some() {
                    match ch {
                        0x01 => { // Ctrl+A: select all
                            state.edit_select_all_text();
                            let _ = InvalidateRect(hwnd, None, false);
                            return LRESULT(0);
                        }
                        0x08 => { // Backspace: delete the selection, else the character before the caret
                            if !state.edit_delete_selection() {
                                let c = state.edit_fix(state.edit_caret);
                                if c > 0 {
                                    let p = state.edit_fix(c - 1);
                                    state.edit_buffer.replace_range(p..c, "");
                                    state.edit_caret = p;
                                    state.edit_anchor = p;
                                }
                            }
                            let _ = InvalidateRect(hwnd, None, false);
                            return LRESULT(0);
                        }
                        0x0D => { // Enter: Commit & Apply
                            let was_custom_ext = state.active_edit == Some(EditTarget::CustomTextExtensions);
                            commit_active_edit(state);
                            state.has_unapplied_text_edits = false;
                            let _ = state.settings.save();
                            if was_custom_ext {
                                apply_custom_text_associations_elevated(hwnd, &state.settings);
                            }
                            state.applied_custom_timer = Some(std::time::Instant::now());
                            let _ = InvalidateRect(hwnd, None, false);
                            return LRESULT(0);
                        }
                        0x16 => { // Ctrl+V: Paste from clipboard
                            if let Some(clip) = read_clipboard_text(hwnd) {
                                state.edit_delete_selection();
                                let is_custom = state.active_edit == Some(EditTarget::CustomTextExtensions);
                                let max_len = if is_custom { 64 } else { 4 };
                                for c in clip.chars() {
                                    let mapped = match c {
                                        '，' | '、' | '；' => ',',
                                        '。' | '．' => '.',
                                        other => other,
                                    };
                                    let ok = if is_custom {
                                        mapped.is_ascii_alphanumeric() || matches!(mapped, ',' | '.' | ' ' | '_' | '-')
                                    } else {
                                        mapped.is_ascii_digit()
                                    };
                                    if ok && state.edit_buffer.len() < max_len {
                                        state.edit_insert(mapped, max_len);
                                    }
                                }
                                let _ = InvalidateRect(hwnd, None, false);
                            }
                            return LRESULT(0);
                        }
                        0x1B => { // Esc: Cancel
                            state.active_edit = None;
                            state.edit_caret_to_end();
                            state.edit_buffer.clear();
                            let _ = InvalidateRect(hwnd, None, false);
                            return LRESULT(0);
                        }
                        0xFF0C | 0x3001 | 0xFF1B | 0x3002 | 0xFF0E => {
                            if state.active_edit == Some(EditTarget::CustomTextExtensions) {
                                let mapped = if matches!(ch, 0x3002 | 0xFF0E) { '.' } else { ',' };
                                state.edit_insert(mapped, 64);
                                let _ = InvalidateRect(hwnd, None, false);
                                return LRESULT(0);
                            }
                        }
                        0x20..=0x7E => {
                            let c = ch as u8 as char;
                            if state.active_edit == Some(EditTarget::CustomTextExtensions) {
                                if c.is_ascii_alphanumeric() || matches!(c, ',' | '.' | ' ' | '_' | '-') {
                                    state.edit_insert(c, 64);
                                    let _ = InvalidateRect(hwnd, None, false);
                                    return LRESULT(0);
                                }
                            } else if c.is_ascii_digit() {
                                state.edit_insert(c, 4);
                                let _ = InvalidateRect(hwnd, None, false);
                                return LRESULT(0);
                            }
                        }
                        _ => {}
                    }
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        m if m == updater::WM_APP_UPDATE => {
            if let Some(mut state) = state_lock() {
                if updater::take_check_completed() {
                    state.settings.last_update_check = unix_now().min(u32::MAX as u64) as u32;
                    let _ = state.settings.save();
                }
            }
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(hwnd, 2);
            if let Some(state) = state_take() {
                let _ = DeleteObject(state.font_hero);
                let _ = DeleteObject(state.font_title);
                let _ = DeleteObject(state.font_bold);
                let _ = DeleteObject(state.font_regular);
                let _ = DeleteObject(state.font_small);
                if let Some(hbmp) = state.test_thumb {
                    let _ = DeleteObject(hbmp);
                }
            }
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

// -----------------------------------------------------------------------------
// Mouse & Interaction Handlers
// -----------------------------------------------------------------------------

unsafe fn handle_mouse_move(hwnd: HWND, x: i32, y: i32) {
    let Some(mut state_guard) = state_lock() else { return };
    let state: &mut AppState = &mut state_guard;

    let prev_mouse = state.mouse_pos;
    state.mouse_pos = (x, y);
    if state.edit_dragging {
        let geom = state.active_edit.and_then(|t| state.edit_boxes.borrow().iter().copied().find(|g| g.target == t));
        if let Some(g) = geom {
            let text = state.edit_buffer.clone();
            state.edit_caret = edit_hit_index(hwnd, &g, &text, x);
            let _ = InvalidateRect(hwnd, None, false);
        }
        return;
    }

    if state.is_dragging_scrollbar {
        let mut rect = RECT::default();
        let _ = logical_client_rect(hwnd, &mut rect);
        let win_h = rect.bottom - rect.top;
        let (_, _, _, scrollable_track) = get_scrollbar_metrics(state, win_h);
        let dy = y - state.drag_scrollbar_start_y;
        let delta = ((dy as f32 / scrollable_track as f32) * state.max_scroll_y as f32).round() as i32;
        state.scroll_y = (state.drag_scroll_start_val + delta).clamp(0, state.max_scroll_y);
        let _ = InvalidateRect(hwnd, None, false);
        return;
    }

    if state.is_dragging_resizer {
        let dy = y - state.drag_resizer_start_y;
        state.monitor_card_h = (state.drag_resizer_start_h + dy).clamp(scale_px(180, state.dpi), scale_px(650, state.dpi));
        let _ = InvalidateRect(hwnd, None, false);
        return;
    }

    if let Some(slider_idx) = state.dragging_slider {
        if state.settings.custom_video_settings {
            update_slider_value(state, slider_idx, x);
            let _ = InvalidateRect(hwnd, None, false);
        }
        return;
    }

    let sidebar_w = sidebar_width(state);
    let mut need_repaint = false;

    if state.is_lang_menu_open {
        let mut rect = RECT::default();
        let _ = logical_client_rect(hwnd, &mut rect);
        let win_h = rect.bottom - rect.top;
        let new_lang_hover = lang_flyout_geom(state, win_h).item_at(x, y);
        if state.hover_lang_item != new_lang_hover {
            state.hover_lang_item = new_lang_hover;
            need_repaint = true;
        }
    }

    if state.open_monitor_dropdown.is_some() {
        let mut rect = RECT::default();
        let _ = logical_client_rect(hwnd, &mut rect);
        let new_hover = monitor_flyout_geom(state, rect.bottom - rect.top).and_then(|g| g.item_at(x, y));
        if state.hover_monitor_dropdown_item != new_hover {
            state.hover_monitor_dropdown_item = new_hover;
            need_repaint = true;
        }
    }

    // Check hover for Collapsible Headers (Archives: Sort + Risk; Video: Custom Algorithm Params)
    let mut new_sort_hover = false;
    let mut new_risk_hover = false;
    let mut new_video_params_hover = false;
    let mut new_ev_hover = false;
    if state.expanded_monitor_item.is_none()
        && !state.is_lang_menu_open
        && state.open_monitor_dropdown.is_none()
    {
        if state.active_tab == ManagerTab::Archives || state.active_tab == ManagerTab::Video {
            let mut rect = RECT::default();
            let _ = logical_client_rect(hwnd, &mut rect);
            let win_w = rect.right - rect.left;
            let content_x = sidebar_w + scale_px(25, state.dpi);
            let content_w = (win_w - content_x - scale_px(30, state.dpi)).max(400);
            let offset_banner = if is_media_preview_installed() { scale_px(45, state.dpi) } else { 0 };
            let content_y = y + state.scroll_y;

            if state.active_tab == ManagerTab::Archives {
                let card1_y = scale_px(75, state.dpi) + offset_banner;
                let card1_h = get_checklist_card_height(MediaCategory::ImageArchive, state.dpi);
                let card2_y = card1_y + card1_h + scale_px(14, state.dpi);
                let card2_h = scale_px(145, state.dpi);
                let sort_card_y = card2_y + card2_h + scale_px(14, state.dpi);
                let sort_header_h = scale_px(52, state.dpi);
                let sort_card_h = archive_sort_card_height(state.dpi, state.archive_sort_expand_anim);
                let opt_card_y = sort_card_y + sort_card_h + scale_px(14, state.dpi);
                let opt_card_h = scale_px(120, state.dpi);
                let risk_card_y = opt_card_y + opt_card_h + scale_px(14, state.dpi);
                let risk_header_h = scale_px(64, state.dpi);

                if x >= content_x && x <= content_x + content_w {
                    if content_y >= sort_card_y && content_y <= sort_card_y + sort_header_h {
                        new_sort_hover = true;
                    } else if content_y >= risk_card_y && content_y <= risk_card_y + risk_header_h {
                        new_risk_hover = true;
                    }
                }
            } else if state.active_tab == ManagerTab::Video {
                let card1_y = scale_px(75, state.dpi) + offset_banner;
                let card1_h = get_checklist_card_height(MediaCategory::Video, state.dpi);
                let card2_y = card1_y + card1_h + scale_px(14, state.dpi);
                let card2_h = scale_px(120, state.dpi);
                let card_tools_y = card2_y + card2_h + scale_px(14, state.dpi);
                let card_tools_h = if state.video_card_tools_h.get() > 0 {
                    state.video_card_tools_h.get()
                } else {
                    scale_px(210, state.dpi)
                };
                let card3_y = card_tools_y + card_tools_h + scale_px(14, state.dpi);
                let card3_h = scale_px(130, state.dpi);
                let card_hdr_y = card3_y + card3_h + scale_px(14, state.dpi);
                let card_hdr_h = scale_px(86, state.dpi);
                let card4_y = card_hdr_y + card_hdr_h + scale_px(14, state.dpi);
                let header_h = scale_px(52, state.dpi);

                if x >= content_x && x <= content_x + content_w && content_y >= card4_y && content_y <= card4_y + header_h {
                    new_video_params_hover = true;
                }
                let ev_y = card_tools_y + state.video_ev_card_rel_y.get();
                if state.video_ev_card_rel_y.get() > 0 && x >= content_x && x <= content_x + content_w && content_y >= ev_y && content_y <= ev_y + header_h {
                    new_ev_hover = true;
                }
            }
        }
    }
    if state.is_sort_header_hovered != new_sort_hover {
        state.is_sort_header_hovered = new_sort_hover;
        need_repaint = true;
    }
    if state.is_risk_header_hovered != new_risk_hover {
        state.is_risk_header_hovered = new_risk_hover;
        need_repaint = true;
    }
    if state.is_video_params_header_hovered != new_video_params_hover {
        state.is_video_params_header_hovered = new_video_params_hover;
        need_repaint = true;
    }
    if state.is_ev_header_hovered != new_ev_hover {
        state.is_ev_header_hovered = new_ev_hover;
        need_repaint = true;
    }

    let mut new_hover = None;
    if x < sidebar_w {
        let item_h = scale_px(44, state.dpi);
        // Same origin as rendering/clicks (was 90 px, 5 px off from the drawn pills).
        let start_y = scale_px(NAV_START_Y, state.dpi);
        for i in 0..7 {
            let iy = start_y + (i as i32 * item_h);
            if y >= iy && y < iy + item_h {
                new_hover = Some(i);
                break;
            }
        }
    }

    if state.hover_tab != new_hover {
        state.hover_tab = new_hover;
        need_repaint = true;
    }

    // Hover feedback for buttons and toggles in content area
    if prev_mouse != (x, y) {
        need_repaint = true;
    }

    if need_repaint {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

unsafe fn trigger_toggle_animation(hwnd: HWND, state: &mut AppState, id: u32, is_on: bool) {
    state.toggle_targets.insert(id, if is_on { 1.0 } else { 0.0 });
    let _ = SetTimer(hwnd, 1, 16, None);
    let _ = InvalidateRect(hwnd, None, false);
}

unsafe fn handle_mouse_down(hwnd: HWND, x: i32, y: i32) {
    let Some(mut state_guard) = state_lock() else { return };
    let state: &mut AppState = &mut state_guard;

    let mut rect = RECT::default();
    let _ = logical_client_rect(hwnd, &mut rect);
    let win_w = rect.right - rect.left;
    let win_h = rect.bottom - rect.top;

    // If detail modal is active, handle copy, close or outside click
    if state.expanded_monitor_item.is_some() {
        let modal_w = scale_px(620, state.dpi).min(win_w - scale_px(32, state.dpi));
        let modal_h = scale_px(518, state.dpi).min(win_h - scale_px(32, state.dpi));
        let modal_x = (win_w - modal_w) / 2;
        let modal_y = (win_h - modal_h) / 2;

        let pad_x = scale_px(22, state.dpi);
        let close_sz = scale_px(26, state.dpi);
        let close_x = modal_x + modal_w - pad_x - close_sz;
        let close_y = modal_y + scale_px(18, state.dpi);

        let ok_w = scale_px(110, state.dpi);
        let ok_h = scale_px(30, state.dpi);
        let ok_x = modal_x + (modal_w - ok_w) / 2;
        let ok_y = modal_y + modal_h - ok_h - scale_px(14, state.dpi);

        // Path display box coordinates
        let sep_y = modal_y + scale_px(18 + 34, state.dpi);
        let lbl_y = sep_y + scale_px(12, state.dpi);
        let box_x = modal_x + pad_x;
        let box_y = lbl_y + scale_px(20, state.dpi);
        let box_w = modal_w - pad_x * 2;
        let box_h = scale_px(62, state.dpi);

        let hit_box = x >= box_x && x <= box_x + box_w && y >= box_y && y <= box_y + box_h;

        if hit_box {
            if let Some(ref item) = state.expanded_monitor_item {
                let is_stream = !std::path::Path::new(&item.file_name).is_absolute()
                    || item.file_name.contains("串流")
                    || item.file_name.contains("Stream");
                let text_to_copy = if is_stream {
                    format!("{}{}", tr("[RPC 串流] ", "[RPC Stream] "), item.file_name)
                } else {
                    item.file_name.clone()
                };
                copy_to_clipboard(hwnd, &text_to_copy);
                state.copied_path_timer = Some(std::time::Instant::now());
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }
        }

        // Details debug box coordinates (5 metadata rows before det_hdr_y)
        let grid_y = box_y + box_h + scale_px(10, state.dpi);
        let row_h = scale_px(22, state.dpi);
        let det_hdr_y = grid_y + row_h * 5 + scale_px(10, state.dpi);
        let dbox_y = det_hdr_y + scale_px(22, state.dpi);
        let dbox_h = scale_px(64, state.dpi);

        let hit_debug_box = x >= box_x && x <= box_x + box_w && y >= dbox_y && y <= dbox_y + dbox_h;

        if hit_debug_box {
            if let Some(item) = state.expanded_monitor_item.clone() {
                let debug_text = format_debug_report(&item);
                copy_to_clipboard(hwnd, &debug_text);
                state.copied_details_timer = Some(std::time::Instant::now());
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }
        }

        let hit_close_btn = x >= close_x && x <= close_x + close_sz && y >= close_y && y <= close_y + close_sz;
        let hit_ok_btn = x >= ok_x && x <= ok_x + ok_w && y >= ok_y && y <= ok_y + ok_h;
        let hit_outside = x < modal_x || x > modal_x + modal_w || y < modal_y || y > modal_y + modal_h;

        if hit_close_btn || hit_ok_btn || hit_outside {
            state.expanded_monitor_item = None;
            state.copied_path_timer = None;
            state.copied_details_timer = None;
            let _ = InvalidateRect(hwnd, None, false);
        }
        return;
    }

    let sidebar_w = sidebar_width(state);

    // If language flyout is open: press an item (applied on release) or close on outside click
    if state.is_lang_menu_open {
        if let Some(idx) = lang_flyout_geom(state, win_h).item_at(x, y) {
            state.pressed_lang_item = Some(idx);
            state.flyout_press_start = Some(std::time::Instant::now());
            let _ = SetTimer(hwnd, 1, 10, None);
            let _ = InvalidateRect(hwnd, None, false);
            return;
        }
        state.is_lang_menu_open = false;
        state.lang_menu_anim_start = None;
        let _ = InvalidateRect(hwnd, None, false);
        return;
    }

    // If a monitor filter flyout is open: press an item (applied on release) or close on outside click
    if state.open_monitor_dropdown.is_some() {
        if let Some(idx) = monitor_flyout_geom(state, win_h).and_then(|g| g.item_at(x, y)) {
            state.pressed_monitor_dd_item = Some(idx);
            state.flyout_press_start = Some(std::time::Instant::now());
            let _ = SetTimer(hwnd, 1, 10, None);
            let _ = InvalidateRect(hwnd, None, false);
            return;
        }
        state.open_monitor_dropdown = None;
        state.monitor_dd_anim_start = None;
        state.hover_monitor_dropdown_item = None;
        let _ = InvalidateRect(hwnd, None, false);
        return;
    }

    // Clear button of the focused text box (acts on release)
    if state.active_edit.is_some() {
        if let Some(rc) = state.edit_clear_rect.get() {
            if x >= rc.left && x < rc.right && y >= rc.top && y < rc.bottom {
                state.edit_clear_pressed = true;
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }
        }
    }

    // Text boxes: focus, place the caret under the pointer and start a drag selection
    let hit_box = state
        .edit_boxes
        .borrow()
        .iter()
        .copied()
        .find(|g| x >= g.rect.left && x < g.rect.right && y >= g.rect.top && y < g.rect.bottom);
    if let Some(g) = hit_box {
        if state.active_edit != Some(g.target) {
            commit_active_edit(state);
            state.active_edit = Some(g.target);
            state.edit_buffer = edit_value_string(state, g.target);
        }
        let text = state.edit_buffer.clone();
        let idx = edit_hit_index(hwnd, &g, &text, x);
        state.edit_caret = idx;
        state.edit_anchor = idx;
        state.edit_dragging = true;
        SetCapture(hwnd);
        let _ = InvalidateRect(hwnd, None, false);
        return;
    }

    // Clicking anywhere else ends text editing (Win11 TextBox loses focus), except the unit
    // label and the − / + spin buttons on the same row, which keep editing the value.
    if let Some(target) = state.active_edit {
        let geom = state.edit_boxes.borrow().iter().copied().find(|g| g.target == target);
        let on_own_row = geom.is_some_and(|g| {
            y >= g.rect.top && y < g.rect.bottom && x >= g.rect.left && x < g.rect.right + scale_px(110, state.dpi)
        });
        if !on_own_row {
            commit_active_edit(state);
            state.edit_dragging = false;
            let _ = InvalidateRect(hwnd, None, false);
            // fall through: the click still reaches whatever is under the pointer
        }
    }

    // Rail toggle button (press here, toggle on release)
    let hb = hamburger_rect(state);
    if x >= hb.left && x < hb.right && y >= hb.top && y < hb.bottom {
        state.hamburger_pressed = true;
        let _ = InvalidateRect(hwnd, None, false);
        return;
    }

    // Sidebar tab clicks
    if x < sidebar_w {
        let lang_y = win_h - scale_px(50, state.dpi);
        let (lang_x, lang_w, _) = lang_button_geom(state, sidebar_w);
        let lang_h = scale_px(LANG_BTN_H, state.dpi);

        // Press down on language button: micro-move arrow, defer menu trigger to mouse up
        if x >= lang_x && x <= lang_x + lang_w && y >= lang_y && y <= lang_y + lang_h {
            state.lang_btn_pressed = true;
            state.pressed_btn = Some(105);
            let _ = InvalidateRect(hwnd, None, false);
            return;
        }

        let item_h = scale_px(44, state.dpi);
        let start_y = scale_px(NAV_START_Y, state.dpi);
        for i in 0..7 {
            let iy = start_y + (i as i32 * item_h);
            if y >= iy && y < iy + item_h {
                state.pressed_tab = Some(i);
                state.pressed_tab_anim = 0.0;
                state.pressed_tab_anim_start = Some(std::time::Instant::now());
                let _ = SetTimer(hwnd, 1, 16, None);
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }
        }
    } else {
        let scrollbar_w = scale_px(10, state.dpi);
        let scrollbar_x = win_w - scrollbar_w - scale_px(4, state.dpi);

        if state.max_scroll_y > 0 && x >= scrollbar_x {
            let (track_y, track_h, thumb_h, scrollable_track) = get_scrollbar_metrics(state, win_h);
            let thumb_y = track_y + ((state.scroll_y as f32 / state.max_scroll_y as f32) * scrollable_track as f32).round() as i32;

            if y >= thumb_y && y <= thumb_y + thumb_h {
                // 點擊在滑塊上，直接開始拖曳
                state.is_dragging_scrollbar = true;
                state.drag_scrollbar_start_y = y;
                state.drag_scroll_start_val = state.scroll_y;
            } else if y >= track_y && y <= track_y + track_h {
                // 點擊在軌道空白處，跳轉至點擊位置並接續拖曳
                let target_thumb_top = (y - thumb_h / 2).clamp(track_y, track_y + scrollable_track);
                let click_ratio = (target_thumb_top - track_y) as f32 / scrollable_track as f32;
                let new_scroll_y = (click_ratio * state.max_scroll_y as f32).round() as i32;
                state.scroll_y = new_scroll_y.clamp(0, state.max_scroll_y);
                state.is_dragging_scrollbar = true;
                state.drag_scrollbar_start_y = y;
                state.drag_scroll_start_val = state.scroll_y;
                let _ = InvalidateRect(hwnd, None, false);
            }
            return;
        }

        handle_content_click(hwnd, state, x, y);
    }
}

unsafe fn handle_mouse_up(hwnd: HWND, x: i32, y: i32) {
    if let Some(mut state_guard) = state_lock() {
        let state: &mut AppState = &mut state_guard;
        if state.edit_dragging {
            state.edit_dragging = false;
            let _ = ReleaseCapture();
            let _ = InvalidateRect(hwnd, None, false);
            return;
        }
        if state.edit_clear_pressed {
            state.edit_clear_pressed = false;
            if let Some(rc) = state.edit_clear_rect.get() {
                if x >= rc.left && x < rc.right && y >= rc.top && y < rc.bottom {
                    clear_active_edit_to_default(state);
                }
            }
            let _ = InvalidateRect(hwnd, None, false);
            return;
        }
        if state.pressed_lang_item.is_some() || state.pressed_monitor_dd_item.is_some() {
            let mut rect = RECT::default();
            let _ = logical_client_rect(hwnd, &mut rect);
            let win_h = rect.bottom - rect.top;
            state.flyout_press_start = None;
            if let Some(idx) = state.pressed_lang_item.take() {
                if state.is_lang_menu_open && lang_flyout_geom(state, win_h).item_at(x, y) == Some(idx) {
                    apply_language_choice(hwnd, state, idx);
                    state.is_lang_menu_open = false;
                    state.lang_menu_anim_start = None;
                    state.hover_lang_item = None;
                }
            }
            if let Some(idx) = state.pressed_monitor_dd_item.take() {
                if let Some(kind) = state.open_monitor_dropdown {
                    if monitor_flyout_geom(state, win_h).and_then(|g| g.item_at(x, y)) == Some(idx) {
                        apply_monitor_filter_choice(state, kind, idx);
                        state.open_monitor_dropdown = None;
                        state.monitor_dd_anim_start = None;
                        state.hover_monitor_dropdown_item = None;
                    }
                }
            }
            let _ = InvalidateRect(hwnd, None, false);
            return;
        }
        if state.hamburger_pressed {
            state.hamburger_pressed = false;
            let hb = hamburger_rect(state);
            if x >= hb.left && x < hb.right && y >= hb.top && y < hb.bottom {
                state.toggle_sidebar(hwnd);
            } else {
                let _ = InvalidateRect(hwnd, None, false);
            }
            return;
        }
        if let Some(exp_id) = state.pressed_expander.take() {
            state.pressed_expander_anim_start = None;
            state.pressed_expander_anim = 0.0;
            let still_over = match exp_id {
                0 => state.active_tab == ManagerTab::Archives && state.is_sort_header_hovered,
                1 => state.active_tab == ManagerTab::Archives && state.is_risk_header_hovered,
                2 => state.active_tab == ManagerTab::Video && state.is_video_params_header_hovered,
                3 => state.active_tab == ManagerTab::Video && state.is_ev_header_hovered,
                _ => false,
            };
            if still_over {
                match exp_id {
                    0 => {
                        state.is_archive_sort_expanded = !state.is_archive_sort_expanded;
                        state.archive_sort_anim_start = Some((std::time::Instant::now(), state.is_archive_sort_expanded));
                    }
                    1 => {
                        state.is_risk_card_expanded = !state.is_risk_card_expanded;
                        state.risk_card_anim_start = Some((std::time::Instant::now(), state.is_risk_card_expanded));
                    }
                    2 => {
                        state.is_video_params_expanded = !state.is_video_params_expanded;
                        state.video_params_anim_start = Some((std::time::Instant::now(), state.is_video_params_expanded));
                    }
                    3 => {
                        state.is_ev_expanded = !state.is_ev_expanded;
                        state.ev_anim_start = Some((std::time::Instant::now(), state.is_ev_expanded));
                    }
                    _ => {}
                }
                let _ = SetTimer(hwnd, 1, 16, None);
            }
            let _ = InvalidateRect(hwnd, None, false);
        }
        if let Some(pt) = state.pressed_tab.take() {
            state.pressed_tab_anim_start = None;
            state.pressed_tab_anim = 0.0;
            let sidebar_w = sidebar_width(state);
            let item_h = scale_px(44, state.dpi);
            let start_y = scale_px(NAV_START_Y, state.dpi);
            let iy = start_y + (pt as i32 * item_h);
            if x >= 0 && x < sidebar_w && y >= iy && y < iy + item_h {
                state.switch_tab(hwnd, ManagerTab::from_index(pt));
            } else {
                let _ = InvalidateRect(hwnd, None, false);
            }
        }
        if state.lang_btn_pressed {
            state.lang_btn_pressed = false;
            let mut rect = RECT::default();
            let _ = logical_client_rect(hwnd, &mut rect);
            let win_h = rect.bottom - rect.top;
            let sidebar_w = sidebar_width(state);
            let lang_y = win_h - scale_px(50, state.dpi);
            let (lang_x, lang_w, _) = lang_button_geom(state, sidebar_w);
            let lang_h = scale_px(LANG_BTN_H, state.dpi);

            // On mouse up over button: open the flyout menu with accelerated upward extend animation
            if x >= lang_x && x <= lang_x + lang_w && y >= lang_y && y <= lang_y + lang_h {
                state.is_lang_menu_open = true;
                state.lang_menu_anim = 0.0;
                state.lang_menu_anim_start = Some(std::time::Instant::now());
                let _ = SetTimer(hwnd, 1, 10, None);
            }
            let sidebar_rect = RECT {
                left: 0,
                top: 0,
                right: lang_invalidate_right(state),
                bottom: win_h + titlebar_h(hwnd),
            };
            let _ = InvalidateRect(hwnd, Some(&sidebar_rect), false);
        }
        if let Some(dd_kind) = state.pressed_monitor_dropdown.take() {
            let (pipe_x, status_x, btn_y, btn_h) = state.monitor_dropdown_anchor.get();
            let (btn_x, btn_w) = if dd_kind == 0 {
                (pipe_x, scale_px(104, state.dpi))
            } else {
                (status_x, scale_px(98, state.dpi))
            };
            if x >= btn_x && x <= btn_x + btn_w && y >= btn_y && y <= btn_y + btn_h {
                state.open_monitor_dropdown = Some(dd_kind);
                state.hover_monitor_dropdown_item = None;
                state.monitor_dd_anim = 0.0;
                state.monitor_dd_anim_start = Some(std::time::Instant::now());
                let _ = SetTimer(hwnd, 1, 10, None);
            }
            let _ = InvalidateRect(hwnd, None, false);
        }
        if state.is_dragging_scrollbar {
            state.is_dragging_scrollbar = false;
            let _ = InvalidateRect(hwnd, None, false);
        }
        if state.is_dragging_resizer {
            state.is_dragging_resizer = false;
            let _ = InvalidateRect(hwnd, None, false);
        }
        if state.dragging_slider.is_some() {
            state.dragging_slider = None;
            let _ = state.settings.save();
        }
        if state.pressed_btn.is_some() {
            state.pressed_btn = None;
            let _ = InvalidateRect(hwnd, None, false);
        }
    }
}

unsafe fn update_slider_value(state: &mut AppState, slider_idx: usize, mouse_x: i32) {
    let sidebar_w = sidebar_width(state);
    // All draggable sliders sit inside the video-parameters expander body (inset rows).
    let content_x = sidebar_w + scale_px(25, state.dpi) + scale_px(EXPANDER_CONTENT_INDENT, state.dpi);
    let track_x = content_x + scale_px(250, state.dpi);
    let track_w = scale_px(180, state.dpi);

    let ratio = ((mouse_x - track_x) as f32 / track_w as f32).clamp(0.0, 1.0);
    let val_100 = (ratio * 100.0).round() as u32;

    match slider_idx {
        0 => state.settings.position_seek = val_100,
        1 => state.settings.luminance_lower_limit = val_100.min(state.settings.luminance_upper_limit.saturating_sub(1)),
        2 => state.settings.luminance_upper_limit = val_100.max(state.settings.luminance_lower_limit + 1),
        3 => state.settings.entropy_lower_limit = val_100,
        5 => state.settings.seek_fixed_sec = (ratio * 600.0).round() as u32, // 0..600s
        6 => state.settings.seek_threshold_min = ((ratio * 120.0).round() as u32).max(5), // 5..120min
        _ => {}
    }
    state.settings.sanitize();
}

fn get_checklist_card_height(cat: MediaCategory, dpi: u32) -> i32 {
    let count = SUPPORTED_MEDIA_FORMATS.iter().filter(|fmt| fmt.category == cat).count();
    let rows = (count as i32 + 4) / 5;
    let item_h = scale_px(26, dpi);
    let start_y = scale_px(45, dpi);
    let btn_h = scale_px(28, dpi);
    start_y + rows * item_h + scale_px(10, dpi) + btn_h + scale_px(16, dpi)
}

fn calculate_content_height(state: &AppState) -> i32 {
    let (tab, dpi) = (state.active_tab, state.dpi);
    let offset_banner = if is_media_preview_installed() { scale_px(45, dpi) } else { 0 };
    match tab {
        ManagerTab::Archives => {
            let card1_h = get_checklist_card_height(MediaCategory::ImageArchive, dpi);
            let card2_h = scale_px(145, dpi);
            let sort_anim = state.archive_sort_expand_anim;
            let sort_card_h = archive_sort_card_height(dpi, sort_anim);
            let opt_card_h = scale_px(120, dpi);
            let header_h = scale_px(64, dpi);
            let extra_h = scale_px(210, dpi);
            let anim_val = state.risk_card_expand_anim;
            let risk_card_h = header_h + ((extra_h as f32) * anim_val) as i32;
            scale_px(75, dpi) + offset_banner + card1_h + scale_px(14, dpi) + card2_h + scale_px(14, dpi) + sort_card_h + scale_px(14, dpi) + opt_card_h + scale_px(14, dpi) + risk_card_h + scale_px(40, dpi)
        }
        ManagerTab::Ebook => {
            let card1_h = get_checklist_card_height(MediaCategory::Ebook, dpi);
            let card2_h = scale_px(145, dpi);
            scale_px(75, dpi) + offset_banner + card1_h + scale_px(16, dpi) + card2_h + scale_px(40, dpi)
        }
        ManagerTab::Video => {
            let card1_h = get_checklist_card_height(MediaCategory::Video, dpi);
            let card2_h = scale_px(120, dpi);
            let card_tools_h = state.video_card_tools_h.get();
            let card3_h = scale_px(130, dpi);
            let card_hdr_h = scale_px(86, dpi);
            let params_anim = state.video_params_expand_anim;
            let card4_h = video_card4_height(dpi, params_anim);
            scale_px(75, dpi) + offset_banner + card1_h + scale_px(14, dpi) + card2_h + scale_px(14, dpi) + card_tools_h + scale_px(14, dpi) + card3_h + scale_px(14, dpi) + card_hdr_h + scale_px(14, dpi) + card4_h + scale_px(55, dpi)
        }
        ManagerTab::Audio => {
            let card1_h = get_checklist_card_height(MediaCategory::Audio, dpi);
            let card2_h = scale_px(145, dpi);
            let card3_h = scale_px(95, dpi);
            scale_px(75, dpi) + offset_banner + card1_h + scale_px(16, dpi) + card2_h + scale_px(16, dpi) + card3_h + scale_px(40, dpi)
        }
        ManagerTab::Other => {
            let card1_h = get_checklist_card_height(MediaCategory::Other, dpi);
            let card2_h = get_checklist_card_height(MediaCategory::Syntax, dpi);
            let card3_h = scale_px(108, dpi);
            let card4_h = scale_px(254, dpi);
            scale_px(75, dpi)
                + offset_banner
                + card1_h
                + scale_px(14, dpi)
                + card2_h
                + scale_px(14, dpi)
                + card3_h
                + scale_px(14, dpi)
                + card4_h
                + scale_px(40, dpi)
        }
        ManagerTab::Advanced => {
            let card1_h = scale_px(125, dpi);
            let header_h = scale_px(44, dpi);
            let user_h = state.monitor_card_h;
            let anim = state.monitor_expand_anim;
            let card2_h = header_h + (((user_h - header_h) as f32) * anim) as i32;
            let card3_h = state.drop_card_h.get().max(scale_px(175, dpi));
            scale_px(75, dpi) + offset_banner + card1_h + scale_px(16, dpi) + card2_h + scale_px(16, dpi) + card3_h + scale_px(40, dpi)
        }
        ManagerTab::About => {
            let card1_h = state.about_card1_h.get();
            let card2_h = scale_px(390, dpi);
            scale_px(75, dpi)
                + card1_h
                + scale_px(14, dpi)
                + about_update_card_h(dpi)
                + scale_px(16, dpi)
                + card2_h
                + scale_px(40, dpi)
        }
    }
}

fn get_scrollbar_metrics(state: &AppState, win_h: i32) -> (i32, i32, i32, i32) {
    let dpi = state.dpi;
    let clip_top = scale_px(40, dpi);
    let track_y = clip_top;
    let track_h = (win_h - track_y - scale_px(12, dpi)).max(scale_px(35, dpi));
    let total_h = calculate_content_height(state);
    let thumb_h = ((track_h as f32 * (win_h as f32 / total_h.max(1) as f32)).round() as i32)
        .clamp(scale_px(30, dpi), track_h);
    let scrollable_track = (track_h - thumb_h).max(1);
    (track_y, track_h, thumb_h, scrollable_track)
}

unsafe fn handle_content_click(hwnd: HWND, state: &mut AppState, x: i32, y: i32) {
    let y = y + state.scroll_y;
    let sidebar_w = sidebar_width(state);
    let content_x = sidebar_w + scale_px(25, state.dpi);
    let mut rect = RECT::default();
    let _ = logical_client_rect(hwnd, &mut rect);
    let win_w = rect.right - rect.left;
    let content_w = (win_w - content_x - scale_px(30, state.dpi)).max(400);

    let offset_banner = if is_media_preview_installed() { scale_px(45, state.dpi) } else { 0 };

    match state.active_tab {
        ManagerTab::Archives => {
            let card1_y = scale_px(75, state.dpi) + offset_banner;
            let card1_h = get_checklist_card_height(MediaCategory::ImageArchive, state.dpi);
            handle_checklist_click(hwnd, state, content_x, card1_y, content_w, MediaCategory::ImageArchive, 200, x, y);

            let card2_y = card1_y + card1_h + scale_px(14, state.dpi);
            let card2_h = scale_px(145, state.dpi);
            handle_border_preview_click(hwnd, state, content_x, card2_y, content_w, 0, x, y);

            // Card 3: Independent Collapsible Card for Natural Sort + Continuous Filename Sequence
            let sort_card_y = card2_y + card2_h + scale_px(14, state.dpi);
            let sort_header_h = scale_px(52, state.dpi);
            let sort_card_h = archive_sort_card_height(state.dpi, state.archive_sort_expand_anim);
            let s_chevron_w = scale_px(22, state.dpi);
            let s_chevron_x = content_x + content_w - s_chevron_w - scale_px(16, state.dpi);
            let s_switch_w = scale_px(40, state.dpi);
            let s_switch_h = scale_px(24, state.dpi);
            let s_switch_x = s_chevron_x - s_switch_w - scale_px(14, state.dpi);
            let s_switch_y = sort_card_y + (sort_header_h - s_switch_h) / 2;

            if x >= s_switch_x && x <= s_switch_x + s_switch_w && y >= s_switch_y && y <= s_switch_y + s_switch_h {
                state.settings.sort_alphabetically = !state.settings.sort_alphabetically;
                let _ = state.settings.save();
                trigger_toggle_animation(hwnd, state, 1, state.settings.sort_alphabetically);
                return;
            }
            if x >= content_x && x <= content_x + content_w && y >= sort_card_y && y <= sort_card_y + sort_header_h {
                state.pressed_expander = Some(0);
                state.pressed_expander_anim = 0.0;
                state.pressed_expander_anim_start = Some(std::time::Instant::now());
                let _ = SetTimer(hwnd, 1, 16, None);
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }
            if state.is_archive_sort_expanded && state.archive_sort_expand_anim > 0.5 && state.settings.sort_alphabetically {
                let div_y = sort_card_y + sort_header_h;
                let scy = div_y + scale_px(8, state.dpi);
                let sub_row_h = scale_px(31, state.dpi);
                if x >= content_x && x <= content_x + content_w && y >= scy && y < scy + sub_row_h {
                    state.settings.prefer_continuous_sequence = !state.settings.prefer_continuous_sequence;
                    let _ = state.settings.save();
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                }
            }

            // Card 4: Remaining Archive Extraction Options (3 toggles)
            let opt_card_y = sort_card_y + sort_card_h + scale_px(14, state.dpi);
            let opt_card_h = scale_px(120, state.dpi);
            let row_h = scale_px(34, state.dpi);
            let switch_w = scale_px(44, state.dpi);
            let switch_x = content_x + content_w - scale_px(40, state.dpi) - scale_px(52, state.dpi) - scale_px(2, state.dpi);
            let mut cy = opt_card_y + scale_px(14, state.dpi);

            if x >= switch_x && x <= switch_x + switch_w {
                // Row 0: Crop tall
                if y >= cy && y <= cy + scale_px(26, state.dpi) {
                    state.settings.crop_tall_strips = !state.settings.crop_tall_strips;
                    let _ = state.settings.save();
                    trigger_toggle_animation(hwnd, state, 2, state.settings.crop_tall_strips);
                    return;
                }
                cy += row_h;
                // Row 1: Infotip
                if y >= cy && y <= cy + scale_px(26, state.dpi) {
                    state.settings.enable_infotip = !state.settings.enable_infotip;
                    let _ = state.settings.save();
                    trigger_toggle_animation(hwnd, state, 3, state.settings.enable_infotip);
                    return;
                }
                cy += row_h;
                // Row 2: Filter plain
                if y >= cy && y <= cy + scale_px(26, state.dpi) {
                    state.settings.filter_plain_covers = !state.settings.filter_plain_covers;
                    let _ = state.settings.save();
                    trigger_toggle_animation(hwnd, state, 7, state.settings.filter_plain_covers);
                    return;
                }
            }

            // Card 5: Security Risk Badges & Detection (Win11 Expander)
            let risk_card_y = opt_card_y + opt_card_h + scale_px(14, state.dpi);
            let header_h = scale_px(64, state.dpi);
            let chevron_w = scale_px(22, state.dpi);
            let chevron_x = content_x + content_w - chevron_w - scale_px(16, state.dpi);
            let switch_w = scale_px(42, state.dpi);
            let switch_h = scale_px(24, state.dpi);
            let switch_x = chevron_x - switch_w - scale_px(14, state.dpi);
            let switch_y = risk_card_y + (header_h - switch_h) / 2;

            // 1. Direct click on Master Switch (ID 4)
            if x >= switch_x && x <= switch_x + switch_w && y >= switch_y && y <= switch_y + switch_h {
                state.settings.badge_risk = !state.settings.badge_risk;
                let _ = state.settings.save();
                trigger_toggle_animation(hwnd, state, 4, state.settings.badge_risk);
                return;
            }

            // 2. Click anywhere on Card Header (press-and-hold; toggle on release)
            if x >= content_x && x <= content_x + content_w && y >= risk_card_y && y <= risk_card_y + header_h {
                state.pressed_expander = Some(1);
                state.pressed_expander_anim = 0.0;
                state.pressed_expander_anim_start = Some(std::time::Instant::now());
                let _ = SetTimer(hwnd, 1, 16, None);
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }

            // 3. Sub-rules Checkbox clicks (when expanded and risk_card_expand_anim > 0.5)
            if state.is_risk_card_expanded && state.risk_card_expand_anim > 0.5 {
                if state.settings.badge_risk {
                    let div_y = risk_card_y + header_h;
                    let mut scy = div_y + scale_px(8, state.dpi);
                    let sub_row_h = scale_px(31, state.dpi);
                    let rules_max_x = content_x + content_w - scale_px(180, state.dpi);

                    if x >= content_x && x <= rules_max_x {
                        // Sub 1: Executables (41)
                        if y >= scy && y < scy + sub_row_h {
                            state.settings.risk_rule_executables = !state.settings.risk_rule_executables;
                            let _ = state.settings.save();
                            let _ = InvalidateRect(hwnd, None, false);
                            return;
                        }
                        scy += sub_row_h;
                        // Sub 2: Traversal (42)
                        if y >= scy && y < scy + sub_row_h {
                            state.settings.risk_rule_traversal = !state.settings.risk_rule_traversal;
                            let _ = state.settings.save();
                            let _ = InvalidateRect(hwnd, None, false);
                            return;
                        }
                        scy += sub_row_h;
                        // Sub 3: Hidden files (43)
                        if y >= scy && y < scy + sub_row_h {
                            state.settings.risk_rule_hidden_files = !state.settings.risk_rule_hidden_files;
                            let _ = state.settings.save();
                            let _ = InvalidateRect(hwnd, None, false);
                            return;
                        }
                        scy += sub_row_h;
                        // Sub 4: Text files (44)
                        if y >= scy && y < scy + sub_row_h {
                            state.settings.risk_rule_text_files = !state.settings.risk_rule_text_files;
                            let _ = state.settings.save();
                            let _ = InvalidateRect(hwnd, None, false);
                            return;
                        }
                        scy += sub_row_h;
                        // Sub 5: Sensitive (45)
                        if y >= scy && y < scy + sub_row_h {
                            state.settings.risk_rule_sensitive = !state.settings.risk_rule_sensitive;
                            let _ = state.settings.save();
                            let _ = InvalidateRect(hwnd, None, false);
                            return;
                        }
                        scy += sub_row_h;
                        // Sub 6: Evasion (46)
                        if y >= scy && y < scy + sub_row_h {
                            state.settings.risk_rule_evasion = !state.settings.risk_rule_evasion;
                            let _ = state.settings.save();
                            let _ = InvalidateRect(hwnd, None, false);
                            return;
                        }
                    }
                }
            }
        }
        ManagerTab::Ebook => {
            let card1_y = scale_px(75, state.dpi) + offset_banner;
            let card1_h = get_checklist_card_height(MediaCategory::Ebook, state.dpi);
            handle_checklist_click(hwnd, state, content_x, card1_y, content_w, MediaCategory::Ebook, 200, x, y);

            let card2_y = card1_y + card1_h + scale_px(16, state.dpi);
            handle_border_preview_click(hwnd, state, content_x, card2_y, content_w, 3, x, y);
        }
        ManagerTab::Video => {
            let card1_y = scale_px(75, state.dpi) + offset_banner;
            let card1_h = get_checklist_card_height(MediaCategory::Video, state.dpi);
            handle_checklist_click(hwnd, state, content_x, card1_y, content_w, MediaCategory::Video, 200, x, y);

            // Card 2: Execution Mode
            let card2_y = card1_y + card1_h + scale_px(14, state.dpi);
            let card2_h = scale_px(120, state.dpi);
            let btn_y = card2_y + scale_px(40, state.dpi);
            let btn_h = scale_px(32, state.dpi);
            let b1_w = scale_px(150, state.dpi);
            let b2_w = scale_px(150, state.dpi);
            let b1_x = content_x + scale_px(16, state.dpi);
            let b2_x = b1_x + b1_w + scale_px(10, state.dpi);

            if y >= btn_y && y <= btn_y + btn_h {
                if x >= b1_x && x <= b1_x + b1_w && !state.settings.video_sandbox_mode {
                    state.trigger_button_anim(hwnd, 220, true);
                    state.settings.video_sandbox_mode = true;
                    let _ = state.settings.save();
                    apply_execution_mode(true);
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                } else if x >= b2_x && x <= b2_x + b2_w && state.settings.video_sandbox_mode {
                    state.trigger_button_anim(hwnd, 221, true);
                    state.settings.video_sandbox_mode = false;
                    let _ = state.settings.save();
                    apply_execution_mode(false);
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                }
            }

            // Card 2.5: Merged FFmpeg & Everything Helper Card Buttons
            let card_tools_y = card2_y + card2_h + scale_px(14, state.dpi);
            let card_tools_h = if state.video_card_tools_h.get() > 0 { state.video_card_tools_h.get() } else { scale_px(210, state.dpi) };
            let btn_h = scale_px(30, state.dpi);
            let btn_auto_w = scale_px(100, state.dpi);
            let btn_browse_w = scale_px(95, state.dpi);
            let btn_y = card_tools_y + scale_px(40, state.dpi);
            let btn_auto_x = content_x + content_w - scale_px(16, state.dpi) - btn_auto_w;
            let btn_browse_x = btn_auto_x - scale_px(8, state.dpi) - btn_browse_w;

            if y >= btn_y && y <= btn_y + btn_h {
                if x >= btn_browse_x && x <= btn_browse_x + btn_browse_w {
                    state.trigger_button_anim(hwnd, 340, false);
                    if let Some(path) = prompt_select_ffmpeg(hwnd) {
                        state.settings.custom_ffmpeg_path = path;
                        let _ = state.settings.save();
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                    return;
                } else if x >= btn_auto_x && x <= btn_auto_x + btn_auto_w {
                    state.trigger_button_anim(hwnd, 341, false);
                    if !state.settings.custom_ffmpeg_path.is_empty() {
                        state.settings.custom_ffmpeg_path.clear();
                        let _ = state.settings.save();
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                    return;
                }
            }

            // Everything expander card (below the FFmpeg card)
            let ev_rel = state.video_ev_card_rel_y.get();
            if ev_rel > 0 {
                let ev_card_y = card_tools_y + ev_rel;
                let ev_header_h = scale_px(52, state.dpi);
                let btn_rf_w = scale_px(100, state.dpi);
                let btn_rf_h = scale_px(26, state.dpi);
                let btn_rf_y = card_tools_y + state.video_everything_btn_rel_y.get();
                let chevron_x = content_x + content_w - scale_px(22, state.dpi) - scale_px(16, state.dpi);
                let btn_rf_x = chevron_x - btn_rf_w - scale_px(14, state.dpi);
                if y >= btn_rf_y && y <= btn_rf_y + btn_rf_h && x >= btn_rf_x && x <= btn_rf_x + btn_rf_w {
                    state.trigger_button_anim(hwnd, 342, false);
                    *state.cached_everything_version.borrow_mut() = None;
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                }
                if x >= content_x && x <= content_x + content_w && y >= ev_card_y && y <= ev_card_y + ev_header_h {
                    state.pressed_expander = Some(3);
                    state.pressed_expander_anim = 0.0;
                    state.pressed_expander_anim_start = Some(std::time::Instant::now());
                    let _ = SetTimer(hwnd, 1, 16, None);
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                }
                let indent = scale_px(EXPANDER_CONTENT_INDENT, state.dpi);
                if state.is_ev_expanded
                    && state.ev_expand_anim > 0.5
                    && handle_net_share_click(
                        hwnd,
                        state,
                        content_x + indent,
                        card_tools_y + state.video_net_sub_rel_y.get(),
                        content_w - indent * 2,
                        x,
                        y,
                    )
                {
                    return;
                }
            }

            // Card 3: Border preview click (card height is 130px, corner badge removed)
            let card3_y = card_tools_y + card_tools_h + scale_px(14, state.dpi);
            let card3_h = scale_px(130, state.dpi);
            handle_border_preview_click(hwnd, state, content_x, card3_y, content_w, 1, x, y);

            // Card 4 (Moved above Custom Algorithm Parameters): Dedicated HDR ACES Tone-Mapping Card
            let card_hdr_y = card3_y + card3_h + scale_px(14, state.dpi);
            let card_hdr_h = scale_px(86, state.dpi);
            let card_hdr_sw_w = scale_px(40, state.dpi);
            let card_hdr_sw_h = scale_px(20, state.dpi);
            let card_hdr_sw_x = content_x + content_w - card_hdr_sw_w - scale_px(52, state.dpi);
            let card_hdr_sw_y = card_hdr_y + scale_px(14, state.dpi);

            if x >= card_hdr_sw_x && x <= card_hdr_sw_x + card_hdr_sw_w && y >= card_hdr_sw_y && y <= card_hdr_sw_y + card_hdr_sw_h {
                commit_active_edit(state);
                state.settings.hdr_tonemap_enabled = !state.settings.hdr_tonemap_enabled;
                let _ = state.settings.save();
                trigger_toggle_animation(hwnd, state, 13, state.settings.hdr_tonemap_enabled);
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }

            // Card 5: Smart Rendering parameters container (Collapsible Sub-Card)
            let card4_y = card_hdr_y + card_hdr_h + scale_px(14, state.dpi);
            let _card4_h = video_card4_height(state.dpi, state.video_params_expand_anim);
            let chevron_w = scale_px(22, state.dpi);
            let chevron_x = content_x + content_w - chevron_w - scale_px(16, state.dpi);
            let switch_w = scale_px(40, state.dpi);
            let switch_x = chevron_x - switch_w - scale_px(14, state.dpi);

            // Restore defaults & Apply buttons next to header
            let reset_w = scale_px(95, state.dpi);
            let reset_h = scale_px(26, state.dpi);
            let reset_x = switch_x - reset_w - scale_px(12, state.dpi);
            let reset_y = card4_y + scale_px(13, state.dpi);

            let apply_w = scale_px(85, state.dpi);
            let apply_h = scale_px(26, state.dpi);
            let apply_x = reset_x - apply_w - scale_px(8, state.dpi);
            let apply_y = reset_y;

            // Check [ 套用 ] button click (ID 305)
            if state.settings.custom_video_settings && x >= apply_x && x <= apply_x + apply_w && y >= apply_y && y <= apply_y + apply_h {
                state.trigger_button_anim(hwnd, 305, false);
                commit_active_edit(state);
                state.has_unapplied_text_edits = false;
                state.applied_custom_timer = Some(std::time::Instant::now());
                let _ = state.settings.save();
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }

            // Check [ 🔄 回復預設 ] button click (ID 303)
            if state.settings.custom_video_settings && x >= reset_x && x <= reset_x + reset_w && y >= reset_y && y <= reset_y + reset_h {
                state.trigger_button_anim(hwnd, 303, false);
                state.active_edit = None;
                state.edit_caret_to_end();
                state.edit_buffer.clear();
                state.has_unapplied_text_edits = false;
                state.settings.position_seek = 25;
                state.settings.seek_mode = 2; // Default to Smart Duration
                state.settings.seek_fixed_sec = 120;
                state.settings.seek_threshold_min = 60;
                state.settings.step_probe_sec = 6;
                state.settings.max_gop_hops = 8;
                state.settings.consecutive_unacceptable_limit = 6;
                state.settings.luminance_enabled = true;
                state.settings.luminance_lower_limit = 40;
                state.settings.luminance_upper_limit = 80;
                state.settings.entropy_enabled = true;
                state.settings.entropy_lower_limit = 20;
                state.settings.watchdog_timeout_sec = 12;
                let _ = state.settings.save();
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }

            let cy = card4_y + scale_px(13, state.dpi);
            // Header: Custom video settings toggle
            if x >= switch_x && x <= switch_x + switch_w && y >= cy && y <= cy + scale_px(26, state.dpi) {
                commit_active_edit(state);
                state.settings.custom_video_settings = !state.settings.custom_video_settings;
                if state.settings.custom_video_settings && !state.is_video_params_expanded {
                    state.is_video_params_expanded = true;
                    state.video_params_anim_start = Some((std::time::Instant::now(), true));
                }
                let _ = state.settings.save();
                trigger_toggle_animation(hwnd, state, 10, state.settings.custom_video_settings);
                return;
            }

            // Header row expand/collapse click (press-and-hold; toggle on release)
            let header_end_y = card4_y + scale_px(52, state.dpi);
            if x >= content_x && x <= content_x + content_w && y >= card4_y && y <= header_end_y {
                state.pressed_expander = Some(2);
                state.pressed_expander_anim = 0.0;
                state.pressed_expander_anim_start = Some(std::time::Instant::now());
                let _ = SetTimer(hwnd, 1, 16, None);
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }

            let is_custom = state.settings.custom_video_settings;
            let body_switch_w = scale_px(42, state.dpi);

            if state.is_video_params_expanded && state.video_params_expand_anim > 0.5 {
            // Same inset as `render_video_page` uses for the expander body.
            let indent = scale_px(EXPANDER_CONTENT_INDENT, state.dpi);
            let content_x = content_x + indent;
            let content_w = content_w - indent * 2;
            let body_switch_x = content_x + content_w - scale_px(40, state.dpi) - scale_px(52, state.dpi) - scale_px(1, state.dpi);
            // Section 1: Frame selection policy
            let div1_y = card4_y + scale_px(52, state.dpi);
            let mut c1_y = div1_y + scale_px(10, state.dpi);
            let row_h = scale_px(32, state.dpi);

            // Row 1: Seek benchmark mode 3-segment buttons
            let seg_w = scale_px(90, state.dpi);
            let seg_h = scale_px(22, state.dpi);
            let seg_x1 = content_x + scale_px(250, state.dpi);
            let seg_x2 = seg_x1 + seg_w + scale_px(6, state.dpi);
            let seg_x3 = seg_x2 + seg_w + scale_px(6, state.dpi);

            if is_custom && y >= c1_y && y <= c1_y + seg_h {
                commit_active_edit(state);
                if x >= seg_x1 && x <= seg_x1 + seg_w && state.settings.seek_mode != 2 {
                    state.trigger_button_anim(hwnd, 322, true);
                    state.settings.seek_mode = 2;
                    let _ = state.settings.save();
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                } else if x >= seg_x2 && x <= seg_x2 + seg_w && state.settings.seek_mode != 0 {
                    state.trigger_button_anim(hwnd, 320, true);
                    state.settings.seek_mode = 0;
                    let _ = state.settings.save();
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                } else if x >= seg_x3 && x <= seg_x3 + seg_w && state.settings.seek_mode != 1 {
                    state.trigger_button_anim(hwnd, 321, true);
                    state.settings.seek_mode = 1;
                    let _ = state.settings.save();
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                }
            }
            c1_y += row_h;

            // Row 1a: Seek percentage slider
            if is_custom && state.settings.seek_mode != 1 && y >= c1_y && y <= c1_y + scale_px(24, state.dpi) {
                commit_active_edit(state);
                state.dragging_slider = Some(0);
                update_slider_value(state, 0, x);
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }
            c1_y += row_h;

            let box_w = scale_px(75, state.dpi);
            let box_h = scale_px(24, state.dpi);
            let box_x = content_x + scale_px(250, state.dpi);
            let btn_sz = scale_px(22, state.dpi);
            let minus_x = box_x + box_w + scale_px(32, state.dpi);
            let plus_x = minus_x + btn_sz + scale_px(4, state.dpi);

            // Row 1b: Fixed seconds input box + stepper
            let box1_y = c1_y + scale_px(2, state.dpi);
            let btn1_y = box1_y + (box_h - btn_sz) / 2;
            if is_custom && state.settings.seek_mode != 0 {
                if y >= box1_y && y <= box1_y + box_h && x >= box_x && x <= box_x + box_w {
                    if state.active_edit == Some(EditTarget::SeekFixedSec) {
                        // Second click on the focused box: drop the selection, keep the caret.
                        state.edit_caret_to_end();
                    } else {
                        commit_active_edit(state);
                        state.active_edit = Some(EditTarget::SeekFixedSec);
                        state.edit_buffer = state.settings.seek_fixed_sec.to_string();
                        state.edit_select_all_text();
                    }
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                } else if y >= btn1_y && y <= btn1_y + btn_sz {
                    if x >= minus_x && x <= minus_x + btn_sz {
                        state.trigger_button_anim(hwnd, 360, false);
                        state.settings.seek_fixed_sec = state.settings.seek_fixed_sec.saturating_sub(10).max(5);
                        if state.active_edit == Some(EditTarget::SeekFixedSec) {
                            state.edit_buffer = state.settings.seek_fixed_sec.to_string();
                            state.edit_caret_to_end();
                        }
                        let _ = state.settings.save();
                        let _ = InvalidateRect(hwnd, None, false);
                        return;
                    } else if x >= plus_x && x <= plus_x + btn_sz {
                        state.trigger_button_anim(hwnd, 361, false);
                        state.settings.seek_fixed_sec = (state.settings.seek_fixed_sec + 10).min(1800);
                        if state.active_edit == Some(EditTarget::SeekFixedSec) {
                            state.edit_buffer = state.settings.seek_fixed_sec.to_string();
                            state.edit_caret_to_end();
                        }
                        let _ = state.settings.save();
                        let _ = InvalidateRect(hwnd, None, false);
                        return;
                    }
                }
            }
            c1_y += row_h;

            // Row 1c: Duration threshold input box + stepper
            let box2_y = c1_y + scale_px(2, state.dpi);
            let btn2_y = box2_y + (box_h - btn_sz) / 2;
            if is_custom && state.settings.seek_mode == 2 {
                if y >= box2_y && y <= box2_y + box_h && x >= box_x && x <= box_x + box_w {
                    if state.active_edit == Some(EditTarget::SeekThresholdMin) {
                        // Second click on the focused box: drop the selection, keep the caret.
                        state.edit_caret_to_end();
                    } else {
                        commit_active_edit(state);
                        state.active_edit = Some(EditTarget::SeekThresholdMin);
                        state.edit_buffer = state.settings.seek_threshold_min.to_string();
                        state.edit_select_all_text();
                    }
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                } else if y >= btn2_y && y <= btn2_y + btn_sz {
                    if x >= minus_x && x <= minus_x + btn_sz {
                        state.trigger_button_anim(hwnd, 370, false);
                        state.settings.seek_threshold_min = state.settings.seek_threshold_min.saturating_sub(5).max(5);
                        if state.active_edit == Some(EditTarget::SeekThresholdMin) {
                            state.edit_buffer = state.settings.seek_threshold_min.to_string();
                            state.edit_caret_to_end();
                        }
                        let _ = state.settings.save();
                        let _ = InvalidateRect(hwnd, None, false);
                        return;
                    } else if x >= plus_x && x <= plus_x + btn_sz {
                        state.trigger_button_anim(hwnd, 371, false);
                        state.settings.seek_threshold_min = (state.settings.seek_threshold_min + 5).min(300);
                        if state.active_edit == Some(EditTarget::SeekThresholdMin) {
                            state.edit_buffer = state.settings.seek_threshold_min.to_string();
                            state.edit_caret_to_end();
                        }
                        let _ = state.settings.save();
                        let _ = InvalidateRect(hwnd, None, false);
                        return;
                    }
                }
            }
            c1_y += row_h;

            // Row 1d: Step probe
            let box_step_y = c1_y + scale_px(2, state.dpi);
            let btn_step_y = box_step_y + (box_h - btn_sz) / 2;
            if is_custom {
                if y >= box_step_y && y <= box_step_y + box_h && x >= box_x && x <= box_x + box_w {
                    if state.active_edit == Some(EditTarget::StepProbeSec) {
                        // Second click on the focused box: drop the selection, keep the caret.
                        state.edit_caret_to_end();
                    } else {
                        commit_active_edit(state);
                        state.active_edit = Some(EditTarget::StepProbeSec);
                        state.edit_buffer = state.settings.step_probe_sec.to_string();
                        state.edit_select_all_text();
                    }
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                } else if y >= btn_step_y && y <= btn_step_y + btn_sz {
                    if x >= minus_x && x <= minus_x + btn_sz {
                        state.trigger_button_anim(hwnd, 380, false);
                        state.settings.step_probe_sec = state.settings.step_probe_sec.saturating_sub(1).max(1);
                        if state.active_edit == Some(EditTarget::StepProbeSec) {
                            state.edit_buffer = state.settings.step_probe_sec.to_string();
                            state.edit_caret_to_end();
                        }
                        let _ = state.settings.save();
                        let _ = InvalidateRect(hwnd, None, false);
                        return;
                    } else if x >= plus_x && x <= plus_x + btn_sz {
                        state.trigger_button_anim(hwnd, 381, false);
                        state.settings.step_probe_sec = (state.settings.step_probe_sec + 1).min(60);
                        if state.active_edit == Some(EditTarget::StepProbeSec) {
                            state.edit_buffer = state.settings.step_probe_sec.to_string();
                            state.edit_caret_to_end();
                        }
                        let _ = state.settings.save();
                        let _ = InvalidateRect(hwnd, None, false);
                        return;
                    }
                }
            }
            c1_y += scale_px(58, state.dpi);

            // Row 1d2: Max GOP hops
            let box_gop_y = c1_y + scale_px(2, state.dpi);
            let btn_gop_y = box_gop_y + (box_h - btn_sz) / 2;
            if is_custom {
                if y >= box_gop_y && y <= box_gop_y + box_h && x >= box_x && x <= box_x + box_w {
                    if state.active_edit == Some(EditTarget::MaxGopHops) {
                        // Second click on the focused box: drop the selection, keep the caret.
                        state.edit_caret_to_end();
                    } else {
                        commit_active_edit(state);
                        state.active_edit = Some(EditTarget::MaxGopHops);
                        state.edit_buffer = state.settings.max_gop_hops.to_string();
                        state.edit_select_all_text();
                    }
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                } else if y >= btn_gop_y && y <= btn_gop_y + btn_sz {
                    if x >= minus_x && x <= minus_x + btn_sz {
                        state.trigger_button_anim(hwnd, 385, false);
                        state.settings.max_gop_hops = state.settings.max_gop_hops.saturating_sub(1);
                        if state.active_edit == Some(EditTarget::MaxGopHops) {
                            state.edit_buffer = state.settings.max_gop_hops.to_string();
                            state.edit_caret_to_end();
                        }
                        let _ = state.settings.save();
                        let _ = InvalidateRect(hwnd, None, false);
                        return;
                    } else if x >= plus_x && x <= plus_x + btn_sz {
                        state.trigger_button_anim(hwnd, 386, false);
                        state.settings.max_gop_hops = (state.settings.max_gop_hops + 1).min(20);
                        if state.active_edit == Some(EditTarget::MaxGopHops) {
                            state.edit_buffer = state.settings.max_gop_hops.to_string();
                            state.edit_caret_to_end();
                        }
                        let _ = state.settings.save();
                        let _ = InvalidateRect(hwnd, None, false);
                        return;
                    }
                }
            }
            c1_y += scale_px(58, state.dpi);

            // Row 1e: Consecutive limit
            let box_consec_y = c1_y + scale_px(2, state.dpi);
            let btn_consec_y = box_consec_y + (box_h - btn_sz) / 2;
            if is_custom {
                if y >= box_consec_y && y <= box_consec_y + box_h && x >= box_x && x <= box_x + box_w {
                    if state.active_edit == Some(EditTarget::ConsecutiveLimit) {
                        // Second click on the focused box: drop the selection, keep the caret.
                        state.edit_caret_to_end();
                    } else {
                        commit_active_edit(state);
                        state.active_edit = Some(EditTarget::ConsecutiveLimit);
                        state.edit_buffer = state.settings.consecutive_unacceptable_limit.to_string();
                        state.edit_select_all_text();
                    }
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                } else if y >= btn_consec_y && y <= btn_consec_y + btn_sz {
                    if x >= minus_x && x <= minus_x + btn_sz {
                        state.trigger_button_anim(hwnd, 390, false);
                        state.settings.consecutive_unacceptable_limit = state.settings.consecutive_unacceptable_limit.saturating_sub(1).max(1);
                        if state.active_edit == Some(EditTarget::ConsecutiveLimit) {
                            state.edit_buffer = state.settings.consecutive_unacceptable_limit.to_string();
                            state.edit_caret_to_end();
                        }
                        let _ = state.settings.save();
                        let _ = InvalidateRect(hwnd, None, false);
                        return;
                    } else if x >= plus_x && x <= plus_x + btn_sz {
                        state.trigger_button_anim(hwnd, 391, false);
                        state.settings.consecutive_unacceptable_limit = (state.settings.consecutive_unacceptable_limit + 1).min(20);
                        if state.active_edit == Some(EditTarget::ConsecutiveLimit) {
                            state.edit_buffer = state.settings.consecutive_unacceptable_limit.to_string();
                            state.edit_caret_to_end();
                        }
                        let _ = state.settings.save();
                        let _ = InvalidateRect(hwnd, None, false);
                        return;
                    }
                }
            }
            c1_y += scale_px(58, state.dpi);

            // Row 1f: Timeout limit
            let box_timeout_y = c1_y + scale_px(2, state.dpi);
            let btn_timeout_y = box_timeout_y + (box_h - btn_sz) / 2;
            if is_custom {
                if y >= box_timeout_y && y <= box_timeout_y + box_h && x >= box_x && x <= box_x + box_w {
                    if state.active_edit == Some(EditTarget::WatchdogTimeoutSec) {
                        // Second click on the focused box: drop the selection, keep the caret.
                        state.edit_caret_to_end();
                    } else {
                        commit_active_edit(state);
                        state.active_edit = Some(EditTarget::WatchdogTimeoutSec);
                        state.edit_buffer = state.settings.watchdog_timeout_sec.to_string();
                        state.edit_select_all_text();
                    }
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                } else if y >= btn_timeout_y && y <= btn_timeout_y + btn_sz {
                    if x >= minus_x && x <= minus_x + btn_sz {
                        state.trigger_button_anim(hwnd, 400, false);
                        state.settings.watchdog_timeout_sec = state.settings.watchdog_timeout_sec.saturating_sub(1).max(3);
                        if state.active_edit == Some(EditTarget::WatchdogTimeoutSec) {
                            state.edit_buffer = state.settings.watchdog_timeout_sec.to_string();
                            state.edit_caret_to_end();
                        }
                        let _ = state.settings.save();
                        let _ = InvalidateRect(hwnd, None, false);
                        return;
                    } else if x >= plus_x && x <= plus_x + btn_sz {
                        state.trigger_button_anim(hwnd, 401, false);
                        state.settings.watchdog_timeout_sec = (state.settings.watchdog_timeout_sec + 1).min(60);
                        if state.active_edit == Some(EditTarget::WatchdogTimeoutSec) {
                            state.edit_buffer = state.settings.watchdog_timeout_sec.to_string();
                            state.edit_caret_to_end();
                        }
                        let _ = state.settings.save();
                        let _ = InvalidateRect(hwnd, None, false);
                        return;
                    }
                }
            }
            c1_y += scale_px(58, state.dpi);

            // Section 2: Mean Luminance Filter
            let div2_y = c1_y + scale_px(10, state.dpi);
            let mut c2_y = div2_y + scale_px(8, state.dpi);

            if is_custom && x >= body_switch_x && x <= body_switch_x + body_switch_w && y >= c2_y && y <= c2_y + scale_px(26, state.dpi) {
                commit_active_edit(state);
                state.settings.luminance_enabled = !state.settings.luminance_enabled;
                let _ = state.settings.save();
                trigger_toggle_animation(hwnd, state, 11, state.settings.luminance_enabled);
                return;
            }
            c2_y += scale_px(32, state.dpi);

            // Row 2a: Lum low
            if is_custom && state.settings.luminance_enabled && y >= c2_y && y <= c2_y + scale_px(24, state.dpi) {
                commit_active_edit(state);
                state.dragging_slider = Some(1);
                update_slider_value(state, 1, x);
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }
            c2_y += scale_px(30, state.dpi);

            // Row 2b: Lum high
            if is_custom && state.settings.luminance_enabled && y >= c2_y && y <= c2_y + scale_px(24, state.dpi) {
                commit_active_edit(state);
                state.dragging_slider = Some(2);
                update_slider_value(state, 2, x);
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }
            c2_y += scale_px(30, state.dpi);

            // Section 3: Shannon Entropy Filter
            let div3_y = c2_y;
            let mut c3_y = div3_y + scale_px(8, state.dpi);

            if is_custom && x >= body_switch_x && x <= body_switch_x + body_switch_w && y >= c3_y && y <= c3_y + scale_px(26, state.dpi) {
                commit_active_edit(state);
                state.settings.entropy_enabled = !state.settings.entropy_enabled;
                let _ = state.settings.save();
                trigger_toggle_animation(hwnd, state, 12, state.settings.entropy_enabled);
                return;
            }
            c3_y += scale_px(32, state.dpi);

            // Row 3a: Entropy low
            if is_custom && state.settings.entropy_enabled && y >= c3_y && y <= c3_y + scale_px(24, state.dpi) {
                commit_active_edit(state);
                state.dragging_slider = Some(3);
                update_slider_value(state, 3, x);
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }
            } // End collapsible Card 4 body
        }
        ManagerTab::Audio => {
            let card1_y = scale_px(75, state.dpi) + offset_banner;
            let card1_h = get_checklist_card_height(MediaCategory::Audio, state.dpi);
            handle_checklist_click(hwnd, state, content_x, card1_y, content_w, MediaCategory::Audio, 200, x, y);

            let card2_y = card1_y + card1_h + scale_px(16, state.dpi);
            let card2_h = scale_px(145, state.dpi);
            handle_border_preview_click(hwnd, state, content_x, card2_y, content_w, 2, x, y);

            let card3_y = card2_y + card2_h + scale_px(16, state.dpi);
            let switch_w = scale_px(44, state.dpi);
            let switch_x = content_x + content_w - scale_px(40, state.dpi) - scale_px(52, state.dpi) - scale_px(2, state.dpi);
            let cy = card3_y + scale_px(15, state.dpi);

            if x >= switch_x && x <= switch_x + switch_w && y >= cy && y <= cy + scale_px(26, state.dpi) {
                state.settings.cover_art_enabled = !state.settings.cover_art_enabled;
                let _ = state.settings.save();
                trigger_toggle_animation(hwnd, state, 20, state.settings.cover_art_enabled);
                return;
            }
        }
        ManagerTab::Other => {
            let card1_y = scale_px(75, state.dpi) + offset_banner;
            let card1_h = get_checklist_card_height(MediaCategory::Other, state.dpi);
            handle_checklist_click(hwnd, state, content_x, card1_y, content_w, MediaCategory::Other, 200, x, y);

            // Card 2: Independent Syntax & Code Extensions (Default Off)
            let card2_y = card1_y + card1_h + scale_px(14, state.dpi);
            let card2_h = get_checklist_card_height(MediaCategory::Syntax, state.dpi);
            handle_checklist_click(hwnd, state, content_x, card2_y, content_w, MediaCategory::Syntax, 203, x, y);

            // Card 3: Custom Plain Text Extensions input box & apply button
            let card3_y = card2_y + card2_h + scale_px(14, state.dpi);
            let box_x = content_x + scale_px(16, state.dpi);
            let box_y = card3_y + scale_px(42, state.dpi);
            let btn_w = scale_px(96, state.dpi);
            let box_w = (content_w - scale_px(44, state.dpi) - btn_w).max(scale_px(180, state.dpi));
            let box_h = scale_px(30, state.dpi);
            let btn_x = box_x + box_w + scale_px(12, state.dpi);

            if y >= box_y && y <= box_y + box_h {
                if x >= box_x && x <= box_x + box_w {
                    if state.active_edit != Some(EditTarget::CustomTextExtensions) {
                        commit_active_edit(state);
                        state.active_edit = Some(EditTarget::CustomTextExtensions);
                        state.edit_buffer = state.settings.custom_text_extensions.clone();
                    }
                    state.edit_caret_to_end();
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                } else if x >= btn_x && x <= btn_x + btn_w {
                    state.trigger_button_anim(hwnd, 345, false);
                    if state.active_edit == Some(EditTarget::CustomTextExtensions) {
                        commit_active_edit(state);
                    } else {
                        state.settings.sanitize();
                    }
                    state.has_unapplied_text_edits = false;
                    let _ = state.settings.save();
                    apply_custom_text_associations_elevated(hwnd, &state.settings);
                    state.applied_custom_timer = Some(std::time::Instant::now());
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                }
            }
        }
        ManagerTab::Advanced => {
            let card1_y = scale_px(75, state.dpi) + offset_banner;
            let btn1_x = content_x + scale_px(16, state.dpi);
            let btn_y = card1_y + scale_px(48, state.dpi);
            let btn_w = scale_px(140, state.dpi);
            let btn_gap = scale_px(12, state.dpi);
            let btn_h = scale_px(32, state.dpi);

            // Button 1: Clear cache
            if x >= btn1_x && x <= btn1_x + btn_w && y >= btn_y && y <= btn_y + btn_h {
                state.trigger_button_anim(hwnd, 300, false);
                state.prewarm_shown = false;
                let (count, bytes) = clear_thumbnail_cache();
                state.cache_status_kind = CacheStatusKind::Cleared(count, bytes);
                state.cache_status_msg = state.get_cache_status_display();
                notify_shell_associations_changed();
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }

            // Button 2: Fill cache
            let btn2_x = btn1_x + btn_w + btn_gap;
            if x >= btn2_x && x <= btn2_x + btn_w && y >= btn_y && y <= btn_y + btn_h {
                state.trigger_button_anim(hwnd, 301, false);
                if state.prewarm.is_active() {
                    state.prewarm.cancel.store(true, std::sync::atomic::Ordering::Release);
                } else if let Some(folder) = pick_folder(hwnd) {
                    start_prewarm(state, folder);
                }
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }

            // Thumbnail size used for pre-generation (cycles 96 / 256 / 768).
            let (size_x, size_y, size_w, size_h) = prewarm_size_button_geom(state, content_x, content_w, card1_y);
            if x >= size_x && x <= size_x + size_w && y >= size_y && y <= size_y + size_h {
                if !state.prewarm.is_active() {
                    state.trigger_button_anim(hwnd, 303, false);
                    state.prewarm_size = match state.prewarm_size {
                        96 => 256,
                        256 => 768,
                        _ => 96,
                    };
                    let _ = InvalidateRect(hwnd, None, false);
                }
                return;
            }

            // Button 3: Repair associations
            let btn3_x = btn2_x + btn_w + btn_gap;
            let btn3_w = scale_px(195, state.dpi);
            if x >= btn3_x && x <= btn3_x + btn3_w && y >= btn_y && y <= btn_y + btn_h {
                state.trigger_button_anim(hwnd, 302, false);
                let mut target_exts: Vec<String> = SUPPORTED_MEDIA_FORMATS
                    .iter()
                    .filter(|fmt| fmt.default_on || is_associated(HKEY_CURRENT_USER, fmt.ext))
                    .map(|f| f.ext.to_string())
                    .collect();
                for ext in state.settings.parsed_custom_text_extensions() {
                    let dot_ext = format!(".{ext}");
                    if !target_exts.iter().any(|e| e.eq_ignore_ascii_case(&dot_ext)) {
                        target_exts.push(dot_ext);
                    }
                }
                let is_admin = cbx_core::registry::is_elevated();
                if !is_admin {
                    let mut spec_items: Vec<String> = Vec::new();
                    for fmt in SUPPORTED_MEDIA_FORMATS {
                        if fmt.default_on || is_associated(HKEY_CURRENT_USER, fmt.ext) {
                            let tip = if state.settings.enable_infotip && fmt.category == MediaCategory::ImageArchive { "1" } else { "0" };
                            spec_items.push(format!("{}:1:{}", fmt.ext, tip));
                        }
                    }
                    for ext in state.settings.parsed_custom_text_extensions() {
                        let dot_ext = format!(".{ext}");
                        if !SUPPORTED_MEDIA_FORMATS.iter().any(|f| f.ext.eq_ignore_ascii_case(&dot_ext)) {
                            spec_items.push(format!("{}:1:0", dot_ext));
                        }
                    }
                    let spec_arg = format!("--apply-associations {}", spec_items.join(";"));
                    let cli_path = std::env::current_exe()
                        .ok()
                        .and_then(|p| p.parent().map(|d| d.join("cbx-cli.exe")))
                        .unwrap_or_else(|| PathBuf::from("cbx-cli.exe"));

                    let success = runas_elevated(hwnd, &cli_path, &spec_arg);
                    if success {
                        state.cache_status_msg = format!("🛡️ {}", tr("已獲系統管理員權限修復所有副檔名關聯 (HKLM & HKCU)", "Repaired all associations with Administrator privileges (HKLM & HKCU)"));
                    } else {
                        let refs: Vec<&str> = target_exts.iter().map(|s| s.as_str()).collect();
                        let _ = repair_all_associations(HKEY_CURRENT_USER, &refs, state.settings.enable_infotip);
                        state.cache_status_msg = format!("⚠️ {}", tr("未獲管理員權限，已僅修復目前使用者 (HKCU)", "UAC cancelled, repaired Current User (HKCU) only"));
                    }
                } else {
                    let refs: Vec<&str> = target_exts.iter().map(|s| s.as_str()).collect();
                    let _ = repair_all_associations(HKEY_LOCAL_MACHINE, &refs, state.settings.enable_infotip);
                    state.cache_status_msg = format!("🛡️ {}", tr("已成功修復所有副檔名關聯 (HKLM & HKCU)", "Successfully repaired all associations (HKLM & HKCU)"));
                }
                notify_shell_associations_changed();
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }

            // Checkboxes row
            let chk_y = btn_y + btn_h + scale_px(14, state.dpi);
            let chk1_x = btn1_x;
            let chk2_x = chk1_x + scale_px(170, state.dpi);

            if y >= chk_y && y <= chk_y + scale_px(22, state.dpi) {
                if x >= chk1_x && x <= chk1_x + scale_px(150, state.dpi) {
                    state.cache_include_network = !state.cache_include_network;
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                } else if x >= chk2_x && x <= chk2_x + scale_px(140, state.dpi) {
                    state.cache_force_refresh = !state.cache_force_refresh;
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                }
            }

            // Card 2: Live Monitor Controls (Clear Log & Enable Monitor toggle)
            let card1_h = scale_px(125, state.dpi);
            let card2_y = card1_y + card1_h + scale_px(14, state.dpi);
            let header_h = scale_px(44, state.dpi);

            let sw_card_w = scale_px(210, state.dpi);
            let sw_x = content_x + content_w - sw_card_w;
            let sw_y = card2_y + scale_px(8, state.dpi);
            let sw_h = scale_px(24, state.dpi);

            // Lock button left of the toggle: keep logging on instead of auto-off after 10 min.
            let (lock_x, lock_w) = monitor_lock_button_geom(state, sw_x);
            if state.settings.monitor_enabled && x >= lock_x && x <= lock_x + lock_w && y >= sw_y && y <= sw_y + sw_h {
                state.trigger_button_anim(hwnd, 97, false);
                state.settings.monitor_locked = !state.settings.monitor_locked;
                if !state.settings.monitor_locked {
                    // Unlocking starts a fresh 10-minute window.
                    state.settings.monitor_enabled_at = unix_now().min(u32::MAX as u64) as u32;
                }
                let _ = state.settings.save();
                state.monitor_left_min = None;
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }

            if x >= sw_x && x <= sw_x + sw_card_w && y >= sw_y && y <= sw_y + sw_h {
                state.settings.monitor_enabled = !state.settings.monitor_enabled;
                state.settings.monitor_locked = false;
                if state.settings.monitor_enabled {
                    state.settings.monitor_enabled_at = unix_now().min(u32::MAX as u64) as u32;
                }
                let is_expanding = state.settings.monitor_enabled;
                state.monitor_anim_start = Some((std::time::Instant::now(), is_expanding));
                let _ = SetTimer(hwnd, 1, 16, None);
                let _ = state.settings.save();
                trigger_toggle_animation(hwnd, state, 99, state.settings.monitor_enabled);
                if !state.settings.monitor_enabled {
                    cbx_core::monitor::clear_all_history();
                    state.monitor_snapshot = None;
                    state.monitor_scroll = 0;
                }
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }

            // Monitor Status Filter Buttons & Clear Log Button in the metrics row
            let pad_x = scale_px(16, state.dpi);
            let top_y = card2_y + header_h + scale_px(4, state.dpi);
            let banner_h = scale_px(38, state.dpi);
            let metrics_y = top_y + banner_h + scale_px(8, state.dpi);

            let clr_w = scale_px(85, state.dpi);
            let clr_h = scale_px(20, state.dpi);
            let clr_x = content_x + content_w - pad_x - clr_w;

            let dd_h = scale_px(20, state.dpi);
            let dd_gap = scale_px(6, state.dpi);
            let status_dd_w = scale_px(98, state.dpi);
            let pipe_dd_w = scale_px(104, state.dpi);
            let exp_w = scale_px(85, state.dpi);
            let exp_x = clr_x - scale_px(6, state.dpi) - exp_w;
            let status_dd_x = exp_x - scale_px(17, state.dpi) - status_dd_w;
            let pipe_dd_x = status_dd_x - dd_gap - pipe_dd_w;

            if state.settings.monitor_enabled && state.monitor_expand_anim > 0.8 {
                if x >= exp_x && x <= exp_x + exp_w && y >= metrics_y && y <= metrics_y + clr_h {
                    state.trigger_button_anim(hwnd, 306, false);
                    export_activity_log(hwnd, state);
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                }
                if x >= clr_x && x <= clr_x + clr_w && y >= metrics_y && y <= metrics_y + clr_h {
                    state.trigger_button_anim(hwnd, 304, false);
                    cbx_core::monitor::clear_all_history();
                    state.monitor_snapshot = None;
                    state.monitor_scroll = 0;
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                }

                if y >= metrics_y - scale_px(3, state.dpi) && y <= metrics_y + dd_h + scale_px(3, state.dpi) {
                    if x >= pipe_dd_x && x <= pipe_dd_x + pipe_dd_w {
                        state.pressed_monitor_dropdown = Some(0);
                        state.hover_monitor_dropdown_item = None;
                        let _ = InvalidateRect(hwnd, None, false);
                        return;
                    }
                    if x >= status_dd_x && x <= status_dd_x + status_dd_w {
                        state.pressed_monitor_dropdown = Some(1);
                        state.hover_monitor_dropdown_item = None;
                        let _ = InvalidateRect(hwnd, None, false);
                        return;
                    }
                }
            }

            // Check row click in monitor history to expand full details modal
            if state.settings.monitor_enabled {
                let rows_start_y = metrics_y + scale_px(40, state.dpi);
                let row_h = scale_px(23, state.dpi);
                let bottom_limit = card2_y + state.monitor_card_h - scale_px(22, state.dpi);

                if let Some(ref snap) = state.monitor_snapshot {
                    let filtered_history: Vec<_> = snap
                        .history
                        .iter()
                        .filter(|item| monitor_filter_matches(item, state.monitor_filter, state.monitor_pipeline_filter))
                        .cloned()
                        .collect();

                    let visible_rows = (((bottom_limit - rows_start_y).max(0) / row_h).max(1) as usize).min(30);
                    let scroll_offset = state.monitor_scroll.min(filtered_history.len().saturating_sub(1));

                    for (idx, item) in filtered_history.iter().skip(scroll_offset).take(visible_rows).enumerate() {
                        let ry = rows_start_y + (idx as i32 * row_h);
                        if ry + row_h > bottom_limit {
                            break;
                        }
                        if y >= ry && y < ry + row_h && x >= content_x + pad_x && x <= content_x + content_w - pad_x {
                            state.expanded_monitor_item = Some(item.clone());
                            let _ = InvalidateRect(hwnd, None, false);
                            return;
                        }
                    }
                }
            }

            // Card 2 bottom resizer drag handle
            let resizer_y = card2_y + state.monitor_card_h - scale_px(10, state.dpi);
            if y >= resizer_y && y <= resizer_y + scale_px(16, state.dpi) {
                state.is_dragging_resizer = true;
                state.drag_resizer_start_y = y;
                state.drag_resizer_start_h = state.monitor_card_h;
                return;
            }

            // Card 3: Drag & Drop Experiment Area "Update this file's thumbnail" button (ID 309)
            let card2_h = if !state.settings.monitor_enabled {
                header_h
            } else {
                header_h + (((state.monitor_card_h - header_h) as f32) * state.monitor_expand_anim) as i32
            };
            let drop_y = card2_y + card2_h + scale_px(14, state.dpi);
            let prev_w = scale_px(140, state.dpi);
            let prev_x = content_x + content_w - prev_w - scale_px(16, state.dpi);
            let upd_w = scale_px(136, state.dpi);
            let upd_h = scale_px(24, state.dpi);
            let upd_x = prev_x - upd_w - scale_px(12, state.dpi);
            let upd_y = drop_y + scale_px(10, state.dpi);
            if x >= upd_x && x <= upd_x + upd_w && y >= upd_y && y <= upd_y + upd_h {
                if let Some(ref target_path) = state.test_path.clone() {
                    state.trigger_button_anim(hwnd, 309, false);
                    let fname = target_path.file_name().and_then(|s| s.to_str()).unwrap_or("file").to_string();
                    match cbx_core::registry::update_single_file_shell_thumbnail(target_path) {
                        Ok(()) => {
                            state.cache_status_kind = CacheStatusKind::Custom(format!(
                                "✓ {} {}",
                                tr("已更新單一檔案縮圖快取:", "Updated thumbnail cache for:"),
                                fname
                            ));
                            state.cache_status_msg = state.get_cache_status_display();
                        }
                        Err(e) => {
                            state.cache_status_kind = CacheStatusKind::Custom(format!("✗ {e}"));
                            state.cache_status_msg = state.get_cache_status_display();
                        }
                    }
                    let _ = InvalidateRect(hwnd, None, false);
                    return;
                }
            }
        }
        ManagerTab::About => {
            let card1_y = scale_px(75, state.dpi) + offset_banner;
            let upd_y = card1_y + state.about_card1_h.get() + scale_px(14, state.dpi);
            for b in about_update_buttons(state, content_x, content_w, upd_y) {
                if b.disabled || x < b.x || x > b.x + b.w || y < b.y || y > b.y + b.h {
                    continue;
                }
                state.trigger_button_anim(hwnd, b.id, false);
                handle_update_button(hwnd, state, b.id);
                let _ = InvalidateRect(hwnd, None, false);
                return;
            }
        }
    }
}

fn runas_elevated(parent_hwnd: HWND, exe_path: &Path, args: &str) -> bool {
    let exe_w = to_wide(&exe_path.to_string_lossy());
    let args_w = to_wide(args);
    let verb_w = to_wide("runas");

    let mut sei = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd: parent_hwnd,
        lpVerb: PCWSTR(verb_w.as_ptr()),
        lpFile: PCWSTR(exe_w.as_ptr()),
        lpParameters: PCWSTR(args_w.as_ptr()),
        lpDirectory: PCWSTR::null(),
        nShow: SW_HIDE.0 as i32,
        hInstApp: HINSTANCE::default(),
        lpIDList: std::ptr::null_mut(),
        lpClass: PCWSTR::null(),
        hkeyClass: windows::Win32::System::Registry::HKEY::default(),
        dwHotKey: 0,
        Anonymous: Default::default(),
        hProcess: windows::Win32::Foundation::HANDLE::default(),
    };

    unsafe {
        if ShellExecuteExW(&mut sei).is_ok() && !sei.hProcess.is_invalid() {
            let _ = windows::Win32::System::Threading::WaitForSingleObject(
                sei.hProcess,
                windows::Win32::System::Threading::INFINITE,
            );
            let mut exit_code = 0u32;
            let _ = windows::Win32::System::Threading::GetExitCodeProcess(sei.hProcess, &mut exit_code);
            let _ = windows::Win32::Foundation::CloseHandle(sei.hProcess);
            exit_code == 0
        } else {
            false
        }
    }
}

unsafe fn handle_checklist_click(
    hwnd: HWND,
    state: &mut AppState,
    card_x: i32,
    card_y: i32,
    card_w: i32,
    cat: MediaCategory,
    btn_base_id: u32,
    click_x: i32,
    click_y: i32,
) {
    let formats: Vec<&MediaFormatInfo> = SUPPORTED_MEDIA_FORMATS
        .iter()
        .filter(|fmt| fmt.category == cat)
        .collect();

    let cols = 5;
    let col_w = (card_w - scale_px(40, state.dpi)) / cols;
    let item_h = scale_px(26, state.dpi);
    let start_x = card_x + scale_px(20, state.dpi);
    let start_y = card_y + scale_px(45, state.dpi);

    // 1. Check individual extension item clicks
    for (i, fmt) in formats.iter().enumerate() {
        let col = (i % cols as usize) as i32;
        let row = (i / cols as usize) as i32;
        let ix = start_x + col * col_w;
        let iy = start_y + row * item_h;

        if click_x >= ix && click_x <= ix + col_w - scale_px(10, state.dpi) && click_y >= iy && click_y <= iy + item_h {
            let current = match state.pending_associations.get(fmt.ext) {
                Some(&v) => v,
                None => is_associated(HKEY_CURRENT_USER, fmt.ext),
            };
            state.pending_associations.insert(fmt.ext.to_string(), !current);
            let _ = InvalidateRect(hwnd, None, false);
            return;
        }
    }

    // 2. Check "全選", "全不選", and "套用" buttons
    let total_rows = (formats.len() as i32 + cols - 1) / cols;
    let btn_y = start_y + total_rows * item_h + scale_px(8, state.dpi);
    let btn_w = scale_px(95, state.dpi);
    let btn_gap = scale_px(12, state.dpi);
    let btn_h = scale_px(28, state.dpi);

    let sel_all_x = start_x;
    let desel_x = start_x + btn_w + btn_gap;
    let apply_x = desel_x + btn_w + btn_gap;

    if click_y >= btn_y && click_y <= btn_y + btn_h {
        if click_x >= sel_all_x && click_x <= sel_all_x + btn_w {
            state.trigger_button_anim(hwnd, btn_base_id, false);
            for fmt in &formats {
                state.pending_associations.insert(fmt.ext.to_string(), true);
            }
            let _ = InvalidateRect(hwnd, None, false);
        } else if click_x >= desel_x && click_x <= desel_x + btn_w {
            state.trigger_button_anim(hwnd, btn_base_id + 1, false);
            for fmt in &formats {
                state.pending_associations.insert(fmt.ext.to_string(), false);
            }
            let _ = InvalidateRect(hwnd, None, false);
        } else if click_x >= apply_x && click_x <= apply_x + btn_w {
            state.trigger_button_anim(hwnd, btn_base_id + 2, true);

            let is_admin = cbx_core::registry::is_elevated();
            if !is_admin {
                // Prepare spec for elevated helper
                let mut spec_items: Vec<String> = Vec::new();
                for fmt in &formats {
                    let should_assoc = match state.pending_associations.get(fmt.ext) {
                        Some(&v) => v,
                        None => is_associated(HKEY_CURRENT_USER, fmt.ext),
                    };
                    let enable_tip = state.settings.enable_infotip && fmt.category == MediaCategory::ImageArchive;
                    spec_items.push(format!("{}:{}:{}", fmt.ext, if should_assoc { "1" } else { "0" }, if enable_tip { "1" } else { "0" }));
                }
                let spec_arg = format!("--apply-associations {}", spec_items.join(";"));

                let cli_path = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("cbx-cli.exe")))
                    .unwrap_or_else(|| PathBuf::from("cbx-cli.exe"));

                let success = runas_elevated(hwnd, &cli_path, &spec_arg);
                if success {
                    state.cache_status_msg = format!("🛡️ {}", tr("已獲系統管理員權限套用所有關聯 (HKLM & HKCU)", "Applied with Administrator privileges (HKLM & HKCU)"));
                } else {
                    // Fallback to HKCU if user cancelled UAC prompt
                    for fmt in &formats {
                        let should_assoc = match state.pending_associations.get(fmt.ext) {
                            Some(&v) => v,
                            None => is_associated(HKEY_CURRENT_USER, fmt.ext),
                        };
                        let enable_tip = state.settings.enable_infotip && fmt.category == MediaCategory::ImageArchive;
                        if should_assoc {
                            associate_extension(HKEY_CURRENT_USER, fmt.ext, enable_tip);
                        } else {
                            disassociate_extension(HKEY_CURRENT_USER, fmt.ext);
                        }
                    }
                    notify_shell_associations_changed();
                    state.cache_status_msg = format!("⚠️ {}", tr("未獲管理員權限，已僅套用於目前使用者 (HKCU)", "UAC cancelled, applied to Current User only"));
                }
            } else {
                // Already elevated, write both HKCU and HKLM directly
                let mut count = 0usize;
                for fmt in &formats {
                    let should_associate = match state.pending_associations.get(fmt.ext) {
                        Some(&v) => v,
                        None => is_associated(HKEY_CURRENT_USER, fmt.ext),
                    };
                    if should_associate {
                        let enable_tip = state.settings.enable_infotip && fmt.category == MediaCategory::ImageArchive;
                        associate_extension(HKEY_CURRENT_USER, fmt.ext, enable_tip);
                        associate_extension(windows::Win32::System::Registry::HKEY_LOCAL_MACHINE, fmt.ext, enable_tip);
                        count += 1;
                    } else {
                        disassociate_extension(HKEY_CURRENT_USER, fmt.ext);
                        disassociate_extension(windows::Win32::System::Registry::HKEY_LOCAL_MACHINE, fmt.ext);
                    }
                }
                notify_shell_associations_changed();
                state.cache_status_msg = format!(
                    "🛡️ {} ({} {})",
                    tr("已全域套用副檔名關聯 (HKLM & HKCU)", "Global associations applied (HKLM & HKCU)"),
                    count,
                    tr("格式", "formats")
                );
            }
            let _ = InvalidateRect(hwnd, None, false);
        }
    }
}

unsafe fn handle_border_preview_click(
    hwnd: HWND,
    state: &mut AppState,
    card_x: i32,
    card_y: i32,
    _card_w: i32,
    target_kind: usize, // 0 = Archive, 1 = Video, 2 = Audio, 3 = Ebook
    click_x: i32,
    click_y: i32,
) {
    let b_y = card_y + scale_px(42, state.dpi);
    let b_w = scale_px(70, state.dpi);
    let b_h = scale_px(32, state.dpi);
    let b_gap = scale_px(5, state.dpi);

    for (idx, (style_id, _, _)) in BORDER_STYLE_OPTIONS.iter().enumerate() {
        let bx = card_x + scale_px(16, state.dpi) + (idx as i32 * (b_w + b_gap));
        if click_x >= bx && click_x <= bx + b_w && click_y >= b_y && click_y <= b_y + b_h {
            state.trigger_button_anim(hwnd, 210 + idx as u32, true);
            match target_kind {
                0 => state.settings.border_archive = *style_id,
                1 => state.settings.border_video = *style_id,
                2 => state.settings.border_audio = *style_id,
                3 => state.settings.border_ebook = *style_id,
                _ => {}
            }
            let _ = state.settings.save();
            let _ = InvalidateRect(hwnd, None, false);
            return;
        }
    }

    // Corner badge checkbox click (Checkbox on the left; E-Book uses opening app icon instead)
    if target_kind != 3 {
        let chk_x = card_x + scale_px(16, state.dpi);
        let badge_y = card_y + scale_px(95, state.dpi);
        let chk_w = scale_px(350, state.dpi);
        let chk_h = scale_px(24, state.dpi);
        if click_x >= chk_x && click_x <= chk_x + chk_w && click_y >= badge_y && click_y <= badge_y + chk_h {
            match target_kind {
                0 => state.settings.badge_static = !state.settings.badge_static,
                1 => state.settings.badge_video = !state.settings.badge_video,
                2 => state.settings.badge_audio = !state.settings.badge_audio,
                _ => {}
            }
            let _ = state.settings.save();
            let _ = InvalidateRect(hwnd, None, false);
        }
    }
}

unsafe fn handle_drop_files(hwnd: HWND, hdrop: HDROP) {
    let Some(mut state_guard) = state_lock() else { return };
    let state: &mut AppState = &mut state_guard;
    let mut buf = [0u16; 512];
    let count = DragQueryFileW(hdrop, 0xFFFFFFFF, None);
    if count > 0 {
        let len = DragQueryFileW(hdrop, 0, Some(&mut buf)) as usize;
        let path_str = String::from_utf16_lossy(&buf[..len]);
        let path = PathBuf::from(path_str);

        if path.is_file() {
            inspect_file_live(state, &path);
            state.active_tab = ManagerTab::Advanced;
            let _ = InvalidateRect(hwnd, None, false);
        } else if path.is_dir() {
            state.active_tab = ManagerTab::Advanced;
            start_prewarm(state, path);
            let _ = InvalidateRect(hwnd, None, false);
        }
    }
    DragFinish(hdrop);
}

unsafe fn handle_mouse_wheel(hwnd: HWND, delta: i32) {
    if let Some(mut state_guard) = state_lock() {
        let state: &mut AppState = &mut state_guard;
        if state.active_tab == ManagerTab::Advanced {
            let total_history = state.monitor_snapshot.as_ref().map(|s| s.history.len()).unwrap_or(0);
            let max_scroll = total_history.saturating_sub(6);
            if delta > 0 {
                // Scroll up (newer items)
                state.monitor_scroll = state.monitor_scroll.saturating_sub(1);
            } else if delta < 0 {
                // Scroll down (older items)
                if state.monitor_scroll < max_scroll {
                    state.monitor_scroll += 1;
                }
            }
            let _ = InvalidateRect(hwnd, None, false);
        } else if state.max_scroll_y > 0 {
            let scroll_step = scale_px(50, state.dpi);
            if delta > 0 {
                state.scroll_y = (state.scroll_y - scroll_step).max(0);
            } else {
                state.scroll_y = (state.scroll_y + scroll_step).min(state.max_scroll_y);
            }
            let _ = InvalidateRect(hwnd, None, false);
        }
    }
}

fn inspect_file_live(state: &mut AppState, path: &Path) {
    state.test_path = Some(path.to_path_buf());
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("unknown");
    let ext = path.extension().and_then(|s| s.to_str()).map(|e| format!(".{}", e.to_ascii_lowercase())).unwrap_or_default();

    let full_path_str = path.to_string_lossy();
    let guard = cbx_core::monitor::record_task_begin(&full_path_str, &ext);
    let t_start = std::time::Instant::now();

    if is_video_extension(&ext) {
        match cbx_core::media::video_mf::extract_video_thumbnail_from_path(path, 256, &state.settings.smart_render_config()) {
            Ok((mut rgba, rep)) => {
                let elapsed_ms = t_start.elapsed().as_millis() as u32;
                // Apply selected border style & corner badge
                rgba = cbx_core::badge::apply_border_decoration(&rgba, state.settings.border_video_enum());
                if state.settings.badge_video {
                    let lbl = ext.trim_start_matches('.').to_ascii_uppercase();
                    cbx_core::badge::draw_media_corner_badge(&mut rgba, &lbl, true);
                }
                let details = cbx_core::i18n::t_fmt(
                    "details.video_experiment_metrics",
                    &[
                        ("w", &rep.original_width.to_string()),
                        ("h", &rep.original_height.to_string()),
                        ("ms", &elapsed_ms.to_string()),
                        ("lum", &format!("{:.0}", rep.metrics.luminance_percent)),
                        ("ent", &format!("{:.0}", rep.metrics.entropy_percent)),
                    ],
                );
                state.last_test_result = LastTestResult::Video {
                    fname: fname.to_string(),
                    elapsed_ms,
                    attempts: rep.attempts_made,
                    time_sec: rep.selected_time_seconds,
                    duration_sec: rep.duration_seconds,
                    orig_w: rep.original_width,
                    orig_h: rep.original_height,
                    out_w: rgba.width(),
                    out_h: rgba.height(),
                    lum: rep.metrics.luminance_percent,
                    entropy: rep.metrics.entropy_percent,
                    score: rep.metrics.score,
                    is_accepted: rep.metrics.is_accepted,
                };
                state.test_metrics_str = state.get_test_metrics_display();
                let bgra = cbx_core::rgba_to_bgra(&rgba);
                state.test_thumb_dims = (rgba.width(), rgba.height());
                state.test_thumb = cbx_core::image_dec::create_hbitmap_from_bgra(rgba.width(), rgba.height(), &bgra).ok();
                if let Some(g) = guard {
                    cbx_core::monitor::record_task_end(g, true, &i18n::t("pipeline.direct_file"), &details);
                }
            }
            Err(e) => {
                let _elapsed_ms = t_start.elapsed().as_millis() as u32;
                let err_msg = e.to_string();
                state.last_test_result = LastTestResult::Error {
                    fname: fname.to_string(),
                    err_msg: err_msg.clone(),
                };
                state.test_metrics_str = state.get_test_metrics_display();
                state.test_thumb = None;
                state.test_thumb_dims = (0, 0);
                if let Some(g) = guard {
                    cbx_core::monitor::record_task_end(g, false, &i18n::t("pipeline.direct_file"), &err_msg);
                }
            }
        }
    } else if is_audio_extension(&ext) {
        if let Ok(mut f) = std::fs::File::open(path) {
            let elapsed_ms = t_start.elapsed().as_millis() as u32;
            if let Some(cover_data) = extract_embedded_cover(&mut f) {
                if let Ok((bgra, w, h)) = cbx_core::decode_simple_image(&cover_data, 256) {
                    state.test_thumb_dims = (w, h);
                    state.test_thumb = cbx_core::image_dec::create_hbitmap_from_bgra(w, h, &bgra).ok();
                }
                state.last_test_result = LastTestResult::Audio {
                    fname: fname.to_string(),
                    elapsed_ms,
                    has_cover: true,
                    cover_len: cover_data.len(),
                };
                state.test_metrics_str = state.get_test_metrics_display();
                if let Some(g) = guard {
                    let msg = cbx_core::i18n::t_fmt("details.embedded_cover_bytes", &[("bytes", &cover_data.len().to_string())]);
                    cbx_core::monitor::record_task_end(g, true, &i18n::t("pipeline.direct_file"), &msg);
                }
            } else {
                let label = cbx_core::static_ext_label(&ext);
                let mut vinyl = cbx_core::generate_procedural_audio_card(256, label);
                vinyl = cbx_core::badge::apply_border_decoration(&vinyl, state.settings.border_audio_enum());
                let bgra = cbx_core::rgba_to_bgra(&vinyl);
                state.test_thumb_dims = (vinyl.width(), vinyl.height());
                state.test_thumb = cbx_core::image_dec::create_hbitmap_from_bgra(vinyl.width(), vinyl.height(), &bgra).ok();
                state.last_test_result = LastTestResult::Audio {
                    fname: fname.to_string(),
                    elapsed_ms,
                    has_cover: false,
                    cover_len: 0,
                };
                state.test_metrics_str = state.get_test_metrics_display();
                if let Some(g) = guard {
                    cbx_core::monitor::record_task_end(g, true, &i18n::t("pipeline.direct_file"), &i18n::t("details.no_embedded_cover"));
                }
            }
        } else if let Some(g) = guard {
            cbx_core::monitor::record_task_end(g, false, &i18n::t("pipeline.direct_file"), &i18n::t("details.cannot_open_file"));
        }
    } else if is_ebook_extension(&ext) {
        if let Ok(mut f) = std::fs::File::open(path) {
            match cbx_core::create_thumbnail_from_reader_unified(&mut f, 256, &state.settings, Some(&fname)) {
                Ok((hbmp, summary)) => {
                    let elapsed_ms = t_start.elapsed().as_millis() as u32;
                    let (out_w, out_h) = summary
                        .split(',')
                        .next()
                        .and_then(|s| s.trim().split_once('x'))
                        .and_then(|(w_s, h_s)| Some((w_s.trim().parse::<u32>().ok()?, h_s.trim().parse::<u32>().ok()?)))
                        .unwrap_or((256, 256));
                    state.test_thumb_dims = (out_w, out_h);
                    state.test_thumb = Some(hbmp);
                    state.last_test_result = LastTestResult::Ebook {
                        fname: fname.to_string(),
                        elapsed_ms,
                        entry_name: summary.clone(),
                        out_w,
                        out_h,
                        fmt: ext.trim_start_matches('.').to_ascii_uppercase(),
                    };
                    state.test_metrics_str = state.get_test_metrics_display();
                    if let Some(g) = guard {
                        cbx_core::monitor::record_task_end(g, true, &i18n::t("pipeline.direct_file"), &summary);
                    }
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    state.last_test_result = LastTestResult::Error {
                        fname: fname.to_string(),
                        err_msg: err_msg.clone(),
                    };
                    state.test_metrics_str = state.get_test_metrics_display();
                    state.test_thumb = None;
                    state.test_thumb_dims = (0, 0);
                    if let Some(g) = guard {
                        cbx_core::monitor::record_task_end(g, false, &i18n::t("pipeline.direct_file"), &err_msg);
                    }
                }
            }
        } else if let Some(g) = guard {
            cbx_core::monitor::record_task_end(g, false, &i18n::t("pipeline.direct_file"), &i18n::t("details.cannot_open_file"));
        }
    } else if is_other_or_custom_text_extension(&ext, &state.settings.parsed_custom_text_extensions()) {
        if let Ok(mut f) = std::fs::File::open(path) {
            match cbx_core::create_thumbnail_from_reader_unified(&mut f, 256, &state.settings, Some(&fname)) {
                Ok((hbmp, summary)) => {
                    let elapsed_ms = t_start.elapsed().as_millis() as u32;
                    let (out_w, out_h) = summary
                        .split(',')
                        .next()
                        .and_then(|s| s.trim().split_once('x'))
                        .and_then(|(w_s, h_s)| Some((w_s.trim().parse::<u32>().ok()?, h_s.trim().parse::<u32>().ok()?)))
                        .unwrap_or((256, 256));
                    state.test_thumb_dims = (out_w, out_h);
                    state.test_thumb = Some(hbmp);
                    state.last_test_result = LastTestResult::Other {
                        fname: fname.to_string(),
                        elapsed_ms,
                        entry_name: summary.clone(),
                        out_w,
                        out_h,
                        fmt: ext.trim_start_matches('.').to_ascii_uppercase(),
                    };
                    state.test_metrics_str = state.get_test_metrics_display();
                    if let Some(g) = guard {
                        cbx_core::monitor::record_task_end(g, true, &i18n::t("pipeline.direct_file"), &summary);
                    }
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    state.last_test_result = LastTestResult::Error {
                        fname: fname.to_string(),
                        err_msg: err_msg.clone(),
                    };
                    state.test_metrics_str = state.get_test_metrics_display();
                    state.test_thumb = None;
                    state.test_thumb_dims = (0, 0);
                    if let Some(g) = guard {
                        cbx_core::monitor::record_task_end(g, false, &i18n::t("pipeline.direct_file"), &err_msg);
                    }
                }
            }
        } else if let Some(g) = guard {
            cbx_core::monitor::record_task_end(g, false, &i18n::t("pipeline.direct_file"), &i18n::t("details.cannot_open_file"));
        }
    } else {
        if let Ok(mut f) = std::fs::File::open(path) {
            match cbx_core::generate_thumbnail_bytes_with_ext(&mut f, 256, &state.settings, Some(&fname)) {
                Ok((bgra, rep)) => {
                    let elapsed_ms = t_start.elapsed().as_millis() as u32;
                    let is_simple = cbx_core::media::smart_render::is_image_too_simple(&bgra, true);
                    state.last_test_result = LastTestResult::Archive {
                        fname: fname.to_string(),
                        elapsed_ms,
                        entry_name: rep.entry_name.clone(),
                        out_w: rep.decode.out_width,
                        out_h: rep.decode.out_height,
                        fmt: rep.decode.format_name.to_string(),
                        is_simple,
                    };
                    state.test_metrics_str = state.get_test_metrics_display();
                    state.test_thumb_dims = (rep.decode.out_width, rep.decode.out_height);
                    state.test_thumb = cbx_core::image_dec::create_hbitmap_from_bgra(rep.decode.out_width, rep.decode.out_height, &bgra).ok();
                    if let Some(g) = guard {
                        let msg = cbx_core::i18n::t_fmt(
                            "details.archive_page",
                            &[
                                ("w", &rep.decode.out_width.to_string()),
                                ("h", &rep.decode.out_height.to_string()),
                                ("entry", &rep.entry_name),
                            ],
                        );
                        cbx_core::monitor::record_task_end(g, true, &i18n::t("pipeline.direct_file"), &msg);
                    }
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    if cbx_core::other_doc::is_likely_plain_text_stream(&mut f) {
                        let fallback_ext = if ext.is_empty() { ".txt" } else { ext.as_str() };
                        if let Ok(rgba) = cbx_core::other_doc::create_other_thumbnail_rgba(&mut f, fallback_ext, 256) {
                            let elapsed_ms = t_start.elapsed().as_millis() as u32;
                            let out_w = rgba.width();
                            let out_h = rgba.height();
                            let bgra = cbx_core::rgba_to_bgra(&rgba);
                            let ext_label = fallback_ext.trim_start_matches('.').to_ascii_uppercase();
                            let summary = cbx_core::i18n::t_fmt(
                                "details.plain_text",
                                &[("w", &out_w.to_string()), ("h", &out_h.to_string()), ("ext", &ext_label)],
                            );
                            state.test_thumb_dims = (out_w, out_h);
                            state.test_thumb = cbx_core::image_dec::create_hbitmap_from_bgra(out_w, out_h, &bgra).ok();
                            state.last_test_result = LastTestResult::Other {
                                fname: fname.to_string(),
                                elapsed_ms,
                                entry_name: summary.clone(),
                                out_w,
                                out_h,
                                fmt: ext_label,
                            };
                            state.test_metrics_str = state.get_test_metrics_display();
                            if let Some(g) = guard {
                                cbx_core::monitor::record_task_end(g, true, &i18n::t("pipeline.direct_file"), &summary);
                            }
                            state.monitor_snapshot = cbx_core::monitor::fetch_snapshot();
                            return;
                        }
                    }
                    state.last_test_result = LastTestResult::Error {
                        fname: fname.to_string(),
                        err_msg: err_msg.clone(),
                    };
                    state.test_metrics_str = state.get_test_metrics_display();
                    state.test_thumb = None;
                    state.test_thumb_dims = (0, 0);
                    if let Some(g) = guard {
                        cbx_core::monitor::record_task_end(g, false, &i18n::t("pipeline.direct_file"), &err_msg);
                    }
                }
            }
        } else if let Some(g) = guard {
            cbx_core::monitor::record_task_end(g, false, &i18n::t("pipeline.direct_file"), &i18n::t("details.cannot_open_file"));
        }
    }

    state.monitor_snapshot = cbx_core::monitor::fetch_snapshot();
}

// -----------------------------------------------------------------------------
// Fluent UI Renderer
// -----------------------------------------------------------------------------

/// One horizontal or vertical running-sum box blur pass over 4-channel pixels (edges clamped).
fn box_blur_1d(src: &[[u32; 4]], dst: &mut [[u32; 4]], w: usize, h: usize, r: usize, horizontal: bool) {
    let (outer, inner) = if horizontal { (h, w) } else { (w, h) };
    let idx = |o: usize, i: usize| if horizontal { o * w + i } else { i * w + o };
    let win = (2 * r + 1) as u32;
    for o in 0..outer {
        let mut sum = [0u32; 4];
        for k in 0..=(2 * r) {
            let i = k.saturating_sub(r).min(inner - 1);
            let p = src[idx(o, i)];
            for c in 0..4 {
                sum[c] += p[c];
            }
        }
        for i in 0..inner {
            let mut out = [0u32; 4];
            for c in 0..4 {
                out[c] = sum[c] / win;
            }
            dst[idx(o, i)] = out;
            let add = src[idx(o, (i + r + 1).min(inner - 1))];
            let sub = src[idx(o, i.saturating_sub(r))];
            for c in 0..4 {
                sum[c] = sum[c] + add[c] - sub[c];
            }
        }
    }
}

/// Anti-aliased coverage (0..1) of a pixel centre inside a rounded rectangle.
fn round_rect_coverage(fx: f32, fy: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> f32 {
    let cx = fx.clamp(x0 + r, x1 - r);
    let cy = fy.clamp(y0 + r, y1 - r);
    let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() - r;
    let inside_box = fx >= x0 && fx <= x1 && fy >= y0 && fy <= y1;
    if !inside_box {
        return 0.0;
    }
    (0.5 - d).clamp(0.0, 1.0)
}

struct DibSurface {
    bits: *mut u32,
    w: i32,
    h: i32,
}

impl DibSurface {
    #[inline]
    unsafe fn blend_premul(&self, x: i32, y: i32, b: u8, g: u8, r: u8, a: u8) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h || a == 0 {
            return;
        }
        let idx = (y as usize) * (self.w as usize) + (x as usize);
        let dst = *self.bits.add(idx);
        let da = ((dst >> 24) & 0xFF) as u32;
        let dr = ((dst >> 16) & 0xFF) as u32;
        let dg = ((dst >> 8) & 0xFF) as u32;
        let db = (dst & 0xFF) as u32;

        let sa = a as u32;
        let sr = (r as u32 * sa + 127) / 255;
        let sg = (g as u32 * sa + 127) / 255;
        let sb = (b as u32 * sa + 127) / 255;
        let inv = 255 - sa;

        let oa = (sa + (da * inv + 127) / 255).min(255);
        let or = (sr + (dr * inv + 127) / 255).min(255);
        let og = (sg + (dg * inv + 127) / 255).min(255);
        let ob = (sb + (db * inv + 127) / 255).min(255);
        *self.bits.add(idx) = (oa << 24) | (or << 16) | (og << 8) | ob;
    }

    unsafe fn fill_rounded_translucent(
        &self,
        x0: i32,
        y0: i32,
        w: i32,
        h: i32,
        radius: i32,
        fill_bgr: u32,
        fill_a: u8,
        border_bgr: u32,
        border_a: u8,
    ) {
        if w <= 0 || h <= 0 {
            return;
        }
        let r = (radius as f32).min((w.min(h) as f32) * 0.5);
        let fb = (fill_bgr >> 16) as u8;
        let fg = ((fill_bgr >> 8) & 0xFF) as u8;
        let fr = (fill_bgr & 0xFF) as u8;
        let bb = (border_bgr >> 16) as u8;
        let bg = ((border_bgr >> 8) & 0xFF) as u8;
        let br = (border_bgr & 0xFF) as u8;

        let x1 = (x0 + w).min(self.w);
        let y1 = (y0 + h).min(self.h);
        let sx0 = x0.max(0);
        let sy0 = y0.max(0);
        let cx0 = x0 as f32 + r;
        let cx1 = (x0 + w) as f32 - r;
        let cy0 = y0 as f32 + r;
        let cy1 = (y0 + h) as f32 - r;

        for py in sy0..y1 {
            let fy = py as f32 + 0.5;
            let dy = if fy < cy0 {
                cy0 - fy
            } else if fy > cy1 {
                fy - cy1
            } else {
                0.0
            };
            for px in sx0..x1 {
                let fx = px as f32 + 0.5;
                let dx = if fx < cx0 {
                    cx0 - fx
                } else if fx > cx1 {
                    fx - cx1
                } else {
                    0.0
                };
                let dist = (dx * dx + dy * dy).sqrt() - r;
                if dist > 0.5 {
                    continue;
                }
                let cov = (0.5 - dist).clamp(0.0, 1.0);
                if dist > -1.0 && border_a > 0 {
                    let ba = ((border_a as f32) * cov).round() as u8;
                    self.blend_premul(px, py, bb, bg, br, ba);
                } else if fill_a > 0 {
                    let fa = ((fill_a as f32) * cov).round() as u8;
                    self.blend_premul(px, py, fb, fg, fr, fa);
                }
            }
        }
    }

    /// In-app acrylic: blurs the premultiplied pixels inside a rounded rectangle (three box-blur
    /// passes ≈ Gaussian), blending at the anti-aliased edge so corners stay round.
    unsafe fn blur_round_rect(&self, x0: i32, y0: i32, x1: i32, y1: i32, radius: i32, blur: i32) {
        let sx0 = x0.max(0);
        let sy0 = y0.max(0);
        let sx1 = x1.min(self.w);
        let sy1 = y1.min(self.h);
        let (w, h) = (sx1 - sx0, sy1 - sy0);
        if w <= 2 || h <= 2 || blur <= 0 {
            return;
        }
        let (wu, hu) = (w as usize, h as usize);
        let mut buf: Vec<[u32; 4]> = Vec::with_capacity(wu * hu);
        for py in sy0..sy1 {
            let row = (py as usize) * (self.w as usize);
            for px in sx0..sx1 {
                let v = *self.bits.add(row + px as usize);
                buf.push([v & 0xFF, (v >> 8) & 0xFF, (v >> 16) & 0xFF, v >> 24]);
            }
        }
        let r = (blur / 3).max(1) as usize;
        let mut tmp = buf.clone();
        for _ in 0..3 {
            box_blur_1d(&buf, &mut tmp, wu, hu, r, true);
            box_blur_1d(&tmp, &mut buf, wu, hu, r, false);
        }
        let rad = (radius as f32).min(w.min(h) as f32 * 0.5);
        for py in sy0..sy1 {
            let row = (py as usize) * (self.w as usize);
            for px in sx0..sx1 {
                let c = round_rect_coverage(px as f32 + 0.5, py as f32 + 0.5, x0 as f32, y0 as f32, x1 as f32, y1 as f32, rad);
                if c <= 0.0 {
                    continue;
                }
                let b = buf[((py - sy0) as usize) * wu + (px - sx0) as usize];
                let ptr = self.bits.add(row + px as usize);
                let o = *ptr;
                let mix = |ov: u32, nv: u32| -> u32 { ((ov as f32) * (1.0 - c) + (nv as f32) * c).round().clamp(0.0, 255.0) as u32 };
                *ptr = mix(o & 0xFF, b[0]) | (mix((o >> 8) & 0xFF, b[1]) << 8) | (mix((o >> 16) & 0xFF, b[2]) << 16) | (mix(o >> 24, b[3]) << 24);
            }
        }
    }

    unsafe fn stamp_opaque_round_rect(&self, x0: i32, y0: i32, x1: i32, y1: i32, radius: i32) {
        let sx0 = x0.max(0);
        let sy0 = y0.max(0);
        let sx1 = x1.min(self.w);
        let sy1 = y1.min(self.h);
        let r = (radius as f32).min(((x1 - x0).min(y1 - y0) as f32) * 0.5);
        let cx0 = x0 as f32 + r;
        let cx1 = x1 as f32 - r;
        let cy0 = y0 as f32 + r;
        let cy1 = y1 as f32 - r;

        for py in sy0..sy1 {
            let fy = py as f32 + 0.5;
            let dy = if fy < cy0 {
                cy0 - fy
            } else if fy > cy1 {
                fy - cy1
            } else {
                0.0
            };
            let row_off = (py as usize) * (self.w as usize);
            for px in sx0..sx1 {
                let fx = px as f32 + 0.5;
                let dx = if fx < cx0 {
                    cx0 - fx
                } else if fx > cx1 {
                    fx - cx1
                } else {
                    0.0
                };
                if dx * dx + dy * dy <= (r + 0.5) * (r + 0.5) {
                    let ptr = self.bits.add(row_off + (px as usize));
                    *ptr |= 0xFF00_0000;
                }
            }
        }
    }

    unsafe fn fill_content_panel_aa(&self, sidebar_w: i32, radius: i32, bg_bgr: u32, border_bgr: u32) {
        let sx0 = sidebar_w.max(0);
        let r_f = radius as f32;
        let bg_b = (bg_bgr >> 16) as u8;
        let bg_g = ((bg_bgr >> 8) & 0xFF) as u8;
        let bg_r = (bg_bgr & 0xFF) as u8;
        let bd_b = (border_bgr >> 16) as u8;
        let bd_g = ((border_bgr >> 8) & 0xFF) as u8;
        let bd_r = (border_bgr & 0xFF) as u8;
        let bg_pixel = 0xFF00_0000 | ((bg_r as u32) << 16) | ((bg_g as u32) << 8) | (bg_b as u32);
        let bd_pixel = 0xFF00_0000 | ((bd_r as u32) << 16) | ((bd_g as u32) << 8) | (bd_b as u32);

        for py in 0..self.h {
            let row_off = (py as usize) * (self.w as usize);
            let in_top_arc_y = py < radius;
            for px in sx0..self.w {
                let ptr = self.bits.add(row_off + (px as usize));
                if in_top_arc_y && px < sidebar_w + radius {
                    let dx = (sidebar_w + radius) as f32 - (px as f32 + 0.5);
                    let dy = radius as f32 - (py as f32 + 0.5);
                    let signed_dist = (dx * dx + dy * dy).sqrt() - r_f;
                    let cov = (0.5 - signed_dist).clamp(0.0, 1.0);
                    if cov <= 0.0 {
                        continue;
                    }
                    let (cr, cg, cb) = if signed_dist > -1.0 {
                        let t = (signed_dist + 1.0).clamp(0.0, 1.0);
                        (
                            (bg_r as f32 + (bd_r as f32 - bg_r as f32) * t).round() as u8,
                            (bg_g as f32 + (bd_g as f32 - bg_g as f32) * t).round() as u8,
                            (bg_b as f32 + (bd_b as f32 - bg_b as f32) * t).round() as u8,
                        )
                    } else {
                        (bg_r, bg_g, bg_b)
                    };
                    let a = (cov * 255.0).round() as u32;
                    let pr = ((cr as u32 * a + 127) / 255) & 0xFF;
                    let pg = ((cg as u32 * a + 127) / 255) & 0xFF;
                    let pb = ((cb as u32 * a + 127) / 255) & 0xFF;
                    *ptr = (a << 24) | (pr << 16) | (pg << 8) | pb;
                } else if (py == 0 && px >= sidebar_w + radius) || (px == sx0 && py >= radius) {
                    *ptr = bd_pixel;
                } else {
                    *ptr = bg_pixel;
                }
            }
        }
    }

    /// Composites every non-opaque (premultiplied) pixel over an opaque base colour (BGR).
    unsafe fn flatten_onto_opaque(&self, base_bgr: u32) {
        let base_r = base_bgr & 0xFF;
        let base_g = (base_bgr >> 8) & 0xFF;
        let base_b = (base_bgr >> 16) & 0xFF;
        let count = (self.w as usize) * (self.h as usize);
        for i in 0..count {
            let ptr = self.bits.add(i);
            let px = *ptr;
            let a = (px >> 24) & 0xFF;
            if a == 255 {
                continue;
            }
            let inv = 255 - a;
            let r = (((px >> 16) & 0xFF) + (base_r * inv + 127) / 255).min(255);
            let g = (((px >> 8) & 0xFF) + (base_g * inv + 127) / 255).min(255);
            let b = ((px & 0xFF) + (base_b * inv + 127) / 255).min(255);
            *ptr = 0xFF00_0000 | (r << 16) | (g << 8) | b;
        }
    }

    /// Makes the plain page background of the content panel (pixels exactly equal to the opaque
    /// `bg_bgr`) translucent, so the Mica backdrop glows through it more darkly than through the
    /// rail; cards, text and controls stay opaque. Call after `stamp_content_panel_alpha`.
    unsafe fn tint_content_background(&self, sidebar_w: i32, bg_bgr: u32, alpha: u32) {
        let (r, g, b) = (bg_bgr & 0xFF, (bg_bgr >> 8) & 0xFF, (bg_bgr >> 16) & 0xFF);
        let opaque = 0xFF00_0000 | (r << 16) | (g << 8) | b;
        let a = alpha.min(255);
        let pm = |c: u32| (c * a + 127) / 255;
        let translucent = (a << 24) | (pm(r) << 16) | (pm(g) << 8) | pm(b);
        for py in 0..self.h {
            let row_off = (py as usize) * (self.w as usize);
            for px in sidebar_w.max(0)..self.w {
                let ptr = self.bits.add(row_off + (px as usize));
                if *ptr == opaque {
                    *ptr = translucent;
                }
            }
        }
    }

    unsafe fn stamp_content_panel_alpha(&self, sidebar_w: i32, radius: i32) {
        let sx0 = sidebar_w.max(0);
        let r_inner = (radius as f32 - 1.5).max(0.0);
        let r_inner_sq = r_inner * r_inner;
        for py in 0..self.h {
            let row_off = (py as usize) * (self.w as usize);
            let in_top_arc_y = py < radius;
            for px in sx0..self.w {
                if in_top_arc_y && px < sidebar_w + radius {
                    let dx = (sidebar_w + radius) as f32 - (px as f32 + 0.5);
                    let dy_arc = radius as f32 - (py as f32 + 0.5);
                    if dx * dx + dy_arc * dy_arc > r_inner_sq {
                        continue;
                    }
                }
                let ptr = self.bits.add(row_off + (px as usize));
                *ptr |= 0xFF00_0000;
            }
        }
    }
}

unsafe fn draw_composited_text(
    hdc: HDC,
    htheme: HTHEME,
    text: &str,
    mut rc: RECT,
    bgr: u32,
    flags: windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT,
) {
    let mut wide = to_wide(text);
    if !htheme.is_invalid() {
        let opts = DTTOPTS {
            dwSize: std::mem::size_of::<DTTOPTS>() as u32,
            dwFlags: DTT_COMPOSITED | DTT_TEXTCOLOR,
            crText: windows::Win32::Foundation::COLORREF(bgr),
            ..Default::default()
        };
        let wide_no_nul = if wide.last() == Some(&0) {
            &wide[..wide.len() - 1]
        } else {
            &wide[..]
        };
        if DrawThemeTextEx(htheme, hdc, 0, 0, wide_no_nul, flags, &mut rc, Some(&opts)).is_ok() {
            return;
        }
    }
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(bgr));
    let _ = DrawTextW(hdc, &mut wide, &mut rc, flags);
}

/// Current width (physical px) of the navigation rail, following the collapse animation.
fn sidebar_width(state: &AppState) -> i32 {
    let full = scale_px(SIDEBAR_BASE_WIDTH, state.dpi) as f32;
    let narrow = scale_px(SIDEBAR_COLLAPSED_WIDTH, state.dpi) as f32;
    (full + (narrow - full) * state.sidebar_anim.clamp(0.0, 1.0)).round() as i32
}

/// Language button geometry in the rail: (x, width, flyout width). When the rail is collapsed the
/// button shrinks to an icon-sized square and its flyout keeps a readable width.
fn lang_button_geom(state: &AppState, sidebar_w: i32) -> (i32, i32, i32) {
    let x = scale_px(10, state.dpi);
    let w = (sidebar_w - x * 2).max(scale_px(RAIL_ICON_COL_W, state.dpi));
    (x, w, w.max(scale_px(160, state.dpi)))
}

/// Right edge of the area the language button and its flyout can cover (for partial repaints).
fn lang_invalidate_right(state: &AppState) -> i32 {
    let sidebar_w = sidebar_width(state);
    let (x, _, menu_w) = lang_button_geom(state, sidebar_w);
    sidebar_w.max(x + menu_w + 1)
}

/// Rail toggle button rectangle (UI coordinates).
fn hamburger_rect(state: &AppState) -> RECT {
    let x = scale_px(10, state.dpi);
    let y = scale_px(HAMBURGER_Y, state.dpi);
    RECT {
        left: x,
        top: y,
        right: x + scale_px(HAMBURGER_W, state.dpi),
        bottom: y + scale_px(HAMBURGER_H, state.dpi),
    }
}

/// Sidebar status text without any version prefix. Older locale files (e.g. a `locales` folder
/// left next to an installed exe) hard-coded "2.0.0 • …"; the version now comes from
/// `APP_VERSION`, so a leading "X.Y.Z • " is dropped instead of being shown twice.
fn status_label(key: &str) -> String {
    let text = i18n::t(key);
    let t = text.trim_start();
    if let Some((head, rest)) = t.split_once('•') {
        let head = head.trim();
        if !head.is_empty() && head.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return rest.trim_start().to_string();
        }
    }
    text.to_string()
}

/// Duration of the selected-indicator squash when a flyout item is pressed (seconds).
const FLYOUT_PRESS_SECS: f32 = 0.12;

/// Win11 ComboBox-style flyout geometry. The list keeps its full content; it opens so that the
/// selected row lies on top of the trigger button and grows up and down from there, and is only
/// shifted (never shortened) when the window is too small to centre it.
#[derive(Clone, Copy)]
struct FlyoutGeom {
    x: i32,
    w: i32,
    top: i32,
    full_h: i32,
    item_h: i32,
    padding: i32,
    count: usize,
    sel: usize,
}

impl FlyoutGeom {
    #[allow(clippy::too_many_arguments)]
    fn new(x: i32, w: i32, btn_y: i32, btn_h: i32, count: usize, sel: usize, item_h: i32, padding: i32, bound_top: i32, bound_bottom: i32) -> Self {
        let sel = sel.min(count.saturating_sub(1));
        let full_h = count as i32 * item_h + padding * 2;
        let mut top = btn_y + btn_h / 2 - padding - sel as i32 * item_h - item_h / 2;
        top = top.min(bound_bottom - full_h);
        top = top.max(bound_top);
        FlyoutGeom { x, w, top, full_h, item_h, padding, count, sel }
    }

    fn item_top(&self, i: usize) -> i32 {
        self.top + self.padding + i as i32 * self.item_h
    }

    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.top && y < self.top + self.full_h
    }

    fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        if !self.contains(x, y) {
            return None;
        }
        let rel = y - self.top - self.padding;
        if rel < 0 {
            return None;
        }
        let idx = (rel / self.item_h) as usize;
        (idx < self.count).then_some(idx)
    }

    /// Visible vertical span while opening: from the selected row out to the full list.
    fn visible(&self, anim: f32) -> (i32, i32) {
        let t = anim.clamp(0.0, 1.0);
        let s0 = self.item_top(self.sel) - self.padding;
        let s1 = self.item_top(self.sel) + self.item_h + self.padding;
        let lerp = |a: i32, b: i32| a + ((b - a) as f32 * t).round() as i32;
        (lerp(s0, self.top), lerp(s1, self.top + self.full_h))
    }
}

/// Flyout of the language button at the bottom of the rail.
fn lang_flyout_geom(state: &AppState, win_h: i32) -> FlyoutGeom {
    let sidebar_w = sidebar_width(state);
    let (lang_x, _, menu_w) = lang_button_geom(state, sidebar_w);
    let lang_y = win_h - scale_px(50, state.dpi);
    let sel = AVAILABLE_LOCALES.iter().position(|l| l.lang == current_lang()).unwrap_or(0);
    FlyoutGeom::new(
        lang_x,
        menu_w,
        lang_y,
        scale_px(LANG_BTN_H, state.dpi),
        AVAILABLE_LOCALES.len(),
        sel,
        scale_px(36, state.dpi),
        scale_px(4, state.dpi),
        scale_px(8, state.dpi),
        win_h - scale_px(8, state.dpi),
    )
}

fn monitor_pipeline_options() -> [(&'static str, MonitorPipelineFilter); 5] {
    [
        (tr("全部管道", "All Pipelines"), MonitorPipelineFilter::All),
        (tr("檔案直讀", "Direct File"), MonitorPipelineFilter::DirectFile),
        (tr("IPC 命中", "IPC Hit"), MonitorPipelineFilter::IpcHit),
        (tr("搜尋命中", "Search Hit"), MonitorPipelineFilter::SearchHit),
        (tr("串流 RPC", "Stream RPC"), MonitorPipelineFilter::StreamRpc),
    ]
}

fn monitor_status_options() -> [(&'static str, MonitorFilter); 4] {
    [
        (tr("全部狀態", "All Status"), MonitorFilter::All),
        (tr("正常", "OK"), MonitorFilter::Success),
        (tr("耗時", "Slow"), MonitorFilter::Slow),
        (tr("中斷/跳過", "Errors"), MonitorFilter::InterruptedOrSkipped),
    ]
}

/// Flyout of an open monitor filter button (0 = pipeline, 1 = status).
fn monitor_flyout_geom(state: &AppState, win_h: i32) -> Option<FlyoutGeom> {
    let kind = state.open_monitor_dropdown?;
    let (pipe_x, status_x, btn_y, btn_h) = state.monitor_dropdown_anchor.get();
    if btn_h <= 0 {
        return None;
    }
    let (x, w, count, sel) = if kind == 0 {
        let sel = monitor_pipeline_options().iter().position(|(_, v)| *v == state.monitor_pipeline_filter).unwrap_or(0);
        (pipe_x, scale_px(114, state.dpi), 5, sel)
    } else {
        let sel = monitor_status_options().iter().position(|(_, v)| *v == state.monitor_filter).unwrap_or(0);
        (status_x, scale_px(108, state.dpi), 4, sel)
    };
    Some(FlyoutGeom::new(
        x,
        w,
        btn_y,
        btn_h,
        count,
        sel,
        scale_px(28, state.dpi),
        scale_px(4, state.dpi),
        scale_px(8, state.dpi),
        win_h - scale_px(8, state.dpi),
    ))
}

/// Applies a language chosen from the flyout.
unsafe fn apply_language_choice(hwnd: HWND, state: &mut AppState, idx: usize) {
    if let Some(loc) = AVAILABLE_LOCALES.get(idx) {
        set_current_lang(loc.lang);
        loc.lang.save();
        let title_w = to_wide(window_title());
        let _ = SetWindowTextW(hwnd, PCWSTR(title_w.as_ptr()));
        state.cache_status_msg = state.get_cache_status_display();
        state.test_metrics_str = state.get_test_metrics_display();
    }
}

/// Applies a monitor filter chosen from the flyout.
fn apply_monitor_filter_choice(state: &mut AppState, kind: u8, idx: usize) {
    if kind == 0 {
        if let Some((_, v)) = monitor_pipeline_options().get(idx) {
            state.monitor_pipeline_filter = *v;
        }
    } else if let Some((_, v)) = monitor_status_options().get(idx) {
        state.monitor_filter = *v;
    }
    state.monitor_scroll = 0;
}

/// Draws an acrylic flyout list: the content beneath is blurred inside the rounded frame, tinted
/// with a translucent card colour (over the rail the window backdrop keeps showing through), and
/// rows show hover / pressed / selected states. Selection itself happens on mouse release.
#[allow(clippy::too_many_arguments)]
unsafe fn draw_acrylic_flyout(
    hdc: HDC,
    dib: &DibSurface,
    htheme: HTHEME,
    state: &AppState,
    g: &FlyoutGeom,
    labels: &[&str],
    hover: Option<usize>,
    pressed: Option<usize>,
    anim: f32,
    font: HFONT,
) {
    let th = theme();
    let (vt, vb) = g.visible(anim);
    if vb <= vt {
        return;
    }
    let radius = scale_px(8, state.dpi);
    dib.blur_round_rect(g.x, vt, g.x + g.w, vb, radius, scale_px(12, state.dpi));
    let (tint, tint_a) = if th.dark { (0x002C2C2C, 0xD8) } else { (0x00F9F9F9, 0xD8) };
    dib.fill_rounded_translucent(g.x, vt, g.w, vb - vt, radius, tint, tint_a, th.btn_border, 0xFF);

    let rgn = CreateRectRgn(g.x, vt, g.x + g.w, vb);
    let _ = SelectClipRgn(hdc, rgn);
    let old_f = SelectObject(hdc, font);
    let overlay = if th.dark { 0x00FFFFFF } else { 0x00000000 };
    let item_r = scale_px(4, state.dpi);
    for (i, label) in labels.iter().enumerate().take(g.count) {
        let ix = g.x + g.padding;
        let iw = g.w - g.padding * 2;
        let iy = g.item_top(i);
        let (ry0, ry1) = (iy.max(vt + 1), (iy + g.item_h).min(vb - 1));
        if ry1 <= ry0 {
            continue;
        }
        let is_sel = i == g.sel;
        let is_pressed = pressed == Some(i);
        let is_hover = hover == Some(i);
        let row_a: u8 = if is_pressed {
            if th.dark { 9 } else { 6 }
        } else if is_hover || is_sel {
            if th.dark { 16 } else { 11 }
        } else {
            0
        };
        if row_a > 0 {
            dib.fill_rounded_translucent(ix, ry0, iw, ry1 - ry0, item_r, overlay, row_a, overlay, 0);
        }
        if is_sel {
            // Accent indicator; squashes briefly while the selected row itself is pressed.
            let mut ind_h = scale_px(16, state.dpi) as f32;
            if is_pressed {
                if let Some(t0) = state.flyout_press_start {
                    let p = (t0.elapsed().as_secs_f32() / FLYOUT_PRESS_SECS).clamp(0.0, 1.0);
                    ind_h *= 1.0 - 0.4 * (1.0 - (1.0 - p).powi(3));
                }
            }
            let ind_h = ind_h.round() as i32;
            let ind_w = scale_px(3, state.dpi);
            let ind_y = (iy + (g.item_h - ind_h) / 2).max(ry0);
            let ind_b = (iy + (g.item_h + ind_h) / 2).min(ry1);
            if ind_b > ind_y {
                let acc = accent_base_bgr();
                dib.fill_rounded_translucent(ix + scale_px(2, state.dpi), ind_y, ind_w, ind_b - ind_y, ind_w, acc, 255, acc, 0);
            }
        }
        let text_col = if is_pressed { th.text_secondary } else if is_sel { th.text_bright } else { th.text_primary };
        draw_composited_text(
            hdc,
            htheme,
            label,
            RECT { left: ix + scale_px(14, state.dpi), top: iy, right: ix + iw - scale_px(6, state.dpi), bottom: iy + g.item_h },
            text_col,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
    }
    SelectObject(hdc, old_f);
    let _ = SelectClipRgn(hdc, None);
    let _ = DeleteObject(rgn);
}

/// Window title, also painted in the custom title bar.
fn window_title() -> &'static str {
    tr("CBX Shell 控制中心", "CBX Shell Control Center")
}

/// Height (physical px) of the top resize band of the custom title bar.
unsafe fn resize_frame_h(hwnd: HWND) -> i32 {
    let dpi = GetDpiForWindow(hwnd).max(96);
    GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
}

/// Height (physical px) of the custom title bar strip, matching the system caption so DWM's
/// caption buttons fit exactly. The UI below keeps its own coordinates: y = 0 is the first row
/// under this strip (see `logical_client_rect` and the mouse handlers in `wnd_proc`).
unsafe fn titlebar_h(hwnd: HWND) -> i32 {
    let dpi = GetDpiForWindow(hwnd).max(96);
    let system = GetSystemMetricsForDpi(SM_CYCAPTION, dpi) + resize_frame_h(hwnd);
    system.max(scale_px(TITLEBAR_BASE_H, dpi))
}

/// Client rectangle of the UI area below the custom title bar, in UI coordinates.
unsafe fn logical_client_rect(hwnd: HWND, rect: &mut RECT) -> windows::core::Result<()> {
    windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, rect)?;
    rect.bottom = (rect.bottom - titlebar_h(hwnd)).max(rect.top + 1);
    Ok(())
}

/// Draws the window title, vertically centred in the custom title bar strip (between the window
/// edge and the top of the content panel), independent of the rail's width.
unsafe fn draw_window_title(hdc: HDC, htheme: HTHEME, state: &AppState, cap: i32) {
    let old = SelectObject(hdc, state.font_title);
    let x = scale_px(20, state.dpi);
    draw_composited_text(
        hdc,
        htheme,
        window_title(),
        RECT {
            left: x,
            top: 0,
            right: x + scale_px(600, state.dpi),
            bottom: cap,
        },
        theme().text_primary,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );
    SelectObject(hdc, old);
}

unsafe fn render_fluent_ui(hwnd: HWND, hdc: HDC, paint_rect: &RECT) {
    let Some(mut state_guard) = state_lock() else { return };
    let state: &mut AppState = &mut state_guard;
    let mut rect = RECT::default();
    let _ = logical_client_rect(hwnd, &mut rect);
    let w = (rect.right - rect.left).max(1);
    let h = (rect.bottom - rect.top).max(1);

    let sidebar_w = sidebar_width(state);
    let sidebar_only = paint_rect.right > 0
        && paint_rect.right <= sidebar_w
        && state.expanded_monitor_item.is_none()
        && state.open_monitor_dropdown.is_none();

    let mut layout_content_h = None;
    if !sidebar_only {
        state.edit_clear_rect.set(None);
        state.edit_boxes.borrow_mut().clear();
        let total_content_h = calculate_content_height(state);
        layout_content_h = Some(total_content_h);
        state.max_scroll_y = (total_content_h - h).max(0);
        if state.scroll_y > state.max_scroll_y {
            state.scroll_y = state.max_scroll_y;
        }
    }

    let target_w = if sidebar_only { sidebar_w.min(w).max(1) } else { w };
    let mem_dc = CreateCompatibleDC(hdc);
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: target_w,
            biHeight: -h, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
    let mem_bmp = match CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0) {
        Ok(b) if !bits_ptr.is_null() => b,
        _ => {
            let _ = DeleteDC(mem_dc);
            return;
        }
    };
    let old_bmp = SelectObject(mem_dc, mem_bmp);
    SetBkMode(mem_dc, TRANSPARENT);
    std::ptr::write_bytes(bits_ptr as *mut u32, 0, (target_w as usize) * (h as usize));
    let dib = DibSurface {
        bits: bits_ptr as *mut u32,
        w: target_w,
        h,
    };

    let content_radius = scale_px(10, state.dpi);
    if !sidebar_only {
        // 1. Content panel with anti-aliased Win11 Task Manager style rounded top-left corner
        let th = theme();
        dib.fill_content_panel_aa(sidebar_w, content_radius, th.win_bg, th.card_border);

        let content_rgn = CreateRoundRectRgn(
            sidebar_w,
            0,
            w + content_radius + 2,
            h + content_radius + 2,
            content_radius * 2,
            content_radius * 2,
        );
        SelectClipRgn(mem_dc, content_rgn);

        render_content(mem_dc, state, sidebar_w, w, h);

        SelectClipRgn(mem_dc, None);
        let _ = DeleteObject(content_rgn);
    }

    // 2. Render the translucent navigation rail (frosted Acrylic backdrop shows through)
    let htheme = OpenThemeData(hwnd, w!("Window"));
    render_sidebar(mem_dc, &dib, htheme, state, sidebar_w, h);
    let cap = titlebar_h(hwnd);

    if state.is_lang_menu_open {
        let g = lang_flyout_geom(state, h);
        let labels: Vec<&str> = AVAILABLE_LOCALES.iter().map(|l| l.display_name).collect();
        let anim = if state.lang_menu_anim_start.is_some() { state.lang_menu_anim } else { 1.0 };
        draw_acrylic_flyout(mem_dc, &dib, htheme, state, &g, &labels, state.hover_lang_item, state.pressed_lang_item, anim, state.font_regular);
    }

    if !sidebar_only {
        if state.active_tab == ManagerTab::Advanced && state.open_monitor_dropdown.is_some() {
            if let Some(g) = monitor_flyout_geom(state, h) {
                let labels: Vec<&str> = if state.open_monitor_dropdown == Some(0) {
                    monitor_pipeline_options().iter().map(|(l, _)| *l).collect()
                } else {
                    monitor_status_options().iter().map(|(l, _)| *l).collect()
                };
                let anim = if state.monitor_dd_anim_start.is_some() { state.monitor_dd_anim } else { 1.0 };
                draw_acrylic_flyout(mem_dc, &dib, htheme, state, &g, &labels, state.hover_monitor_dropdown_item, state.pressed_monitor_dd_item, anim, state.font_small);
            }
        }

        if let Some(ref item) = state.expanded_monitor_item {
            draw_expanded_monitor_modal(mem_dc, state, item, w, h);
            dib.stamp_content_panel_alpha(sidebar_w, content_radius);
            let modal_w = scale_px(620, state.dpi).min(w - scale_px(32, state.dpi));
            let modal_h = scale_px(518, state.dpi).min(h - scale_px(32, state.dpi));
            let modal_x = (w - modal_w) / 2;
            let modal_y = (h - modal_h) / 2;
            dib.stamp_opaque_round_rect(modal_x, modal_y, modal_x + modal_w, modal_y + modal_h, 8);
        } else {
            dib.stamp_content_panel_alpha(sidebar_w, content_radius);
            if is_backdrop_active() {
                dib.tint_content_background(sidebar_w, theme().win_bg, CONTENT_MICA_TINT_ALPHA);
            }
        }
    }

    if !is_backdrop_active() {
        // No blur backdrop available: never let the rail become clear, unblurred glass.
        dib.flatten_onto_opaque(theme().sidebar_bg);
    }

    let _ = windows::Win32::Graphics::Gdi::BitBlt(hdc, 0, cap, target_w, h, mem_dc, 0, 0, windows::Win32::Graphics::Gdi::SRCCOPY);

    SelectObject(mem_dc, old_bmp);
    let _ = DeleteObject(mem_bmp);
    let _ = DeleteDC(mem_dc);

    // 3. Custom title bar strip: the same frosted backdrop shows through, DWM draws the caption
    //    buttons on top, and we draw the window title.
    render_title_bar(hdc, htheme, state, target_w, cap);

    // Cards that size themselves to their content (e.g. the drag & drop result card) are only
    // measured while painting; if that changed the page height, repaint once with the new
    // scroll range.
    if let Some(prev_h) = layout_content_h {
        if calculate_content_height(state) != prev_h {
            let _ = InvalidateRect(hwnd, None, false);
        }
    }
    if !htheme.is_invalid() {
        let _ = CloseThemeData(htheme);
    }
}

unsafe fn render_title_bar(hdc: HDC, htheme: HTHEME, state: &AppState, w: i32, cap: i32) {
    if cap <= 0 || w <= 0 {
        return;
    }
    let strip_dc = CreateCompatibleDC(hdc);
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -cap, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
    let strip_bmp = match CreateDIBSection(strip_dc, &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0) {
        Ok(b) if !bits_ptr.is_null() => b,
        _ => {
            let _ = DeleteDC(strip_dc);
            return;
        }
    };
    let old_bmp = SelectObject(strip_dc, strip_bmp);
    SetBkMode(strip_dc, TRANSPARENT);
    std::ptr::write_bytes(bits_ptr as *mut u32, 0, (w as usize) * (cap as usize));
    let strip = DibSurface {
        bits: bits_ptr as *mut u32,
        w,
        h: cap,
    };

    draw_window_title(strip_dc, htheme, state, cap);
    if state.expanded_monitor_item.is_some() {
        // Same scrim as the modal dialog, so the title bar dims with the rest of the window.
        let dark = theme().dark;
        render_aa::draw_semi_transparent_rect(strip_dc, 0, 0, w, cap, 0x00000000, if dark { 0.65 } else { 0.35 });
    }
    if !is_backdrop_active() {
        strip.flatten_onto_opaque(theme().sidebar_bg);
    }

    let _ = windows::Win32::Graphics::Gdi::BitBlt(hdc, 0, 0, w, cap, strip_dc, 0, 0, windows::Win32::Graphics::Gdi::SRCCOPY);
    SelectObject(strip_dc, old_bmp);
    let _ = DeleteObject(strip_bmp);
    let _ = DeleteDC(strip_dc);
}

fn normalize_pipeline_display(p: &str) -> String {
    match p {
        "檔案直讀" | "Direct File" => i18n::t("pipeline.direct_file"),
        "IPC 命中" | "IPC Hit" => i18n::t("pipeline.ipc_hit"),
        "搜尋命中" | "Search Hit" => i18n::t("pipeline.search_hit"),
        _ => i18n::t("pipeline.stream_rpc"),
    }
}

fn is_details_no_cover(d: &str) -> bool {
    d.contains("無封面") || d.eq_ignore_ascii_case("No cover image") || d.contains("No Cover")
}

fn is_details_timeout_or_aborted(d: &str) -> bool {
    d.contains("跳過")
        || d.contains("逾時")
        || d.contains("中斷")
        || d.contains("Timeout")
        || d.contains("timeout")
        || d.contains("interrupted")
}

fn normalize_details_display(raw: &str) -> String {
    let d = raw.trim();
    if d.is_empty() {
        return String::new();
    }
    if is_details_no_cover(d) {
        return i18n::t("details.no_cover_image");
    }
    if d == "處理中斷或回退預設" || d == "Processing interrupted or fell back to default" {
        return i18n::t("details.worker_aborted");
    }
    if d == "影片解碼失敗" || d == "Video decode failed" {
        return i18n::t("details.video_decode_failed");
    }
    if let Some(err) = d
        .strip_prefix("影片解碼失敗: ")
        .or_else(|| d.strip_prefix("Video decode failed: "))
    {
        return cbx_core::i18n::t_fmt("details.video_decode_failed_err", &[("err", err)]);
    }

    match current_lang() {
        UiLang::En => d
            .replace("逾時跳過", "Timeout skipped")
            .replace("，階段:", ", stage:")
            .replace("階段:", "stage:")
            .replace("串流或解碼逾時", "Stream or decode timeout")
            .replace("準備擷取縮圖...", "Preparing thumbnail...")
            .replace("正在定位畫面 (", "Seeking frame (")
            .replace(" 基準點)", " baseline)")
            .replace("解碼畫面...", "Decoding frame...")
            .replace("電子書(", "E-Book (")
            .replace("音訊預設卡片", "Audio default card")
            .replace("音訊封面", "Audio cover")
            .replace("內嵌封面: ", "Embedded cover: ")
            .replace("無內嵌封面", "No embedded cover")
            .replace("無法開啟檔案", "Unable to open file")
            .replace("文件(", "Document (")
            .replace("純文字解析(", "Plain-text (")
            .replace("通用串流解析", "Generic stream parser")
            .replace("影片串流解析", "Video stream parser")
            .replace("耗時: ", "Latency: ")
            .replace("頁:", "Page: ")
            .replace("亮度", "Luma ")
            .replace("圖像熵", "Entropy ")
            .replace("熵", "Entropy ")
            .replace("重試", "Retries "),
        UiLang::ZhTw => d
            .replace("Timeout skipped", "逾時跳過")
            .replace(", stage:", "，階段:")
            .replace("stage:", "階段:")
            .replace("Stream or decode timeout", "串流或解碼逾時")
            .replace("Preparing thumbnail...", "準備擷取縮圖...")
            .replace("Seeking frame (", "正在定位畫面 (")
            .replace(" baseline)", " 基準點)")
            .replace("Decoding frame...", "解碼畫面...")
            .replace("E-Book (", "電子書(")
            .replace("Audio default card", "音訊預設卡片")
            .replace("Audio cover", "音訊封面")
            .replace("Embedded cover: ", "內嵌封面: ")
            .replace("No embedded cover", "無內嵌封面")
            .replace("Unable to open file", "無法開啟檔案")
            .replace("Document (", "文件(")
            .replace("Plain-text (", "純文字解析(")
            .replace("Generic stream parser", "通用串流解析")
            .replace("Video stream parser", "影片串流解析")
            .replace("Latency: ", "耗時: ")
            .replace("Page: ", "頁:")
            .replace("Luma ", "亮度")
            .replace("Entropy ", "熵")
            .replace("Retries ", "重試"),
    }
}

fn monitor_filter_matches(
    item: &cbx_core::monitor::SnapshotHistoryItem,
    status_filter: MonitorFilter,
    pipeline_filter: MonitorPipelineFilter,
) -> bool {
    let status_ok = match status_filter {
        MonitorFilter::All => true,
        MonitorFilter::Success => item.success && !is_details_timeout_or_aborted(&item.details),
        MonitorFilter::Slow => item.duration_ms >= 2000,
        MonitorFilter::InterruptedOrSkipped => !item.success || is_details_timeout_or_aborted(&item.details),
    };
    if !status_ok {
        return false;
    }
    match pipeline_filter {
        MonitorPipelineFilter::All => true,
        MonitorPipelineFilter::DirectFile => matches!(item.pipeline.as_str(), "檔案直讀" | "Direct File"),
        MonitorPipelineFilter::IpcHit => matches!(item.pipeline.as_str(), "IPC 命中" | "IPC Hit"),
        MonitorPipelineFilter::SearchHit => matches!(item.pipeline.as_str(), "搜尋命中" | "Search Hit"),
        MonitorPipelineFilter::StreamRpc => {
            item.pipeline.is_empty() || matches!(item.pipeline.as_str(), "串流 RPC" | "Stream RPC")
        }
    }
}

fn resolve_trace_display(item: &cbx_core::monitor::SnapshotHistoryItem) -> String {
    let raw = item.search_trace.trim();
    if !raw.is_empty() {
        if raw.contains("直接傳遞實體路徑") {
            return tr(
                "檔案總管直接傳遞實體路徑",
                "Explorer provided physical file path directly",
            )
            .to_string();
        }
        if raw.contains("Everything IPC") {
            return tr(
                "Everything IPC 服務查詢命中",
                "Resolved via Everything IPC service",
            )
            .to_string();
        }
        if raw.contains("LAST_FOUND_DIR") {
            return tr(
                "搜尋命中 — 記憶體目錄快取 (LAST_FOUND_DIR)",
                "Search Hit — Memory Directory Cache (LAST_FOUND_DIR)",
            )
            .to_string();
        }
        if raw.contains("(CWD)") {
            return tr(
                "搜尋命中 — 目前處理程序工作目錄 (CWD)",
                "Search Hit — Current Working Directory (CWD)",
            )
            .to_string();
        }
        if raw.contains("TypedPaths") {
            return tr(
                "搜尋命中 — 檔案總管網址列歷史 (TypedPaths)",
                "Search Hit — Explorer Address Bar History (TypedPaths)",
            )
            .to_string();
        }
        if raw.contains("Recent") {
            return tr(
                "搜尋命中 — Windows 最近使用檔案紀錄 (Recent .lnk)",
                "Search Hit — Windows Recent Items History (Recent .lnk)",
            )
            .to_string();
        }
        if raw.contains("使用者標準資料夾") {
            return tr(
                "搜尋命中 — 使用者標準資料夾",
                "Search Hit — User Standard Folders",
            )
            .to_string();
        }
        if raw.contains("根目錄") {
            return tr(
                "搜尋命中 — 本機磁碟機根目錄掃描",
                "Search Hit — Local Drive Root Scan",
            )
            .to_string();
        }
        if raw.contains("本機目錄或歷史紀錄") {
            return tr(
                "搜尋命中 — 本機目錄或歷史紀錄定位",
                "Search Hit — Local Directory or History Lookup",
            )
            .to_string();
        }
        if raw.contains("IInitializeWithStream") {
            return tr(
                "檔案總管透過 COM IInitializeWithStream 串流讀取",
                "Read via COM IInitializeWithStream",
            )
            .to_string();
        }
        if raw.contains("異常中止") {
            return tr(
                "工作執行緒異常中止或回退預設",
                "Worker thread aborted or fell back to default",
            )
            .to_string();
        }
        return raw.to_string();
    }
    match normalize_pipeline_display(&item.pipeline).as_str() {
        "檔案直讀" | "Direct File" => tr(
            "檔案總管直接傳遞實體路徑",
            "Explorer provided physical file path directly",
        )
        .to_string(),
        "IPC 命中" | "IPC Hit" => tr(
            "Everything IPC 服務查詢命中",
            "Resolved via Everything IPC service",
        )
        .to_string(),
        "搜尋命中" | "Search Hit" => tr(
            "搜尋命中 — 本機目錄或歷史紀錄定位",
            "Search Hit — Local Directory or History Lookup",
        )
        .to_string(),
        _ => tr(
            "檔案總管透過 COM IInitializeWithStream 串流讀取",
            "Read via COM IInitializeWithStream",
        )
        .to_string(),
    }
}

fn format_debug_report(item: &cbx_core::monitor::SnapshotHistoryItem) -> String {
    let status_str = if is_details_no_cover(&item.details) {
        tr("無封面圖檔", "No Cover Image")
    } else if item.success {
        tr("正常產生", "Normal Generation")
    } else if is_details_timeout_or_aborted(&item.details) {
        tr("逾時跳過", "Timeout Skipped")
    } else {
        tr("處理中斷或失敗", "Interrupted or Failed")
    };
    let pipeline_source = normalize_pipeline_display(&item.pipeline);
    let trace_str = resolve_trace_display(item);
    let details_str = normalize_details_display(&item.details);
    format!(
        "{}\r\n{}: {}\r\n{}: {}\r\n{}: {} ms ({:.2} s)\r\n{}: {}\r\n{}: {}\r\n{}: {}\r\n{}: {}\r\n",
        tr("[CBX Shell 處理報告]", "[CBX Shell Processing Report]"),
        tr("處理目標", "Target"),
        item.file_name,
        tr("檔案格式", "File Format"),
        item.ext,
        tr("處理耗時", "Latency"),
        item.duration_ms,
        item.duration_ms as f32 / 1000.0,
        tr("執行狀態", "Result Status"),
        status_str,
        tr("管線來源", "Pipeline Source"),
        pipeline_source,
        tr("搜尋紀錄", "Search Trace"),
        trace_str,
        tr("詳細指標與除錯資訊", "Detailed Metrics & Debug Info"),
        details_str
    )
}

unsafe fn draw_expanded_monitor_modal(
    hdc: HDC,
    state: &AppState,
    item: &cbx_core::monitor::SnapshotHistoryItem,
    win_w: i32,
    win_h: i32,
) {
    // 1. Semi-transparent overlay curtain (dimming the background)
    let th = theme();
    render_aa::draw_semi_transparent_rect(hdc, 0, 0, win_w, win_h, 0x00000000, if th.dark { 0.65 } else { 0.35 });

    // 2. Centered Win11 Fluent Card
    let modal_w = scale_px(620, state.dpi).min(win_w - scale_px(32, state.dpi));
    let modal_h = scale_px(518, state.dpi).min(win_h - scale_px(32, state.dpi));
    let modal_x = (win_w - modal_w) / 2;
    let modal_y = (win_h - modal_h) / 2;

    let th = theme();
    // Card background & border
    let bg_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(th.card_bg));
    let pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(th.card_border));
    let old_b = SelectObject(hdc, bg_brush);
    let old_p = SelectObject(hdc, pen);
    let _ = RoundRect(hdc, modal_x, modal_y, modal_x + modal_w, modal_y + modal_h, 8, 8);
    SelectObject(hdc, old_b);
    SelectObject(hdc, old_p);
    let _ = DeleteObject(bg_brush);
    let _ = DeleteObject(pen);

    // Title & Close Button
    let pad_x = scale_px(22, state.dpi);
    let mut cy = modal_y + scale_px(18, state.dpi);

    SelectObject(hdc, state.font_title);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_bright));
    let title_str = to_wide(tr("📊 處理報告", "📊 Processing Report"));
    let _ = TextOutW(hdc, modal_x + pad_x, cy, &title_str);

    // Close button (✕)
    let close_sz = scale_px(26, state.dpi);
    let close_x = modal_x + modal_w - pad_x - close_sz;
    let close_y = cy;
    draw_rounded_button(hdc, state, 399, close_x, close_y, close_sz, close_sz, "✕", false, false);

    cy += scale_px(34, state.dpi);

    // Separator line
    let sep_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(th.divider));
    let old_sp = SelectObject(hdc, sep_pen);
    let _ = windows::Win32::Graphics::Gdi::MoveToEx(hdc, modal_x + pad_x, cy, None);
    let _ = windows::Win32::Graphics::Gdi::LineTo(hdc, modal_x + modal_w - pad_x, cy);
    SelectObject(hdc, old_sp);
    let _ = DeleteObject(sep_pen);

    cy += scale_px(12, state.dpi);

    // 1. Target Label
    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_muted));
    let path_lbl = to_wide(tr("處理目標", "Target"));
    let _ = TextOutW(hdc, modal_x + pad_x, cy, &path_lbl);

    let is_recently_copied = state.copied_path_timer.map_or(false, |t| t.elapsed().as_secs() < 2);
    if is_recently_copied {
        SelectObject(hdc, state.font_small);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.status_ok));
        let c_hint = to_wide(tr("✓ 已複製處理目標", "✓ Copied target"));
        let _ = TextOutW(hdc, modal_x + modal_w - pad_x - scale_px(95, state.dpi), cy, &c_hint);
    }

    cy += scale_px(20, state.dpi);

    // Target Word-wrapped Display Box
    let box_x = modal_x + pad_x;
    let box_y = cy;
    let box_w = modal_w - pad_x * 2;
    let box_h = scale_px(62, state.dpi);

    let border_color = if is_recently_copied { accent_base_bgr() } else { th.sub_card_border };
    let box_bg = CreateSolidBrush(windows::Win32::Foundation::COLORREF(th.sub_card_bg));
    let box_pen = CreatePen(PS_SOLID, if is_recently_copied { 2 } else { 1 }, windows::Win32::Foundation::COLORREF(border_color));
    let old_bb = SelectObject(hdc, box_bg);
    let old_bp = SelectObject(hdc, box_pen);
    let _ = RoundRect(hdc, box_x, box_y, box_x + box_w, box_y + box_h, 6, 6);
    SelectObject(hdc, old_bb);
    SelectObject(hdc, old_bp);
    let _ = DeleteObject(box_bg);
    let _ = DeleteObject(box_pen);

    SelectObject(hdc, state.font_regular);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_primary));
    let mut path_rc = RECT {
        left: box_x + scale_px(10, state.dpi),
        top: box_y + scale_px(8, state.dpi),
        right: box_x + box_w - scale_px(10, state.dpi),
        bottom: box_y + box_h - scale_px(8, state.dpi),
    };
    let mut path_w = to_wide(&item.file_name);
    let _ = DrawTextW(hdc, &mut path_w, &mut path_rc, DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);

    cy = box_y + box_h + scale_px(10, state.dpi);

    // Field grid
    let row_h = scale_px(22, state.dpi);
    let lbl_w = scale_px(110, state.dpi);
    let val_x = modal_x + pad_x + lbl_w;

    let (status_text, status_color) = if is_details_no_cover(&item.details) {
        (tr("✓ 無封面圖檔", "✓ No Cover Image"), th.text_muted)
    } else if item.success {
        (tr("✓ 正常產生", "✓ Normal"), th.status_ok)
    } else if is_details_timeout_or_aborted(&item.details) {
        (tr("⚠️ 逾時跳過", "⚠️ Timeout Skipped"), th.status_warn)
    } else {
        (tr("✗ 處理中斷或失敗", "✗ Interrupted or Failed"), th.status_err)
    };

    let dur_str = format!("{} ms ({:.2} s)", item.duration_ms, item.duration_ms as f32 / 1000.0);
    let dur_col = if item.duration_ms >= 2000 { th.status_warn } else { th.status_ok };

    let pipeline_source = normalize_pipeline_display(&item.pipeline);
    let pipeline_color = th.text_primary;
    let trace_str = resolve_trace_display(item);

    let accent_txt = accent_light_bgr();
    let fields = [
        (tr("檔案類型", "Format / Ext"), item.ext.as_str(), accent_txt),
        (tr("處理耗時", "Latency"), dur_str.as_str(), dur_col),
        (tr("執行狀態", "Result Status"), status_text, status_color),
        (tr("管線來源", "Pipeline Source"), pipeline_source.as_str(), pipeline_color),
        (tr("搜尋紀錄", "Search Trace"), trace_str.as_str(), th.text_secondary),
    ];

    for (lbl, val, col) in fields {
        SelectObject(hdc, state.font_bold);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_muted));
        let lw = to_wide(lbl);
        let _ = TextOutW(hdc, modal_x + pad_x, cy, &lw);

        SelectObject(hdc, state.font_regular);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(col));
        let mut vw = to_wide(val);
        let mut rc = RECT {
            left: val_x,
            top: cy,
            right: modal_x + modal_w - pad_x,
            bottom: cy + row_h,
        };
        let _ = DrawTextW(hdc, &mut vw, &mut rc, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);

        cy += row_h;
    }

    cy += scale_px(10, state.dpi);

    // Detailed metrics & stage header
    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_muted));
    let det_lbl = to_wide(tr("詳細指標與除錯資訊", "Detailed Metrics & Debug Info"));
    let _ = TextOutW(hdc, modal_x + pad_x, cy, &det_lbl);

    let is_copied_det = state.copied_details_timer.map_or(false, |t| t.elapsed().as_secs() < 2);
    if is_copied_det {
        SelectObject(hdc, state.font_small);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.status_ok));
        let c_hint = to_wide(tr("✓ 已複製除錯資訊", "✓ Copied debug info"));
        let _ = TextOutW(hdc, modal_x + modal_w - pad_x - scale_px(110, state.dpi), cy, &c_hint);
    }

    cy += scale_px(22, state.dpi);

    // Details word-wrapped display box
    let dbox_x = modal_x + pad_x;
    let dbox_y = cy;
    let dbox_w = modal_w - pad_x * 2;
    let dbox_h = scale_px(64, state.dpi);

    let dbox_border_color = if is_copied_det { th.status_ok } else { th.sub_card_border };
    let dbox_bg = CreateSolidBrush(windows::Win32::Foundation::COLORREF(th.sub_card_bg));
    let dbox_pen = CreatePen(PS_SOLID, if is_copied_det { 2 } else { 1 }, windows::Win32::Foundation::COLORREF(dbox_border_color));
    let old_db = SelectObject(hdc, dbox_bg);
    let old_dp = SelectObject(hdc, dbox_pen);
    let _ = RoundRect(hdc, dbox_x, dbox_y, dbox_x + dbox_w, dbox_y + dbox_h, 6, 6);
    SelectObject(hdc, old_db);
    SelectObject(hdc, old_dp);
    let _ = DeleteObject(dbox_bg);
    let _ = DeleteObject(dbox_pen);

    SelectObject(hdc, state.font_regular);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_secondary));
    let mut det_rc = RECT {
        left: dbox_x + scale_px(10, state.dpi),
        top: dbox_y + scale_px(8, state.dpi),
        right: dbox_x + dbox_w - scale_px(10, state.dpi),
        bottom: dbox_y + dbox_h - scale_px(8, state.dpi),
    };
    let details_display = normalize_details_display(&item.details);
    let mut det_w = to_wide(&details_display);
    let _ = DrawTextW(hdc, &mut det_w, &mut det_rc, DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);

    // Bottom Close button
    let ok_w = scale_px(110, state.dpi);
    let ok_h = scale_px(30, state.dpi);
    let ok_x = modal_x + (modal_w - ok_w) / 2;
    let ok_y = modal_y + modal_h - ok_h - scale_px(14, state.dpi);
    draw_rounded_button(hdc, state, 398, ok_x, ok_y, ok_w, ok_h, tr("關閉", "Close"), true, false);
}

unsafe fn render_sidebar(
    hdc: HDC,
    dib: &DibSurface,
    htheme: HTHEME,
    state: &AppState,
    sidebar_w: i32,
    win_h: i32,
) {
    SetBkMode(hdc, TRANSPARENT);

    let th = theme();

    // Rail toggle ("hamburger") button, Task Manager style: same translucent pill as nav items.
    let hb = hamburger_rect(state);
    let (mx, my) = state.mouse_pos;
    let hb_hover = mx >= hb.left && mx < hb.right && my >= hb.top && my < hb.bottom;
    let hb_a: u8 = if state.hamburger_pressed {
        if th.dark { 10 } else { 7 }
    } else if hb_hover {
        if th.dark { 16 } else { 11 }
    } else {
        0
    };
    if hb_a > 0 {
        let pill_bgr = if th.dark { 0x00FFFFFF } else { 0x00000000 };
        dib.fill_rounded_translucent(hb.left, hb.top, hb.right - hb.left, hb.bottom - hb.top, scale_px(8, state.dpi), pill_bgr, hb_a, pill_bgr, 0);
    }
    SelectObject(hdc, state.font_chevron);
    let hb_text = if state.hamburger_pressed {
        if th.dark { 0x00A6A6A6 } else { 0x00666666 }
    } else if th.dark {
        0x00FFFFFF
    } else {
        0x001A1A1A
    };
    draw_composited_text(hdc, htheme, "\u{E700}", hb, hb_text, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);

    // Version / engine status, to the right of the toggle; fades out as the rail collapses.
    SelectObject(hdc, state.font_small);
    let (status_text, status_color) = if let Some(ref snap) = state.monitor_snapshot {
        if snap.active_tasks > 0 {
            let sec = snap.current_elapsed_ms / 1000;
            if sec >= 5 {
                (format!("{} • {} ({}s)", APP_VERSION, status_label("sidebar.status_processing_warn"), sec), th.status_warn)
            } else {
                (format!("{} • {} ({})", APP_VERSION, status_label("sidebar.status_processing"), snap.active_tasks), accent_light_bgr())
            }
        } else {
            (format!("{} • {}", APP_VERSION, status_label("sidebar.status_ready")), th.status_ok)
        }
    } else {
        (format!("{} • {}", APP_VERSION, status_label("sidebar.status_ready")), th.status_ok)
    };
    if state.sidebar_anim < 0.35 {
        draw_composited_text(
            hdc,
            htheme,
            &status_text,
            RECT {
                left: hb.right + scale_px(8, state.dpi),
                top: hb.top,
                right: sidebar_w - scale_px(10, state.dpi),
                bottom: hb.bottom,
            },
            status_color,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
    }

    // Draw 7 navigation items
    let item_h = scale_px(44, state.dpi);
    let start_y = scale_px(NAV_START_Y, state.dpi);
    let margin_x = scale_px(10, state.dpi);
    let item_w = sidebar_w - margin_x * 2;
    let pill_r = scale_px(8, state.dpi);

    for i in 0..7 {
        let tab = ManagerTab::from_index(i);
        let iy = start_y + (i as i32 * item_h);
        let is_active = state.active_tab == tab;
        let is_hover = state.hover_tab == Some(i);
        let is_pressed = state.pressed_tab == Some(i);

        let pill_bgr = if th.dark { 0x00FFFFFF } else { 0x00000000 };
        let base_a: f32 = if is_active && is_hover {
            if th.dark { 10.0 } else { 7.0 }
        } else if is_active || is_hover {
            if th.dark { 16.0 } else { 11.0 }
        } else {
            0.0
        };
        let target_press_a: f32 = if th.dark { 10.0 } else { 7.0 };
        let pill_a = if is_pressed {
            let p = state.pressed_tab_anim.clamp(0.0, 1.0);
            (base_a + (target_press_a - base_a) * p).round() as u8
        } else {
            base_a.round() as u8
        };
        if pill_a > 0 {
            dib.fill_rounded_translucent(
                margin_x,
                iy,
                item_w,
                item_h - scale_px(4, state.dpi),
                pill_r,
                pill_bgr,
                pill_a,
                pill_bgr,
                0,
            );
        }

        SelectObject(hdc, state.font_regular);
        let rest_text = if th.dark { 0x00FFFFFF } else { 0x001A1A1A };
        let pressed_text = if th.dark { 0x00A6A6A6 } else { 0x00666666 };
        let text_color = if is_pressed {
            lerp_color(rest_text, pressed_text, state.pressed_tab_anim.clamp(0.0, 1.0))
        } else {
            rest_text
        };

        // Icon centred in the shared icon column (aligned with the rail toggle), label after it;
        // the label is clipped away as the rail collapses.
        let (t_zh, t_en) = tab.title();
        let pill_bottom = iy + item_h - scale_px(4, state.dpi);
        let icon_col = scale_px(RAIL_ICON_COL_W, state.dpi);
        draw_composited_text(
            hdc,
            htheme,
            tab.icon(),
            RECT { left: margin_x, top: iy, right: margin_x + icon_col, bottom: pill_bottom },
            text_color,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
        );
        let label_left = margin_x + icon_col + scale_px(4, state.dpi);
        let label_right = margin_x + item_w - scale_px(8, state.dpi);
        if label_right - label_left > scale_px(12, state.dpi) {
            draw_composited_text(
                hdc,
                htheme,
                tr(t_zh, t_en),
                RECT { left: label_left, top: iy, right: label_right, bottom: pill_bottom },
                text_color,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
            );
        }
    }

    // Animated Win11 Left vertical accent pill indicator
    // Drawn straight into the premultiplied surface (anti-aliased, fully opaque). Painting it with
    // GDI and then stamping its bounding box opaque turned the translucent hover pill pixels
    // around it into a dark outline.
    let accent_bgr = accent_base_bgr();
    let half_h = state.indicator_h / 2.0;
    let top_y = (state.indicator_y - half_h).round() as i32;
    let bot_y = (state.indicator_y + half_h).round() as i32;
    let pill_w = scale_px(3, state.dpi);
    let pill_x = margin_x + scale_px(3, state.dpi);
    dib.fill_rounded_translucent(pill_x, top_y, pill_w, bot_y - top_y, pill_w, accent_bgr, 255, accent_bgr, 0);

    // Language switcher at bottom of left sidebar
    let lang_y = win_h - scale_px(50, state.dpi);
    let (lang_x, lang_w, _) = lang_button_geom(state, sidebar_w);
    let lang_h = scale_px(LANG_BTN_H, state.dpi);

    draw_language_button(hdc, dib, htheme, state, 105, lang_x, lang_y, lang_w, lang_h);
}

unsafe fn render_content(hdc: HDC, state: &AppState, sidebar_w: i32, win_w: i32, win_h: i32) {
    let content_x = sidebar_w + scale_px(25, state.dpi);
    let scrollbar_w = scale_px(10, state.dpi);
    let content_w = (win_w - content_x - scrollbar_w - scale_px(16, state.dpi)).max(400);

    let th = theme();
    // Page Title
    let (t_zh, t_en) = state.active_tab.title();
    SelectObject(hdc, state.font_hero);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_bright));
    let title = format!("{} {}", state.active_tab.icon(), tr(t_zh, t_en));
    let title_w = to_wide(&title);
    let _ = TextOutW(hdc, content_x, scale_px(25, state.dpi), &title_w);

    // Media Preview detection warning notice banner
    if is_media_preview_installed() {
        let banner_y = scale_px(65, state.dpi) - state.scroll_y;
        let banner_h = scale_px(34, state.dpi);
        let bg = CreateSolidBrush(windows::Win32::Foundation::COLORREF(th.caution_bg));
        let pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(th.caution_border));
        let old_b = SelectObject(hdc, bg);
        let old_p = SelectObject(hdc, pen);
        let _ = windows::Win32::Graphics::Gdi::RoundRect(hdc, content_x, banner_y, content_x + content_w, banner_y + banner_h, 8, 8);
        SelectObject(hdc, old_b);
        SelectObject(hdc, old_p);
        let _ = DeleteObject(bg);
        let _ = DeleteObject(pen);

        SelectObject(hdc, state.font_small);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.caution_text));
        let notice = tr(
            "⚠️ 檢測到系統已安裝 Media Preview。部分格式縮圖功能重疊，您可在此自由勾選由 CBX Shell 接管處理之格式。",
            "⚠️ Media Preview detected on system. Since capabilities overlap, you may choose which formats CBX Shell handles."
        );
        let nw = to_wide(notice);
        let _ = TextOutW(hdc, content_x + scale_px(12, state.dpi), banner_y + scale_px(8, state.dpi), &nw);
    }

    // Clip content strictly below header
    let clip_top = scale_px(65, state.dpi);
    let old_saved = windows::Win32::Graphics::Gdi::SaveDC(hdc);
    let _ = windows::Win32::Graphics::Gdi::IntersectClipRect(
        hdc,
        content_x - scale_px(5, state.dpi),
        clip_top,
        win_w - scrollbar_w - scale_px(8, state.dpi),
        win_h,
    );

    // Win11 Page Entrance Transition (Drill-in Slide Up + Alpha Fade In)
    let (trans_y, alpha) = if let Some(start_t) = state.page_trans_start {
        let elapsed = start_t.elapsed().as_secs_f32();
        let duration = 0.20;
        if elapsed < duration {
            let p = (elapsed / duration).clamp(0.0, 1.0);
            let t = 1.0 - (1.0 - p).powi(3);
            let ty = ((1.0 - t) * (scale_px(14, state.dpi) as f32)).round() as i32;
            let a = (p * 255.0).round() as u8;
            (ty, a)
        } else {
            (0, 255)
        }
    } else {
        (0, 255)
    };

    if alpha < 255 {
        let clip_w = win_w - content_x;
        let clip_h = win_h - clip_top;
        if clip_w > 0 && clip_h > 0 {
            let fade_dc = CreateCompatibleDC(hdc);
            let fade_bmp = CreateCompatibleBitmap(hdc, clip_w, clip_h);
            let old_fbmp = SelectObject(fade_dc, fade_bmp);

            // Window surface background
            let bg_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(th.win_bg));
            let r_bg = RECT { left: 0, top: 0, right: clip_w, bottom: clip_h };
            FillRect(fade_dc, &r_bg, bg_brush);
            let _ = DeleteObject(bg_brush);
            SetBkMode(fade_dc, TRANSPARENT);

            let mut old_fpt = windows::Win32::Foundation::POINT::default();
            let _ = windows::Win32::Graphics::Gdi::SetViewportOrgEx(fade_dc, -content_x, -clip_top + trans_y, Some(&mut old_fpt));

            match state.active_tab {
                ManagerTab::Archives => render_archives_page(fade_dc, state, content_x, content_w),
                ManagerTab::Ebook => render_ebook_page(fade_dc, state, content_x, content_w),
                ManagerTab::Video => render_video_page(fade_dc, state, content_x, content_w),
                ManagerTab::Audio => render_audio_page(fade_dc, state, content_x, content_w),
                ManagerTab::Other => render_other_page(fade_dc, state, content_x, content_w),
                ManagerTab::Advanced => render_advanced_page(fade_dc, state, content_x, content_w),
                ManagerTab::About => render_about_page(fade_dc, state, content_x, content_w),
            }

            let _ = windows::Win32::Graphics::Gdi::SetViewportOrgEx(fade_dc, old_fpt.x, old_fpt.y, None);

            let blend = windows::Win32::Graphics::Gdi::BLENDFUNCTION {
                BlendOp: windows::Win32::Graphics::Gdi::AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: alpha,
                AlphaFormat: 0,
            };
            let _ = windows::Win32::Graphics::Gdi::AlphaBlend(
                hdc,
                content_x,
                clip_top,
                clip_w,
                clip_h,
                fade_dc,
                0,
                0,
                clip_w,
                clip_h,
                blend,
            );

            SelectObject(fade_dc, old_fbmp);
            let _ = DeleteObject(fade_bmp);
            let _ = DeleteDC(fade_dc);
        }
    } else {
        let mut old_pt = windows::Win32::Foundation::POINT::default();
        if trans_y > 0 {
            let _ = windows::Win32::Graphics::Gdi::SetViewportOrgEx(hdc, 0, trans_y, Some(&mut old_pt));
        }

        match state.active_tab {
            ManagerTab::Archives => render_archives_page(hdc, state, content_x, content_w),
            ManagerTab::Ebook => render_ebook_page(hdc, state, content_x, content_w),
            ManagerTab::Video => render_video_page(hdc, state, content_x, content_w),
            ManagerTab::Audio => render_audio_page(hdc, state, content_x, content_w),
            ManagerTab::Other => render_other_page(hdc, state, content_x, content_w),
            ManagerTab::Advanced => render_advanced_page(hdc, state, content_x, content_w),
            ManagerTab::About => render_about_page(hdc, state, content_x, content_w),
        }

        if trans_y > 0 {
            let _ = windows::Win32::Graphics::Gdi::SetViewportOrgEx(hdc, old_pt.x, old_pt.y, None);
        }
    }

    let _ = windows::Win32::Graphics::Gdi::RestoreDC(hdc, old_saved);

    // Render modern Fluent vertical scrollbar
    if state.max_scroll_y > 0 {
        let (track_y, track_h, thumb_h, scrollable_track) = get_scrollbar_metrics(state, win_h);
        let track_x = win_w - scrollbar_w - scale_px(4, state.dpi);
        if track_h > scale_px(35, state.dpi) {
            let scroll_ratio = (state.scroll_y as f32 / state.max_scroll_y as f32).clamp(0.0, 1.0);
            let thumb_y = track_y + ((scrollable_track as f32 * scroll_ratio).round() as i32);

            // Track background
            let track_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(th.scrollbar_track));
            let track_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(th.divider));
            let old_b = SelectObject(hdc, track_brush);
            let old_p = SelectObject(hdc, track_pen);
            let _ = RoundRect(hdc, track_x, track_y, track_x + scrollbar_w, track_y + track_h, 6, 6);
            SelectObject(hdc, old_b);
            SelectObject(hdc, old_p);
            let _ = DeleteObject(track_brush);
            let _ = DeleteObject(track_pen);

            // Thumb pill
            let thumb_color = if state.is_dragging_scrollbar {
                th.text_dim
            } else {
                th.scrollbar_thumb
            };
            let thumb_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(thumb_color));
            let thumb_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(thumb_color));
            let old_tb = SelectObject(hdc, thumb_brush);
            let old_tp = SelectObject(hdc, thumb_pen);
            let _ = RoundRect(hdc, track_x + 1, thumb_y, track_x + scrollbar_w - 1, thumb_y + thumb_h, 6, 6);
            SelectObject(hdc, old_tb);
            SelectObject(hdc, old_tp);
            let _ = DeleteObject(thumb_brush);
            let _ = DeleteObject(thumb_pen);
        }
    }
}

// -----------------------------------------------------------------------------
// Individual Fluent Page Renderers
// -----------------------------------------------------------------------------

unsafe fn render_archives_page(hdc: HDC, state: &AppState, x: i32, w: i32) {
    let offset = (if is_media_preview_installed() { scale_px(45, state.dpi) } else { 0 }) - state.scroll_y;

    // Card 1: Checklist of Archive formats
    let card1_y = scale_px(75, state.dpi) + offset;
    let card1_h = get_checklist_card_height(MediaCategory::ImageArchive, state.dpi);
    draw_card_container(hdc, state, x, card1_y, w, card1_h);
    render_extensions_grid(hdc, state, x, card1_y, w, MediaCategory::ImageArchive, 200, tr("關聯的壓縮檔副檔名", "Associated Archive Extensions"));

    // Card 2: Independent Border Style & Live Preview
    let card2_y = card1_y + card1_h + scale_px(14, state.dpi);
    let card2_h = scale_px(145, state.dpi);
    draw_card_container(hdc, state, x, card2_y, w, card2_h);
    render_border_preview_widget(
        hdc,
        state,
        x,
        card2_y,
        w,
        tr("壓縮檔縮圖外框樣式與預覽", "Archive Border Style & Preview"),
        state.settings.border_archive,
        state.settings.badge_static,
        tr("顯示格式辨識角標 (CBZ, CBR, 7Z 等)", "Show Format Badge (CBZ, CBR, 7Z)"),
        "CBZ",
    );

    // Card 3: Independent Collapsible Card for Natural Sort & Continuous Filename Sequence
    let sort_card_y = card2_y + card2_h + scale_px(14, state.dpi);
    let sort_header_h = scale_px(52, state.dpi);
    let sort_card_h = archive_sort_card_height(state.dpi, state.archive_sort_expand_anim);
    draw_expander_card_container(hdc, state, x, sort_card_y, w, sort_card_h, state.is_sort_header_hovered);

    let sort_hovered = state.is_sort_header_hovered;
    let sort_pressed = state.pressed_expander == Some(0);
    if sort_hovered || sort_pressed {
        draw_collapsible_header_hover(
            hdc,
            state,
            x,
            sort_card_y,
            w,
            sort_header_h,
            sort_card_h,
            state.archive_sort_expand_anim > 0.02,
            sort_hovered,
            sort_pressed,
            state.pressed_expander_anim,
        );
    }

    let sort_chevron_w = scale_px(22, state.dpi);
    let sort_chevron_x = x + w - sort_chevron_w - scale_px(16, state.dpi);
    let sort_switch_w = scale_px(40, state.dpi);
    let sort_switch_h = scale_px(20, state.dpi);
    let sort_switch_x = sort_chevron_x - sort_switch_w - scale_px(14, state.dpi);
    let sort_switch_y = sort_card_y + (sort_header_h - sort_switch_h) / 2;

    let th = theme();
    SelectObject(hdc, state.font_bold);
    let sort_title_col = if sort_pressed {
        lerp_color(th.text_bright, th.text_muted, state.pressed_expander_anim.clamp(0.0, 1.0))
    } else {
        th.text_bright
    };
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(sort_title_col));
    let mut sort_title = to_wide(tr("依檔案名稱自然排序圖片", "Sort images naturally"));
    let mut sort_title_rc = RECT {
        left: x + scale_px(18, state.dpi),
        top: sort_card_y,
        right: sort_switch_x - scale_px(12, state.dpi),
        bottom: sort_card_y + sort_header_h,
    };
    let _ = DrawTextW(hdc, &mut sort_title, &mut sort_title_rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);

    draw_toggle_switch(hdc, state, 1, sort_switch_x, sort_switch_y, state.settings.sort_alphabetically, false);
    let sort_ch_cx = sort_chevron_x + sort_chevron_w / 2;
    let sort_ch_cy = sort_card_y + sort_header_h / 2;
    let sort_ch_col = if state.is_archive_sort_expanded { accent_light_bgr() } else { th.text_muted };
    draw_chevron(hdc, state, sort_ch_cx, sort_ch_cy, state.is_archive_sort_expanded, sort_ch_col);

    if state.archive_sort_expand_anim > 0.02 {
        let div_y = sort_card_y + sort_header_h;
        draw_expander_seam(hdc, state, x, div_y, w);

        let rgn = CreateRectRgn(x, div_y, x + w, sort_card_y + sort_card_h);
        let _ = SelectClipRgn(hdc, rgn);

        let sub_disabled = !state.settings.sort_alphabetically;
        draw_risk_checkbox_row(
            hdc,
            state,
            305,
            x,
            div_y + scale_px(8, state.dpi),
            w,
            tr("使用連續檔名的第一個檔案", "Use first file of continuous filename sequence"),
            state.settings.prefer_continuous_sequence,
            sub_disabled,
        );

        let _ = SelectClipRgn(hdc, HRGN::default());
        let _ = DeleteObject(rgn);
    }

    // Card 4: Remaining Archive Extraction Options (3 toggles)
    let opt_card_y = sort_card_y + sort_card_h + scale_px(14, state.dpi);
    let opt_card_h = scale_px(120, state.dpi);
    draw_card_container(hdc, state, x, opt_card_y, w, opt_card_h);

    let mut cy = opt_card_y + scale_px(14, state.dpi);
    let row_h = scale_px(34, state.dpi);

    draw_row_toggle(hdc, state, 2, x, cy, w, tr("自動裁切超長條圖檔 (> 3:1)", "Crop ultra-tall image strips (> 3:1)"), state.settings.crop_tall_strips, false);
    cy += row_h;

    draw_row_toggle(hdc, state, 3, x, cy, w, tr("檔案總管滑鼠懸停資訊 (Tooltip)", "Explorer hover infotip"), state.settings.enable_infotip, false);
    cy += row_h;

    draw_row_toggle(hdc, state, 7, x, cy, w, tr("跳過單色或過於單純之圖片", "Skip plain or text-only pages"), state.settings.filter_plain_covers, false);

    // Card 5: Dedicated Security Risk Badges & Detection (Win11 Expander / Collapsible Card)
    let risk_card_y = opt_card_y + opt_card_h + scale_px(14, state.dpi);
    let header_h = scale_px(64, state.dpi);
    let extra_h = scale_px(210, state.dpi);
    let anim_val = state.risk_card_expand_anim;
    let risk_card_h = header_h + ((extra_h as f32) * anim_val) as i32;

    draw_expander_card_container(hdc, state, x, risk_card_y, w, risk_card_h, state.is_risk_header_hovered);

    // Optional header hover / press background highlight
    let risk_hovered = state.is_risk_header_hovered;
    let risk_pressed = state.pressed_expander == Some(1);
    if risk_hovered || risk_pressed {
        draw_collapsible_header_hover(
            hdc,
            state,
            x,
            risk_card_y,
            w,
            header_h,
            risk_card_h,
            anim_val > 0.02,
            risk_hovered,
            risk_pressed,
            state.pressed_expander_anim,
        );
    }

    // Title: "啟用安全警告角標 (RISK)" (No ICON)
    let card4_pad_x = scale_px(18, state.dpi);
    SelectObject(hdc, state.font_bold);
    let risk_title_col = if risk_pressed {
        lerp_color(th.text_bright, th.text_muted, state.pressed_expander_anim.clamp(0.0, 1.0))
    } else {
        th.text_bright
    };
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(risk_title_col));
    let r_title = to_wide(tr("啟用安全警告角標 (RISK)", "Enable Security Warning Badge (RISK)"));
    let _ = TextOutW(hdc, x + card4_pad_x, risk_card_y + scale_px(13, state.dpi), &r_title);

    // Subtitle / Description
    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_muted));
    let r_desc = to_wide(tr("當壓縮檔內檢測到潛在風險項目時，在縮圖邊角繪製橘紅 [RISK] 警告角標", "Overlays an orange-red [RISK] badge when anomalies or dangerous files are detected"));
    let _ = TextOutW(hdc, x + card4_pad_x, risk_card_y + scale_px(34, state.dpi), &r_desc);

    // Right side: Chevron arrow + Master switch
    let chevron_w = scale_px(22, state.dpi);
    let chevron_x = x + w - chevron_w - scale_px(16, state.dpi);

    let switch_w = scale_px(40, state.dpi);
    let switch_h = scale_px(20, state.dpi);
    let switch_x = chevron_x - switch_w - scale_px(14, state.dpi);
    let switch_y = risk_card_y + (header_h - switch_h) / 2;

    // Draw Master switch (ID 4)
    draw_toggle_switch(hdc, state, 4, switch_x, switch_y, state.settings.badge_risk, false);

    // Draw Chevron: Fluent glyph with ClearType antialiasing
    let ch_cx = chevron_x + chevron_w / 2;
    let ch_cy = risk_card_y + header_h / 2;
    let ch_col = if state.is_risk_card_expanded { accent_light_bgr() } else { th.text_muted };
    draw_chevron(hdc, state, ch_cx, ch_cy, state.is_risk_card_expanded, ch_col);

    // Expanded Sub-items (Left) + RISK Badge Live Preview Diagram (Right)
    if anim_val > 0.02 {
        // Divider line between header and sub-items
        let div_y = risk_card_y + header_h;
        draw_expander_seam(hdc, state, x, div_y, w);

        // Clip region to prevent overflow during expand/collapse animation
        let rgn = CreateRectRgn(x, div_y, x + w, risk_card_y + risk_card_h);
        let _ = SelectClipRgn(hdc, rgn);

        let sub_disabled = !state.settings.badge_risk;
        let mut scy = div_y + scale_px(8, state.dpi);
        let sub_row_h = scale_px(31, state.dpi);
        let rules_w = w - scale_px(184, state.dpi);

        draw_risk_checkbox_row(hdc, state, 41, x, scy, rules_w, tr("存在可執行檔與指令碼 (.exe, .bat, .cmd, .ps1, .scr)", "Presence of Executables & Scripts (.exe, .bat, etc.)"), state.settings.risk_rule_executables, sub_disabled);
        scy += sub_row_h;

        draw_risk_checkbox_row(hdc, state, 42, x, scy, rules_w, tr("路徑遍歷逃逸與絕對路徑 (.. / 磁碟機代號 / ADS)", "Directory Traversal & Absolute Paths (../, Drive:, ADS)"), state.settings.risk_rule_traversal, sub_disabled);
        scy += sub_row_h;

        draw_risk_checkbox_row(hdc, state, 43, x, scy, rules_w, tr("隱藏檔與點目錄 (.git/, .env, __MACOSX)", "Hidden Files & Dot Directories (.git/, .env, etc.)"), state.settings.risk_rule_hidden_files, sub_disabled);
        scy += sub_row_h;

        draw_risk_checkbox_row(hdc, state, 44, x, scy, rules_w, tr("純文字與說明文件 (.txt, .nfo, .diz)", "Plain Text & Documentation (.txt, .nfo, .diz)"), state.settings.risk_rule_text_files, sub_disabled);
        scy += sub_row_h;

        draw_risk_checkbox_row(hdc, state, 45, x, scy, rules_w, tr("憑證密鑰與敏感檔案 (.key, .pem, .pfx)", "Certificates & Sensitive Keys (.key, .pem, .pfx)"), state.settings.risk_rule_sensitive, sub_disabled);
        scy += sub_row_h;

        draw_risk_checkbox_row(hdc, state, 46, x, scy, rules_w, tr("名稱規避與雙副檔名 (Unicode RTLO, 結尾點/空格)", "Name Spoofing & Evasion (RTLO, trailing dot/spaces)"), state.settings.risk_rule_evasion, sub_disabled);

        // Right-side preview box centered vertically without text caption
        let prev_w = scale_px(156, state.dpi);
        let prev_x = x + w - prev_w - scale_px(18, state.dpi);
        let prev_box_y = div_y + scale_px(18, state.dpi);
        let prev_box_h = scale_px(174, state.dpi);
        let prev_bg = CreateSolidBrush(windows::Win32::Foundation::COLORREF(th.sub_card_bg));
        let prev_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(th.sub_card_border));
        let old_pb = SelectObject(hdc, prev_bg);
        let old_pp = SelectObject(hdc, prev_pen);
        let _ = RoundRect(hdc, prev_x, prev_box_y, prev_x + prev_w, prev_box_y + prev_box_h, 8, 8);
        SelectObject(hdc, old_pb);
        SelectObject(hdc, old_pp);
        let _ = DeleteObject(prev_bg);
        let _ = DeleteObject(prev_pen);

        // Simulated Archive Cover Thumbnail inside preview box
        let thumb_w = scale_px(86, state.dpi);
        let thumb_h = scale_px(110, state.dpi);
        let thumb_x = prev_x + (prev_w - thumb_w) / 2;
        let thumb_y = prev_box_y + (prev_box_h - thumb_h) / 2;
        let thumb_r = match state.settings.border_archive {
            1 => scale_px(8, state.dpi),
            2 => scale_px(9, state.dpi),
            3 => scale_px(6, state.dpi),
            4 => scale_px(9, state.dpi),
            _ => 4,
        };
        if state.settings.border_archive == 2 {
            draw_win11_drop_shadow_layers(hdc, thumb_x, thumb_y, thumb_w, thumb_h, thumb_r);
        } else if state.settings.border_archive == 3 {
            let fpad = scale_px(4, state.dpi);
            let r_fr = scale_px(10, state.dpi);
            draw_win11_drop_shadow_layers(
                hdc,
                thumb_x - fpad,
                thumb_y - fpad,
                thumb_w + fpad * 2,
                thumb_h + fpad * 2,
                r_fr,
            );
            let fr_b = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00EAECEF));
            let fr_p = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(0x00C6CCD6));
            let ofb = SelectObject(hdc, fr_b);
            let ofp = SelectObject(hdc, fr_p);
            let _ = RoundRect(
                hdc,
                thumb_x - fpad,
                thumb_y - fpad,
                thumb_x + thumb_w + fpad,
                thumb_y + thumb_h + fpad,
                r_fr,
                r_fr,
            );
            SelectObject(hdc, ofb);
            SelectObject(hdc, ofp);
            let _ = DeleteObject(fr_b);
            let _ = DeleteObject(fr_p);
        }

        let cover_b = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00583824));
        let cover_p = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(0x00785038));
        let ocb = SelectObject(hdc, cover_b);
        let ocp = SelectObject(hdc, cover_p);
        let _ = RoundRect(hdc, thumb_x, thumb_y, thumb_x + thumb_w, thumb_y + thumb_h, thumb_r, thumb_r);
        SelectObject(hdc, ocb);
        SelectObject(hdc, ocp);
        let _ = DeleteObject(cover_b);
        let _ = DeleteObject(cover_p);

        // Simulated cover lines
        let line_b = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x008A6852));
        for li in 0..3 {
            let ly = thumb_y + scale_px(44 + li * 16, state.dpi);
            let lr = RECT {
                left: thumb_x + scale_px(14, state.dpi),
                top: ly,
                right: thumb_x + thumb_w - scale_px(14 + li * 8, state.dpi),
                bottom: ly + scale_px(5, state.dpi),
            };
            FillRect(hdc, &lr, line_b);
        }
        let _ = DeleteObject(line_b);

        // Top-left orange-red [RISK] warning badge matching badge.rs
        let (badge_bg_col, badge_bd_col, badge_txt_col) = if state.settings.badge_risk {
            (0x001C1CB9u32, 0x005555FFu32, 0x00FFFFFFu32)
        } else {
            (0x0026264Au32, 0x00484870u32, 0x00A8A8B0u32)
        };
        let badge_w = scale_px(38, state.dpi);
        let badge_h = scale_px(17, state.dpi);
        let badge_x = thumb_x + scale_px(5, state.dpi);
        let badge_y = thumb_y + scale_px(5, state.dpi);
        let r_b = CreateSolidBrush(windows::Win32::Foundation::COLORREF(badge_bg_col));
        let r_p = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(badge_bd_col));
        let orb = SelectObject(hdc, r_b);
        let orp = SelectObject(hdc, r_p);
        let _ = RoundRect(hdc, badge_x, badge_y, badge_x + badge_w, badge_y + badge_h, 3, 3);
        SelectObject(hdc, orb);
        SelectObject(hdc, orp);
        let _ = DeleteObject(r_b);
        let _ = DeleteObject(r_p);

        SelectObject(hdc, state.font_small);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(badge_txt_col));
        let mut rsk = to_wide("RISK");
        let mut rsk_rc = RECT {
            left: badge_x,
            top: badge_y,
            right: badge_x + badge_w,
            bottom: badge_y + badge_h,
        };
        let _ = DrawTextW(hdc, &mut rsk, &mut rsk_rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);

        let _ = SelectClipRgn(hdc, HRGN::default());
        let _ = DeleteObject(rgn);
    }
}

unsafe fn render_ebook_page(hdc: HDC, state: &AppState, x: i32, w: i32) {
    let offset = (if is_media_preview_installed() { scale_px(45, state.dpi) } else { 0 }) - state.scroll_y;

    // Card 1: Checklist of E-Book formats (.epub, .mobi, .iba, .azw, .pdf)
    let card1_y = scale_px(75, state.dpi) + offset;
    let card1_h = get_checklist_card_height(MediaCategory::Ebook, state.dpi);
    draw_card_container(hdc, state, x, card1_y, w, card1_h);
    render_extensions_grid(
        hdc,
        state,
        x,
        card1_y,
        w,
        MediaCategory::Ebook,
        200,
        tr(
            "關聯的電子書副檔名",
            "Associated E-Book Extensions",
        ),
    );

    // Card 2: Independent E-Book Book Cover Border Style & Live Preview
    let card2_y = card1_y + card1_h + scale_px(16, state.dpi);
    let card2_h = scale_px(145, state.dpi);
    draw_card_container(hdc, state, x, card2_y, w, card2_h);
    render_border_preview_widget(
        hdc,
        state,
        x,
        card2_y,
        w,
        tr("電子書縮圖外框樣式與書籍封面預覽", "E-Book Border Style & Book Cover Preview"),
        state.settings.border_ebook,
        false,
        "",
        "",
    );
    let hint_w = to_wide(tr(
        "縮圖右下角將自動顯示預設開啟應用程式圖示",
        "Bottom-right corner displays your associated reader app icon",
    ));
    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(theme().text_dim));
    let _ = TextOutW(
        hdc,
        x + scale_px(16, state.dpi),
        card2_y + scale_px(98, state.dpi),
        &hint_w,
    );
}

unsafe fn render_video_page(hdc: HDC, state: &AppState, x: i32, w: i32) {
    let offset = (if is_media_preview_installed() { scale_px(45, state.dpi) } else { 0 }) - state.scroll_y;

    // Card 1: Checklist of Video formats
    let card1_y = scale_px(75, state.dpi) + offset;
    let card1_h = get_checklist_card_height(MediaCategory::Video, state.dpi);
    draw_card_container(hdc, state, x, card1_y, w, card1_h);
    render_extensions_grid(hdc, state, x, card1_y, w, MediaCategory::Video, 200, tr("關聯的影片副檔名", "Associated Video Extensions"));

    // Card 2: Execution Mode (Safe Mode vs Native Mode)
    let card2_y = card1_y + card1_h + scale_px(14, state.dpi);
    let card2_h = scale_px(120, state.dpi);
    draw_card_container(hdc, state, x, card2_y, w, card2_h);

    let th = theme();
    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_primary));
    let t_mode = to_wide(tr("影片解碼執行模式", "Video Decoding Execution Mode"));
    let _ = TextOutW(hdc, x + scale_px(16, state.dpi), card2_y + scale_px(14, state.dpi), &t_mode);

    // Mode Buttons
    let btn_y = card2_y + scale_px(40, state.dpi);
    let btn_h = scale_px(32, state.dpi);
    let b1_w = scale_px(150, state.dpi);
    let b2_w = scale_px(150, state.dpi);
    let b1_x = x + scale_px(16, state.dpi);
    let b2_x = b1_x + b1_w + scale_px(10, state.dpi);

    let is_sandbox = state.settings.video_sandbox_mode;
    draw_selectable_button(
        hdc,
        state,
        220,
        b1_x,
        btn_y,
        b1_w,
        btn_h,
        tr("隔離模式", "Isolation Mode"),
        is_sandbox,
        false,
    );
    draw_selectable_button(
        hdc,
        state,
        221,
        b2_x,
        btn_y,
        b2_w,
        btn_h,
        tr("原生模式", "Native Mode"),
        !is_sandbox,
        false,
    );

    // Neutral description text
    SelectObject(hdc, state.font_regular);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_muted));
    let desc = if is_sandbox {
        tr(
            "在獨立的 COM 代理處理序 (dllhost.exe) 中載入與執行解碼。若處理異常檔案時解碼器發生未預期的錯誤，僅會終止該代理處理序，不影響檔案總管主程式之運作。\n（切換後建議重新啟動檔案總管以使新處理序模式全面生效）",
            "Executes decoding inside a separate COM surrogate process (dllhost.exe). If an unexpected decoder error occurs on a malformed file, only the surrogate process terminates, leaving Windows Explorer unaffected.\n(Restarting Windows Explorer is recommended for changes to fully take effect)",
        )
    } else {
        tr(
            "直接於檔案總管 (explorer.exe) 處理序內部載入動態連結庫並執行解碼。由檔案總管直接傳遞實體檔案路徑，省去跨處理程序調度開銷。\n（切換後建議重新啟動檔案總管以使新處理序模式全面生效）",
            "Loads the dynamic link library and decodes directly inside the Windows Explorer (explorer.exe) process, avoiding inter-process proxy overhead.\n(Restarting Windows Explorer is recommended for changes to fully take effect)",
        )
    };
    let mut desc_w = to_wide(desc);
    let mut rect_desc = RECT {
        left: x + scale_px(18, state.dpi),
        top: btn_y + btn_h + scale_px(8, state.dpi),
        right: x + w - scale_px(20, state.dpi),
        bottom: card2_y + card2_h - scale_px(6, state.dpi),
    };
    let _ = DrawTextW(hdc, &mut desc_w, &mut rect_desc, DT_LEFT | DT_WORDBREAK);

    // Card 2.5: Merged FFmpeg Component Path & Everything Search Helper Card
    let card_tools_y = card2_y + card2_h + scale_px(14, state.dpi);
    let note_ff = tr(
        "調用 FFmpeg 進行 Windows 原生解碼器不支援之規格（如 10-bit H.264/Hi10P、AV1、VP9、ProRes、特定 HEVC 設定檔或非標準 MKV/WebM 容器）的時長探測與影格擷取",
        "Invokes FFmpeg for duration probing and frame extraction on formats unsupported by the native Windows decoder (such as 10-bit H.264/Hi10P, AV1, VP9, ProRes, custom HEVC profiles, or non-standard MKV/WebM containers).",
    );
    let note_ev = tr(
        "當檔案總管在隔離模式下僅傳遞串流時，透過本機已安裝之 Everything 索引服務查詢實體檔案路徑以啟用原生解碼管線（預設只採用本機磁碟的結果；網路共用資料夾需開啟下方選項）。\n若系統未安裝或未開啟 Everything，將自動切換至本機目錄快取、網址列歷程與使用者資料夾搜尋。",
        "When Windows Explorer passes streams in isolation mode, queries the locally installed Everything index service to locate the physical file path for native decoding (local drives only by default; enable the option below for network shares).\nIf Everything is not installed or running, automatically falls back to local directory cache, TypedPaths, and user folder lookup.",
    );

    SelectObject(hdc, state.font_small);
    let note_max_w = (w - scale_px(36, state.dpi)).max(100);
    let mut rect_note_calc = RECT { left: 0, top: 0, right: note_max_w, bottom: 0 };
    let mut nw_calc = to_wide(note_ff);
    let _ = DrawTextW(hdc, &mut nw_calc, &mut rect_note_calc, DT_CALCRECT | DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);
    let note_ff_h = (rect_note_calc.bottom - rect_note_calc.top).max(scale_px(16, state.dpi));


    let btn_h = scale_px(30, state.dpi);
    let ff_btn_y = card_tools_y + scale_px(40, state.dpi);
    let ff_note_top = ff_btn_y + btn_h + scale_px(8, state.dpi);
    let ff_card_h = (ff_note_top + note_ff_h + scale_px(14, state.dpi)) - card_tools_y;

    // Everything search helper: its own expander card right below the FFmpeg card. The network
    // share settings are its body rows, visible only while expanded.
    let ev_card_y = card_tools_y + ff_card_h + scale_px(14, state.dpi);
    let ev_header_h = scale_px(52, state.dpi);
    let ev_indent = scale_px(EXPANDER_CONTENT_INDENT, state.dpi);
    let ev_row_x = x + ev_indent;
    let ev_row_w = w - ev_indent * 2;
    let ev_note_w = (ev_row_w - scale_px(32, state.dpi)).max(scale_px(100, state.dpi));
    let mut ev_note_calc = RECT { left: 0, top: 0, right: ev_note_w, bottom: 0 };
    let mut ev_note_calc_w = to_wide(note_ev);
    SelectObject(hdc, state.font_small);
    let _ = DrawTextW(hdc, &mut ev_note_calc_w, &mut ev_note_calc, DT_CALCRECT | DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);
    let ev_note_h = (ev_note_calc.bottom - ev_note_calc.top).max(scale_px(16, state.dpi));
    let ev_note_top = ev_card_y + ev_header_h + scale_px(12, state.dpi);
    let net_rows_y = ev_note_top + ev_note_h + scale_px(12, state.dpi);
    let ev_body_h = (net_rows_y + net_share_rows_height(state)) - (ev_card_y + ev_header_h);
    let ev_card_h = ev_header_h + (ev_body_h as f32 * state.ev_expand_anim.clamp(0.0, 1.0)).round() as i32;
    let btn_rf_y = ev_card_y + scale_px(13, state.dpi);

    // Layout feedback for hit-testing; `Cell` because rendering only borrows the state immutably.
    // `video_card_tools_h` spans both cards so the cards below keep their positions.
    let card_tools_h = (ev_card_y + ev_card_h) - card_tools_y;
    state.video_card_tools_h.set(card_tools_h);
    state.video_ev_card_rel_y.set(ev_card_y - card_tools_y);
    state.video_everything_btn_rel_y.set(btn_rf_y - card_tools_y);
    state.video_net_sub_rel_y.set(net_rows_y - card_tools_y);
    draw_card_container(hdc, state, x, card_tools_y, w, ff_card_h);

    // Section A: FFmpeg Component Path
    let th = theme();
    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_primary));
    let t_ff = to_wide(tr("FFmpeg 元件路徑", "FFmpeg Component Path"));
    let _ = TextOutW(hdc, x + scale_px(16, state.dpi), card_tools_y + scale_px(14, state.dpi), &t_ff);

    let btn_auto_w = scale_px(100, state.dpi);
    let btn_browse_w = scale_px(95, state.dpi);
    let btn_y = ff_btn_y;
    let btn_auto_x = x + w - scale_px(16, state.dpi) - btn_auto_w;
    let btn_browse_x = btn_auto_x - scale_px(8, state.dpi) - btn_browse_w;

    let is_already_auto = state.settings.custom_ffmpeg_path.trim().is_empty();
    draw_rounded_button(hdc, state, 340, btn_browse_x, btn_y, btn_browse_w, btn_h, tr("📁 瀏覽...", "Browse..."), false, false);
    draw_rounded_button(hdc, state, 341, btn_auto_x, btn_y, btn_auto_w, btn_h, tr("🔄 自動檢測", "Auto-Detect"), is_already_auto, false);

    let box_x = x + scale_px(16, state.dpi);
    let box_w = (btn_browse_x - box_x - scale_px(10, state.dpi)).max(scale_px(150, state.dpi));
    let box_y = btn_y;
    let box_h = btn_h;

    let bg_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(th.input_bg));
    let border_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(th.input_border));
    let old_b = SelectObject(hdc, bg_brush);
    let old_p = SelectObject(hdc, border_pen);
    let r_box = scale_px(5, state.dpi);
    let _ = RoundRect(hdc, box_x, box_y, box_x + box_w, box_y + box_h, r_box, r_box);
    SelectObject(hdc, old_b);
    SelectObject(hdc, old_p);
    let _ = DeleteObject(bg_brush);
    let _ = DeleteObject(border_pen);

    let (resolved_path, is_custom) = cbx_core::media::video_mf::resolve_ffmpeg_status(&state.settings.custom_ffmpeg_path);

    let (badge_txt, badge_color) = if is_custom {
        if resolved_path.is_some() {
            (tr("[手動指定]", "[Manual]"), th.status_ok)
        } else {
            (tr("[手動指定 - 檔案不存在]", "[Manual - Not Found]"), th.status_err)
        }
    } else {
        if resolved_path.is_some() {
            (tr("[自動檢測]", "[Auto]"), accent_light_bgr())
        } else {
            (tr("[未安裝]", "[Not Found]"), th.text_disabled)
        }
    };

    SelectObject(hdc, state.font_small);
    let mut badge_w = to_wide(badge_txt);
    let mut rect_badge_calc = RECT::default();
    let _ = DrawTextW(hdc, &mut badge_w, &mut rect_badge_calc, DT_CALCRECT | DT_SINGLELINE);
    let badge_width = rect_badge_calc.right - rect_badge_calc.left + scale_px(12, state.dpi);

    let mut badge_rect = RECT {
        left: box_x + box_w - badge_width - scale_px(6, state.dpi),
        top: box_y,
        right: box_x + box_w - scale_px(6, state.dpi),
        bottom: box_y + box_h,
    };
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(badge_color));
    let mut badge_w_draw = to_wide(badge_txt);
    let _ = DrawTextW(hdc, &mut badge_w_draw, &mut badge_rect, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);

    let path_display = if is_custom {
        if let Some(p) = &resolved_path {
            p.to_string_lossy().to_string()
        } else {
            state.settings.custom_ffmpeg_path.clone()
        }
    } else {
        if let Some(p) = &resolved_path {
            p.to_string_lossy().to_string()
        } else {
            tr("檢測不到", "Not detected").to_string()
        }
    };

    let text_color = if resolved_path.is_some() {
        th.text_primary
    } else if th.dark {
        0x007878D0
    } else {
        th.status_err
    };

    SelectObject(hdc, state.font_regular);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(text_color));
    let mut path_w = to_wide(&path_display);
    let mut path_rect = RECT {
        left: box_x + scale_px(10, state.dpi),
        top: box_y,
        right: badge_rect.left - scale_px(6, state.dpi),
        bottom: box_y + box_h,
    };
    let _ = DrawTextW(hdc, &mut path_w, &mut path_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_PATH_ELLIPSIS);

    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_dim));
    let mut note_rect = RECT {
        left: x + scale_px(18, state.dpi),
        top: ff_note_top,
        right: x + scale_px(18, state.dpi) + note_max_w,
        bottom: ff_note_top + note_ff_h,
    };
    let mut nw_draw = to_wide(note_ff);
    let _ = DrawTextW(hdc, &mut nw_draw, &mut note_rect, DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);

    // Card: Everything Search Helper (expander)
    let th = theme();
    draw_expander_card_container(hdc, state, x, ev_card_y, w, ev_card_h, state.is_ev_header_hovered);
    let ev_hovered = state.is_ev_header_hovered;
    let ev_pressed = state.pressed_expander == Some(3);
    if ev_hovered || ev_pressed {
        draw_collapsible_header_hover(
            hdc,
            state,
            x,
            ev_card_y,
            w,
            ev_header_h,
            ev_card_h,
            state.ev_expand_anim > 0.02,
            ev_hovered,
            ev_pressed,
            state.pressed_expander_anim,
        );
    }

    let chevron_w = scale_px(22, state.dpi);
    let chevron_x = x + w - chevron_w - scale_px(16, state.dpi);
    let btn_rf_w = scale_px(100, state.dpi);
    let btn_rf_h = scale_px(26, state.dpi);
    let btn_rf_x = chevron_x - btn_rf_w - scale_px(14, state.dpi);
    draw_rounded_button(hdc, state, 342, btn_rf_x, btn_rf_y, btn_rf_w, btn_rf_h, tr("🔄 重新整理", "Refresh"), false, false);

    SelectObject(hdc, state.font_bold);
    let ev_title_col = if ev_pressed {
        lerp_color(th.text_primary, th.text_muted, state.pressed_expander_anim.clamp(0.0, 1.0))
    } else {
        th.text_primary
    };
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(ev_title_col));
    let ev_title = tr("Everything 搜尋輔助", "Everything Search Helper");
    let ev_title_w = to_wide(ev_title);
    let _ = TextOutW(hdc, x + scale_px(16, state.dpi), btn_rf_y + scale_px(3, state.dpi), &ev_title_w);
    let title_px = text_width(hdc, ev_title);

    let ev_ver_opt = {
        let mut cache = state.cached_everything_version.borrow_mut();
        if cache.is_none() {
            *cache = Some(cbx_core::media::get_everything_version());
        }
        cache.as_ref().and_then(|v| v.clone())
    };
    let (ev_status_text, ev_status_col) = if let Some(ver) = ev_ver_opt {
        (
            if matches!(current_lang(), UiLang::En) { format!("Everything {ver} service active") } else { format!("Everything {ver} 服務運作中") },
            th.status_ok,
        )
    } else {
        (
            tr("未偵測到 Everything（使用目錄快取與歷程比對）", "Everything not detected (folder cache & history)").to_string(),
            th.text_muted,
        )
    };
    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(ev_status_col));
    let mut ev_status_w = to_wide(&ev_status_text);
    let mut ev_status_rect = RECT {
        left: x + scale_px(16, state.dpi) + title_px + scale_px(16, state.dpi),
        top: ev_card_y,
        right: btn_rf_x - scale_px(12, state.dpi),
        bottom: ev_card_y + ev_header_h,
    };
    let _ = DrawTextW(hdc, &mut ev_status_w, &mut ev_status_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);

    let ch_col = if state.is_ev_expanded { accent_light_bgr() } else { th.text_muted };
    draw_chevron(hdc, state, chevron_x + chevron_w / 2, ev_card_y + ev_header_h / 2, state.is_ev_expanded, ch_col);

    if state.ev_expand_anim > 0.02 {
        draw_expander_seam(hdc, state, x, ev_card_y + ev_header_h, w);
        let rgn = CreateRectRgn(x - 2, ev_card_y + ev_header_h + 1, x + w + 2, ev_card_y + ev_card_h + 2);
        let _ = SelectClipRgn(hdc, rgn);

        SelectObject(hdc, state.font_small);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_dim));
        let mut note_ev_rect = RECT {
            left: ev_row_x + scale_px(16, state.dpi),
            top: ev_note_top,
            right: ev_row_x + scale_px(16, state.dpi) + ev_note_w,
            bottom: ev_note_top + ev_note_h,
        };
        let mut nw_ev_draw = to_wide(note_ev);
        let _ = DrawTextW(hdc, &mut nw_ev_draw, &mut note_ev_rect, DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);

        render_net_share_rows(hdc, state, x, w, ev_row_x, net_rows_y, ev_row_w);

        let _ = SelectClipRgn(hdc, HRGN::default());
        let _ = DeleteObject(rgn);
    }

    // Card 3: Independent Border Style & Live Preview (Video corner badge removed)
    let card3_y = card_tools_y + card_tools_h + scale_px(14, state.dpi);
    let card3_h = scale_px(130, state.dpi);
    draw_card_container(hdc, state, x, card3_y, w, card3_h);
    render_border_preview_widget(
        hdc,
        state,
        x,
        card3_y,
        w,
        tr("影片縮圖外框樣式與預覽", "Video Border Style & Preview"),
        state.settings.border_video,
        false,
        "",
        "",
    );

    // Card 4 (Moved above Custom Algorithm Parameters): Dedicated HDR ACES Tone-Mapping Card
    let card_hdr_y = card3_y + card3_h + scale_px(14, state.dpi);
    let card_hdr_h = scale_px(86, state.dpi);
    draw_card_container(hdc, state, x, card_hdr_y, w, card_hdr_h);

    let hdr_pad_x = scale_px(18, state.dpi);
    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_bright));
    let t_hdr = to_wide(tr("HDR ACES 色調映射", "HDR ACES Tone-Mapping"));
    let _ = TextOutW(hdc, x + hdr_pad_x, card_hdr_y + scale_px(14, state.dpi), &t_hdr);

    let hdr_sw_w = scale_px(40, state.dpi);
    let hdr_sw_x = x + w - hdr_sw_w - scale_px(52, state.dpi);
    let hdr_sw_y = card_hdr_y + scale_px(14, state.dpi);
    draw_toggle_switch(hdc, state, 13, hdr_sw_x, hdr_sw_y, state.settings.hdr_tonemap_enabled, false);

    SelectObject(hdc, state.font_regular);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_muted));
    let hdr_purpose = tr(
        "將 HDR (HDR10 / HLG / BT.2020) 影片之高動態範圍色調映射至標準 sRGB，避免縮圖畫面泛白；一般 SDR 影片保持直通處理。",
        "Applies ACES tone-mapping from HDR (HDR10 / HLG / BT.2020) to sRGB to avoid washed-out thumbnails; SDR videos pass through unchanged.",
    );
    let mut hp_w = to_wide(hdr_purpose);
    let mut rect_hp = RECT {
        left: x + hdr_pad_x,
        top: card_hdr_y + scale_px(42, state.dpi),
        right: x + w - scale_px(20, state.dpi),
        bottom: card_hdr_y + card_hdr_h - scale_px(8, state.dpi),
    };
    let _ = DrawTextW(hdc, &mut hp_w, &mut rect_hp, DT_LEFT | DT_WORDBREAK);

    // Card 5: Smart Rendering parameters container (Win11 Collapsible Card)
    let card4_y = card_hdr_y + card_hdr_h + scale_px(14, state.dpi);
    let header_h = scale_px(52, state.dpi);
    let card4_h = video_card4_height(state.dpi, state.video_params_expand_anim);
    draw_expander_card_container(hdc, state, x, card4_y, w, card4_h, state.is_video_params_header_hovered);

    let vp_hovered = state.is_video_params_header_hovered;
    let vp_pressed = state.pressed_expander == Some(2);
    if vp_hovered || vp_pressed {
        draw_collapsible_header_hover(
            hdc,
            state,
            x,
            card4_y,
            w,
            header_h,
            card4_h,
            state.video_params_expand_anim > 0.02,
            vp_hovered,
            vp_pressed,
            state.pressed_expander_anim,
        );
    }

    let cy = card4_y + scale_px(13, state.dpi);
    let is_custom = state.settings.custom_video_settings;

    let chevron_w = scale_px(22, state.dpi);
    let chevron_x = x + w - chevron_w - scale_px(16, state.dpi);
    let switch_w = scale_px(40, state.dpi);
    let switch_x = chevron_x - switch_w - scale_px(14, state.dpi);

    // Header row: "自訂演算法參數" + [ 套用 ] + [ 🔄 回復預設 ] + Toggle Switch + Chevron
    let reset_w = scale_px(95, state.dpi);
    let reset_h = scale_px(26, state.dpi);
    let reset_x = switch_x - reset_w - scale_px(12, state.dpi);
    let reset_y = cy;
    draw_rounded_button(hdc, state, 303, reset_x, reset_y, reset_w, reset_h, tr("🔄 回復預設", "Restore"), false, !is_custom);

    let apply_w = scale_px(85, state.dpi);
    let apply_h = scale_px(26, state.dpi);
    let apply_x = reset_x - apply_w - scale_px(8, state.dpi);
    let apply_y = reset_y;
    let is_applied = state.applied_custom_timer.map_or(false, |t| t.elapsed().as_secs_f32() < 1.5);
    let apply_text = if is_applied {
        tr("✓ 已套用", "✓ Applied")
    } else {
        tr("套用", "Apply")
    };
    draw_rounded_button(hdc, state, 305, apply_x, apply_y, apply_w, apply_h, apply_text, is_applied, !is_custom);

    let th = theme();
    SelectObject(hdc, state.font_bold);
    let vp_title_col = if vp_pressed {
        lerp_color(th.text_primary, th.text_muted, state.pressed_expander_anim.clamp(0.0, 1.0))
    } else {
        th.text_primary
    };
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(vp_title_col));
    let hdr_lbl = to_wide(tr("自訂演算法參數", "Customize Algorithm Parameters"));
    let _ = TextOutW(hdc, x + scale_px(16, state.dpi), cy + scale_px(3, state.dpi), &hdr_lbl);

    draw_toggle_switch(hdc, state, 10, switch_x, cy + scale_px(3, state.dpi), is_custom, false);

    let ch_cx = chevron_x + chevron_w / 2;
    let ch_cy = card4_y + header_h / 2;
    let ch_col = if state.is_video_params_expanded { accent_light_bgr() } else { th.text_muted };
    draw_chevron(hdc, state, ch_cx, ch_cy, state.is_video_params_expanded, ch_col);

    if state.video_params_expand_anim > 0.02 {
    // Divider Line 1
    let div1_y = card4_y + header_h;
    let rgn = CreateRectRgn(x - 2, div1_y, x + w + 2, card4_y + card4_h + 2);
    let _ = SelectClipRgn(hdc, rgn);
    let _ = SelectClipRgn(hdc, None);
    draw_expander_seam(hdc, state, x, div1_y, w);
    let _ = SelectClipRgn(hdc, rgn);

    // Expander body rows are inset from both card edges; seams still cut the full card width.
    let (card_x, card_w) = (x, w);
    let indent = scale_px(EXPANDER_CONTENT_INDENT, state.dpi);
    let x = x + indent;
    let w = w - indent * 2;

    // Section 1: Frame selection policy
    let mut c1_y = div1_y + scale_px(10, state.dpi);
    let row_h = scale_px(32, state.dpi);

    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, if is_custom { windows::Win32::Foundation::COLORREF(th.text_secondary) } else { windows::Win32::Foundation::COLORREF(th.text_disabled) });
    let mode_lbl = to_wide(tr("影格選取原則", "Frame Selection Policy"));
    let _ = TextOutW(hdc, x + scale_px(16, state.dpi), c1_y + scale_px(2, state.dpi), &mode_lbl);

    let seg_w = scale_px(90, state.dpi);
    let seg_h = scale_px(22, state.dpi);
    let seg_x1 = x + scale_px(250, state.dpi);
    let seg_x2 = seg_x1 + seg_w + scale_px(6, state.dpi);
    let seg_x3 = seg_x2 + seg_w + scale_px(6, state.dpi);
    draw_selectable_button(hdc, state, 322, seg_x1, c1_y + scale_px(1, state.dpi), seg_w, seg_h, tr("動態比例", "Dynamic Ratio"), state.settings.seek_mode == 2, !is_custom);
    draw_selectable_button(hdc, state, 320, seg_x2, c1_y + scale_px(1, state.dpi), seg_w, seg_h, tr("百分比定位", "Percent"), state.settings.seek_mode == 0, !is_custom);
    draw_selectable_button(hdc, state, 321, seg_x3, c1_y + scale_px(1, state.dpi), seg_w, seg_h, tr("跳過固定時長", "Fixed Offset"), state.settings.seek_mode == 1, !is_custom);
    c1_y += row_h;

    // 1a. 片長百分比
    let pct_disabled = !is_custom || state.settings.seek_mode == 1;
    draw_row_slider(hdc, state, 0, x, c1_y, w, tr("片長百分比", "Duration Percentage"), state.settings.position_seek, "%", pct_disabled, true);
    c1_y += row_h;

    // 1b. 跳過前導秒數
    let fix_disabled = !is_custom || state.settings.seek_mode == 0;
    draw_row_number_input(hdc, state, EditTarget::SeekFixedSec, x, c1_y, w, tr("跳過前導秒數", "Skip Offset Seconds"), state.settings.seek_fixed_sec, tr("秒", "s"), fix_disabled);
    c1_y += row_h;

    // 1c. 長片切換門檻
    let thres_disabled = !is_custom || state.settings.seek_mode != 2;
    draw_row_number_input(hdc, state, EditTarget::SeekThresholdMin, x, c1_y, w, tr("長片切換門檻", "Duration Threshold"), state.settings.seek_threshold_min, tr("分", "min"), thres_disabled);
    c1_y += row_h;

    // 1d. 跳步探測間距 (加寬說明與下行間距)
    draw_row_number_input(hdc, state, EditTarget::StepProbeSec, x, c1_y, w, tr("跳步探測間距", "Keyframe Step Interval"), state.settings.step_probe_sec, tr("秒", "s"), !is_custom);
    let note_step_y = c1_y + scale_px(27, state.dpi);
    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_dim));
    let note_step = to_wide(tr("影格未達標或跨 GOP 推進時，向後定位下一個關鍵影格的秒數。", "Forward seek step when probing distinct keyframes or advancing past GOP snap-back."));
    let _ = TextOutW(hdc, x + scale_px(32, state.dpi), note_step_y, &note_step);
    c1_y += scale_px(58, state.dpi);

    // 1d2. 跨 GOP 防回彈推進
    draw_row_number_input(hdc, state, EditTarget::MaxGopHops, x, c1_y, w, tr("跨 GOP 防回彈推進", "GOP Snap-back Hops"), state.settings.max_gop_hops, tr("次", "hops"), !is_custom);
    let note_gop_y = c1_y + scale_px(27, state.dpi);
    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_dim));
    let note_gop = to_wide(tr("跳步後若回彈至同一關鍵影格 (≤0.5s)，自動跨區推進之上限 (0=停用)。", "Max extra hops if seeking snaps back to the same keyframe (≤0.5s, 0=off)."));
    let _ = TextOutW(hdc, x + scale_px(32, state.dpi), note_gop_y, &note_gop);
    c1_y += scale_px(58, state.dpi);

    // 1e. 關鍵影格探測上限 (加寬說明與下行間距)
    draw_row_number_input(hdc, state, EditTarget::ConsecutiveLimit, x, c1_y, w, tr("關鍵影格探測上限", "Keyframe Probe Limit"), state.settings.consecutive_unacceptable_limit, tr("幀", "frames"), !is_custom);
    let note_consec_y = c1_y + scale_px(27, state.dpi);
    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_dim));
    let note_consec = to_wide(tr("MF 與 FFmpeg 共同套用之關鍵影格最大嘗試次數，未達標則回退最佳候選。", "Max distinct keyframe attempts for both MF and FFmpeg before fallback best."));
    let _ = TextOutW(hdc, x + scale_px(32, state.dpi), note_consec_y, &note_consec);
    c1_y += scale_px(58, state.dpi);

    // 1f. 單檔處理時間上限 (加寬說明與分隔線間距)
    draw_row_number_input(hdc, state, EditTarget::WatchdogTimeoutSec, x, c1_y, w, tr("單檔處理時間上限", "Single-File Time Budget"), state.settings.watchdog_timeout_sec, tr("秒", "s"), !is_custom);
    let note_timeout_y = c1_y + scale_px(27, state.dpi);
    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_dim));
    let note_timeout = to_wide(tr("關鍵影格探測與看門狗之單檔總時間上限，逾時立即回退最佳候選或中斷。", "Time budget for keyframe probing and extraction watchdog before fallback/abort."));
    let _ = TextOutW(hdc, x + scale_px(32, state.dpi), note_timeout_y, &note_timeout);
    c1_y += scale_px(58, state.dpi);

    // Divider Line 2
    let div2_y = c1_y + scale_px(10, state.dpi);
    draw_expander_seam(hdc, state, card_x, div2_y, card_w);

    // Section 2: Mean Luminance Filter
    let mut c2_y = div2_y + scale_px(8, state.dpi);
    draw_row_toggle(hdc, state, 11, x, c2_y, w, tr("平均亮度過濾", "Mean Luminance Filter"), state.settings.luminance_enabled, !is_custom);
    c2_y += scale_px(32, state.dpi);

    let lum_disabled = !is_custom || !state.settings.luminance_enabled;
    draw_row_slider(hdc, state, 1, x, c2_y, w, tr("亮度下限", "Lower Limit"), state.settings.luminance_lower_limit, "%", lum_disabled, true);
    c2_y += scale_px(30, state.dpi);

    draw_row_slider(hdc, state, 2, x, c2_y, w, tr("亮度上限", "Upper Limit"), state.settings.luminance_upper_limit, "%", lum_disabled, true);
    c2_y += scale_px(30, state.dpi);

    // Divider Line 3
    let div3_y = c2_y;
    draw_expander_seam(hdc, state, card_x, div3_y, card_w);

    // Section 3: Shannon Entropy Filter
    let mut c3_y = div3_y + scale_px(8, state.dpi);
    draw_row_toggle(hdc, state, 12, x, c3_y, w, tr("圖像香農熵度", "Shannon Entropy Filter"), state.settings.entropy_enabled, !is_custom);
    c3_y += scale_px(32, state.dpi);

    let entropy_disabled = !is_custom || !state.settings.entropy_enabled;
    draw_row_slider(hdc, state, 3, x, c3_y, w, tr("熵度下限", "Lower Limit"), state.settings.entropy_lower_limit, "%", entropy_disabled, true);

    let _ = SelectClipRgn(hdc, HRGN::default());
    let _ = DeleteObject(rgn);
    }
}

unsafe fn render_audio_page(hdc: HDC, state: &AppState, x: i32, w: i32) {
    let offset = (if is_media_preview_installed() { scale_px(45, state.dpi) } else { 0 }) - state.scroll_y;

    // Card 1: Checklist of Audio formats
    let card1_y = scale_px(75, state.dpi) + offset;
    let card1_h = get_checklist_card_height(MediaCategory::Audio, state.dpi);
    draw_card_container(hdc, state, x, card1_y, w, card1_h);
    render_extensions_grid(hdc, state, x, card1_y, w, MediaCategory::Audio, 200, tr("關聯的音訊副檔名", "Associated Audio Extensions"));

    // Card 2: Independent Border Style & Live Preview
    let card2_y = card1_y + card1_h + scale_px(16, state.dpi);
    let card2_h = scale_px(145, state.dpi);
    draw_card_container(hdc, state, x, card2_y, w, card2_h);
    render_border_preview_widget(
        hdc,
        state,
        x,
        card2_y,
        w,
        tr("音訊縮圖外框樣式與預覽", "Audio Border Style & Preview"),
        state.settings.border_audio,
        state.settings.badge_audio,
        tr("顯示音訊格式辨識角標 (MP3, FLAC 等)", "Show Audio Format Badge (MP3, FLAC)"),
        "MP3",
    );

    // Card 3: Cover Art options
    let card3_y = card2_y + card2_h + scale_px(16, state.dpi);
    let card3_h = scale_px(90, state.dpi);
    draw_card_container(hdc, state, x, card3_y, w, card3_h);

    let cy = card3_y + scale_px(16, state.dpi);
    draw_row_toggle(hdc, state, 20, x, cy, w, tr("優先擷取內嵌專輯封面 (ID3v2 APIC / MP4 covr / FLAC)", "Extract Embedded Cover Art First"), state.settings.cover_art_enabled, false);

    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(theme().text_muted));
    let tip = to_wide(tr("💡 若音訊無內嵌封面，CBX Shell 將自動產生帶有格式辨識的黑膠唱片圖示", "💡 If no embedded cover is found, a procedural vinyl disc card is generated automatically"));
    let _ = TextOutW(hdc, x + scale_px(16, state.dpi), cy + scale_px(38, state.dpi), &tip);
}

unsafe fn render_other_page(hdc: HDC, state: &AppState, x: i32, w: i32) {
    let offset = (if is_media_preview_installed() { scale_px(45, state.dpi) } else { 0 }) - state.scroll_y;

    // Card 1: Checklist of Document & Image formats (Word, Excel, PSD, TXT, MD)
    let card1_y = scale_px(75, state.dpi) + offset;
    let card1_h = get_checklist_card_height(MediaCategory::Other, state.dpi);
    draw_card_container(hdc, state, x, card1_y, w, card1_h);
    render_extensions_grid(
        hdc,
        state,
        x,
        card1_y,
        w,
        MediaCategory::Other,
        200,
        tr(
            "關聯的文件、專業圖像與純文字副檔名",
            "Associated Document, PSD & Plain-Text Extensions",
        ),
    );

    // Card 2: Independent Syntax & Code Extensions (Default Off)
    let card2_y = card1_y + card1_h + scale_px(14, state.dpi);
    let card2_h = get_checklist_card_height(MediaCategory::Syntax, state.dpi);
    draw_card_container(hdc, state, x, card2_y, w, card2_h);
    render_extensions_grid(
        hdc,
        state,
        x,
        card2_y,
        w,
        MediaCategory::Syntax,
        203,
        tr(
            "常見語法與純文字副檔名",
            "Common Syntax & Plain-Text Extensions",
        ),
    );

    // Card 3: Custom Plain Text Extensions
    let card3_y = card2_y + card2_h + scale_px(14, state.dpi);
    let card3_h = scale_px(108, state.dpi);
    draw_card_container(hdc, state, x, card3_y, w, card3_h);

    let th = theme();
    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_primary));
    let t_txt = to_wide(tr(
        "自訂純文字縮圖關聯副檔名",
        "Custom Plain-Text Thumbnail Extensions",
    ));
    let _ = TextOutW(hdc, x + scale_px(16, state.dpi), card3_y + scale_px(14, state.dpi), &t_txt);

    let box_x = x + scale_px(16, state.dpi);
    let box_y = card3_y + scale_px(42, state.dpi);
    let btn_w = scale_px(96, state.dpi);
    let box_w = (w - scale_px(44, state.dpi) - btn_w).max(scale_px(180, state.dpi));
    let box_h = scale_px(30, state.dpi);
    let btn_x = box_x + box_w + scale_px(12, state.dpi);

    let is_editing = state.active_edit == Some(EditTarget::CustomTextExtensions);
    let (mx, my) = state.mouse_pos;
    let is_hovered = mx >= box_x && mx <= box_x + box_w && my >= box_y && my <= box_y + box_h;
    draw_win11_textbox_frame(hdc, state, box_x, box_y, box_w, box_h, is_editing, is_hovered, false);
    let show_clear = is_editing && !state.edit_buffer.is_empty();

    let display_val = if is_editing {
        state.edit_buffer.as_str()
    } else {
        state.settings.custom_text_extensions.as_str()
    };
    SelectObject(hdc, state.font_regular);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_primary));
    let txt_right = box_x + box_w - if show_clear { textbox_clear_w(state) + scale_px(4, state.dpi) } else { scale_px(10, state.dpi) };
    let txt_left = box_x + scale_px(10, state.dpi);
    state.edit_boxes.borrow_mut().push(EditBoxGeom {
        target: EditTarget::CustomTextExtensions,
        rect: RECT { left: box_x, top: box_y, right: box_x + box_w, bottom: box_y + box_h },
        text_left: txt_left,
        text_right: txt_right,
        font: state.font_regular,
    });
    draw_edit_text(hdc, state, state.font_regular, display_val, th.text_primary, txt_left, txt_right, box_y, box_h, is_editing);
    if show_clear {
        draw_textbox_clear_button(hdc, state, box_x, box_y, box_w, box_h);
    }

    let is_applied = state.applied_custom_timer.map_or(false, |t| t.elapsed().as_secs_f32() < 1.5);
    let apply_txt = if is_applied {
        tr("✓ 已套用", "✓ Applied")
    } else {
        tr("🛡️ 套用", "🛡️ Apply")
    };
    draw_rounded_button(hdc, state, 345, btn_x, box_y, btn_w, box_h, apply_txt, false, false);

    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_dim));
    let mut sub_rc = RECT {
        left: box_x,
        top: box_y + box_h + scale_px(8, state.dpi),
        right: x + w - scale_px(16, state.dpi),
        bottom: box_y + box_h + scale_px(28, state.dpi),
    };
    let mut sh_w = to_wide(tr(
        ".txt 與 .md 由上方勾選項目控制；此處可額外輸入逗號分隔的副檔名（例如: nfo, cfg），按下 Enter 或點擊套用",
        ".txt & .md are controlled above; enter extra comma-separated extensions (e.g. nfo, cfg) and click Apply.",
    ));
    let _ = DrawTextW(hdc, &mut sh_w, &mut sub_rc, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);

    // Card 4: Independent Preview Sub-Cards for Thumbnail Frames
    let card4_y = card3_y + card3_h + scale_px(14, state.dpi);
    let card4_h = scale_px(254, state.dpi);
    draw_card_container(hdc, state, x, card4_y, w, card4_h);

    let th = theme();
    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_primary));
    let t_frames = to_wide(tr("縮圖外框", "Thumbnail Frames"));
    let _ = TextOutW(hdc, x + scale_px(16, state.dpi), card4_y + scale_px(14, state.dpi), &t_frames);

    let pill_y0 = card4_y + scale_px(44, state.dpi);
    let pill_h = scale_px(92, state.dpi);
    let pill_gap = scale_px(10, state.dpi);
    let pill_w = ((w - scale_px(32, state.dpi) - pill_gap * 2) / 3).max(scale_px(130, state.dpi));
    let app_styles = [
        (0x00BD5A18u32, 0x00EE8828u32, 0x00FBFDFAu32, "Word", ".docx / .doc"),
        (0x00417C10u32, 0x0066AA33u32, 0x00FCFDFCu32, "Excel", ".xlsx / .xls"),
        (0x003E2208u32, 0x00F8A838u32, 0x00302218u32, "Photoshop", ".psd"),
        (0x0058524Cu32, 0x00B8B0A8u32, 0x00FCFBFBu32, tr("純文字", "Plain Text"), ".txt / .md"),
        (0x003C2A20u32, 0x00F59442u32, 0x0024201Eu32, "BitTorrent", ".torrent"),
    ];
    for (idx, (frame_col, accent_col, sheet_col, title_s, ext_s)) in app_styles.iter().enumerate() {
        let col = (idx % 3) as i32;
        let row = (idx / 3) as i32;
        let px = box_x + col * (pill_w + pill_gap);
        let pill_y = pill_y0 + row * (pill_h + pill_gap);
        draw_sub_card(hdc, state, px, pill_y, pill_w, pill_h);

        // Miniature visual frame preview on the left side of sub-card (no DOCX/XLSX text)
        let prev_w = scale_px(50, state.dpi);
        let prev_h = scale_px(64, state.dpi);
        let prev_x = px + scale_px(10, state.dpi);
        let prev_y = pill_y + (pill_h - prev_h) / 2;
        let r_prev = scale_px(7, state.dpi);

        {
            let saved = windows::Win32::Graphics::Gdi::SaveDC(hdc);
            let rgn = CreateRoundRectRgn(prev_x, prev_y, prev_x + prev_w + 1, prev_y + prev_h + 1, r_prev, r_prev);
            let _ = windows::Win32::Graphics::Gdi::ExtSelectClipRgn(hdc, rgn, windows::Win32::Graphics::Gdi::RGN_AND);

            let fr_br = CreateSolidBrush(windows::Win32::Foundation::COLORREF(*frame_col));
            let fr_rc = RECT { left: prev_x, top: prev_y, right: prev_x + prev_w, bottom: prev_y + prev_h };
            FillRect(hdc, &fr_rc, fr_br);
            let _ = DeleteObject(fr_br);

            // Top & bottom accent stripes
            let ac_br = CreateSolidBrush(windows::Win32::Foundation::COLORREF(*accent_col));
            let top_st = RECT { left: prev_x, top: prev_y, right: prev_x + prev_w, bottom: prev_y + 2 };
            let bot_st = RECT { left: prev_x, top: prev_y + prev_h - 2, right: prev_x + prev_w, bottom: prev_y + prev_h };
            FillRect(hdc, &top_st, ac_br);
            FillRect(hdc, &bot_st, ac_br);
            let _ = DeleteObject(ac_br);

            // Symmetrical inner rounded sheet/content area matching actual thumbnail output
            let pad_x_f = scale_px(4, state.dpi);
            let pad_y_f = scale_px(5, state.dpi);
            let inner_rc = RECT {
                left: prev_x + pad_x_f,
                top: prev_y + pad_y_f,
                right: prev_x + prev_w - pad_x_f,
                bottom: prev_y + prev_h - pad_y_f,
            };
            let r_in = scale_px(4, state.dpi);
            let sh_br = CreateSolidBrush(windows::Win32::Foundation::COLORREF(*sheet_col));
            let sh_pn = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(*sheet_col));
            let osb = SelectObject(hdc, sh_br);
            let osp = SelectObject(hdc, sh_pn);
            let _ = RoundRect(hdc, inner_rc.left, inner_rc.top, inner_rc.right, inner_rc.bottom, r_in, r_in);
            SelectObject(hdc, osb);
            SelectObject(hdc, osp);
            let _ = DeleteObject(sh_br);
            let _ = DeleteObject(sh_pn);

            // Minimal mock lines/grid inside inner preview
            let ln_col = if idx == 2 { 0x00786050 } else { 0x00C8C8CE };
            let ln_br = CreateSolidBrush(windows::Win32::Foundation::COLORREF(ln_col));
            if idx == 1 {
                // Excel mini table rows
                for r in 0..3 {
                    let ry = inner_rc.top + scale_px(6 + r * 11, state.dpi);
                    let rr = RECT {
                        left: inner_rc.left + scale_px(4, state.dpi),
                        top: ry,
                        right: inner_rc.right - scale_px(4, state.dpi),
                        bottom: ry + scale_px(5, state.dpi),
                    };
                    FillRect(hdc, &rr, ln_br);
                }
            } else if idx == 2 {
                // PSD mini artwork card inside dark studio frame
                let art_br = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00906850));
                let art_rc = RECT {
                    left: inner_rc.left + scale_px(5, state.dpi),
                    top: inner_rc.top + scale_px(5, state.dpi),
                    right: inner_rc.right - scale_px(5, state.dpi),
                    bottom: inner_rc.bottom - scale_px(5, state.dpi),
                };
                FillRect(hdc, &art_rc, art_br);
                let _ = DeleteObject(art_br);
            } else if idx == 3 {
                // Plain Text: colored syntax-highlight token bars
                let seg_h = scale_px(3, state.dpi);
                let pad_i = scale_px(4, state.dpi);
                let token_rows: [&[(i32, i32, u32)]; 4] = [
                    &[(0, 12, 0x00A55104), (14, 24, 0x001515A3)],
                    &[(0, 9, 0x0055007F), (11, 20, 0x00588609), (22, 30, 0x0076685C)],
                    &[(2, 13, 0x00A55104), (15, 28, 0x001515A3)],
                    &[(0, 16, 0x00308010), (18, 25, 0x00907000)],
                ];
                for (r, segs) in token_rows.iter().enumerate() {
                    let ry = inner_rc.top + scale_px(6 + (r as i32) * 8, state.dpi);
                    let max_r = inner_rc.right - pad_i;
                    for &(sx, ex, col_bgr) in *segs {
                        let sl = inner_rc.left + pad_i + scale_px(sx, state.dpi);
                        let sr = (inner_rc.left + pad_i + scale_px(ex, state.dpi)).min(max_r);
                        if sl < sr {
                            let tb = CreateSolidBrush(windows::Win32::Foundation::COLORREF(col_bgr));
                            let rr = RECT { left: sl, top: ry, right: sr, bottom: ry + seg_h };
                            FillRect(hdc, &rr, tb);
                            let _ = DeleteObject(tb);
                        }
                    }
                }
            } else if idx == 4 {
                // BitTorrent (.torrent): dark sheet with folder/file mini-icons and filename bars
                let icon_sz = scale_px(5, state.dpi);
                let pad_i = scale_px(4, state.dpi);
                let tor_rows: [(i32, u32, i32); 4] = [
                    (0, 0x0048BEF5, 22), // folder icon (amber)
                    (1, 0x00EB6E9B, 20), // video file icon (purple)
                    (1, 0x009098A4, 16), // subtitle/text icon
                    (0, 0x00E8963C, 18), // archive/nfo icon
                ];
                for (r, &(indent, icon_col, bar_len)) in tor_rows.iter().enumerate() {
                    let ry = inner_rc.top + scale_px(6 + (r as i32) * 10, state.dpi);
                    let ix = inner_rc.left + pad_i + indent * scale_px(5, state.dpi);
                    let ib = CreateSolidBrush(windows::Win32::Foundation::COLORREF(icon_col));
                    let irc = RECT { left: ix, top: ry, right: ix + icon_sz, bottom: ry + icon_sz };
                    FillRect(hdc, &irc, ib);
                    let _ = DeleteObject(ib);

                    let bx0 = ix + icon_sz + scale_px(3, state.dpi);
                    let bx1 = (bx0 + scale_px(bar_len, state.dpi)).min(inner_rc.right - pad_i);
                    if bx0 < bx1 {
                        let tb = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00D8D4CE));
                        let trc = RECT { left: bx0, top: ry + scale_px(1, state.dpi), right: bx1, bottom: ry + scale_px(4, state.dpi) };
                        FillRect(hdc, &trc, tb);
                        let _ = DeleteObject(tb);
                    }
                }
            } else {
                // Document horizontal lines
                for r in 0..4 {
                    let ry = inner_rc.top + scale_px(6 + r * 8, state.dpi);
                    let rr = RECT {
                        left: inner_rc.left + scale_px(4, state.dpi),
                        top: ry,
                        right: inner_rc.right - scale_px(4 + (r % 2) * 8, state.dpi),
                        bottom: ry + scale_px(3, state.dpi),
                    };
                    FillRect(hdc, &rr, ln_br);
                }
            }
            let _ = DeleteObject(ln_br);

            let _ = windows::Win32::Graphics::Gdi::RestoreDC(hdc, saved);
            let _ = DeleteObject(rgn);
        }

        // Plain Text (idx == 3) & BitTorrent (idx == 4): cut top-right corner & draw folded triangle dog-ear flap
        if idx == 3 || idx == 4 {
            let fold = scale_px(13, state.dpi);
            let fold_x = prev_x + prev_w - fold;
            let fold_y = prev_y + fold;

            // Erase cut-away top-right triangle back to sub-card background (#17171B BGR)
            let cut_pts = [
                POINT { x: fold_x, y: prev_y - 1 },
                POINT { x: prev_x + prev_w + 1, y: prev_y - 1 },
                POINT { x: prev_x + prev_w + 1, y: fold_y },
            ];
            let sub_bg = theme().sub_card_bg;
            let cut_br = CreateSolidBrush(windows::Win32::Foundation::COLORREF(sub_bg));
            let cut_pn = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(sub_bg));
            let ocb = SelectObject(hdc, cut_br);
            let ocp = SelectObject(hdc, cut_pn);
            let _ = Polygon(hdc, &cut_pts);
            SelectObject(hdc, ocb);
            SelectObject(hdc, ocp);
            let _ = DeleteObject(cut_br);
            let _ = DeleteObject(cut_pn);

            // Soft cast shadow immediately below the folded triangle flap
            let sh_br = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00B4B0AC));
            let sh_rc = RECT {
                left: fold_x - scale_px(1, state.dpi),
                top: fold_y,
                right: prev_x + prev_w - scale_px(3, state.dpi),
                bottom: fold_y + scale_px(3, state.dpi),
            };
            FillRect(hdc, &sh_rc, sh_br);
            let _ = DeleteObject(sh_br);

            // Folded-over triangular flap with crisp edge
            let flap_pts = [
                POINT { x: fold_x, y: prev_y },
                POINT { x: fold_x, y: fold_y },
                POINT { x: prev_x + prev_w, y: fold_y },
            ];
            let fl_br = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00DCDAD6));
            let fl_pn = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(0x00908C88));
            let ofb = SelectObject(hdc, fl_br);
            let ofp = SelectObject(hdc, fl_pn);
            let _ = Polygon(hdc, &flap_pts);
            SelectObject(hdc, ofb);
            SelectObject(hdc, ofp);
            let _ = DeleteObject(fl_br);
            let _ = DeleteObject(fl_pn);
        }

        // Title and extensions to the right of miniature preview
        let th = theme();
        let text_left = prev_x + prev_w + scale_px(10, state.dpi);
        SelectObject(hdc, state.font_bold);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_primary));
        let mut t_rc = RECT {
            left: text_left,
            top: pill_y + scale_px(22, state.dpi),
            right: px + pill_w - scale_px(8, state.dpi),
            bottom: pill_y + scale_px(44, state.dpi),
        };
        let mut tw = to_wide(title_s);
        let _ = DrawTextW(hdc, &mut tw, &mut t_rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);

        SelectObject(hdc, state.font_small);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_muted));
        let mut s_rc = RECT {
            left: text_left,
            top: pill_y + scale_px(46, state.dpi),
            right: px + pill_w - scale_px(8, state.dpi),
            bottom: pill_y + scale_px(66, state.dpi),
        };
        let mut sw = to_wide(ext_s);
        let _ = DrawTextW(hdc, &mut sw, &mut s_rc, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
    }
}

unsafe fn render_advanced_page(hdc: HDC, state: &AppState, x: i32, w: i32) {
    let offset = (if is_media_preview_installed() { scale_px(45, state.dpi) } else { 0 }) - state.scroll_y;

    // Card 1: System Repair & Cache Management
    let card1_y = scale_px(75, state.dpi) + offset;
    let card1_h = scale_px(125, state.dpi);
    draw_card_container(hdc, state, x, card1_y, w, card1_h);

    let th = theme();
    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_primary));
    let t_w = to_wide(tr("系統修復與快取管理", "System Repair & Cache"));
    let _ = TextOutW(hdc, x + scale_px(16, state.dpi), card1_y + scale_px(14, state.dpi), &t_w);

    let btn1_x = x + scale_px(16, state.dpi);
    let btn_y = card1_y + scale_px(44, state.dpi);
    let btn_w = scale_px(140, state.dpi);
    let btn_gap = scale_px(12, state.dpi);
    let btn_h = scale_px(32, state.dpi);

    draw_rounded_button(hdc, state, 300, btn1_x, btn_y, btn_w, btn_h, tr("清理縮圖快取", "Clear Cache"), false, false);

    let btn2_x = btn1_x + btn_w + btn_gap;
    let fill_label = if state.prewarm.is_active() { tr("■ 停止產生", "■ Stop") } else { tr("📁 預先產生縮圖", "📁 Pre-generate") };
    draw_rounded_button(hdc, state, 301, btn2_x, btn_y, btn_w, btn_h, fill_label, false, false);

    // Title-row extras: drop hint and the pre-generation size selector.
    let (size_x, size_y, size_w, size_h) = prewarm_size_button_geom(state, x, w, card1_y);
    let size_label = format!("{} {} px", tr("縮圖尺寸", "Size"), state.prewarm_size);
    draw_subtle_button(hdc, state, 303, size_x, size_y, size_w, size_h, &size_label, state.prewarm.is_active());
    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_secondary));
    let mut hint_rc = RECT {
        left: x + scale_px(190, state.dpi),
        top: card1_y + scale_px(16, state.dpi),
        right: size_x - scale_px(8, state.dpi),
        bottom: card1_y + scale_px(36, state.dpi),
    };
    let mut hint_w = to_wide(tr(
        "將資料夾拖曳到此頁，可多執行緒預先產生縮圖（含 3 層子資料夾）",
        "Drop a folder on this page to pre-generate thumbnails in parallel (3 sub-folder levels)",
    ));
    let _ = DrawTextW(hdc, &mut hint_w, &mut hint_rc, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);

    let btn3_x = btn2_x + btn_w + btn_gap;
    let btn3_w = scale_px(195, state.dpi);
    draw_rounded_button(hdc, state, 302, btn3_x, btn_y, btn3_w, btn_h, tr("🛡️ 修復所有副檔名關聯", "🛡️ Repair Associations"), false, false);

    // Checkboxes row: [✓] 包含網路設備   [✓] 強制更新
    let chk_y = btn_y + btn_h + scale_px(10, state.dpi);
    let chk1_x = btn1_x;
    let chk2_x = chk1_x + scale_px(170, state.dpi);

    draw_checkbox(hdc, state, 350, chk1_x, chk_y, tr("包含網路設備", "Include Network Devices"), state.cache_include_network);
    draw_checkbox(hdc, state, 351, chk2_x, chk_y, tr("強制更新", "Force Refresh"), state.cache_force_refresh);

    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.status_ok));
    let status_str = if state.prewarm_shown { prewarm_status_text(state) } else { state.get_cache_status_display() };
    let mut status_w = to_wide(&status_str);
    let mut status_rc = RECT {
        left: chk2_x + scale_px(160, state.dpi),
        top: chk_y + scale_px(2, state.dpi),
        right: x + w - scale_px(16, state.dpi),
        bottom: chk_y + scale_px(22, state.dpi),
    };
    let _ = DrawTextW(hdc, &mut status_w, &mut status_rc, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);

    // Card 2: Live Activity Monitor (即時縮圖運作監視器)
    let card2_y = card1_y + card1_h + scale_px(14, state.dpi);
    let header_h = scale_px(44, state.dpi);
    let user_card_h = state.monitor_card_h;
    let card2_h = header_h + (((user_card_h - header_h) as f32) * state.monitor_expand_anim) as i32;
    draw_card_container(hdc, state, x, card2_y, w, card2_h);

    let th = theme();
    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_primary));
    let mon_title = to_wide(tr("即時運作監視器", "Live Activity Monitor"));
    let _ = TextOutW(hdc, x + scale_px(16, state.dpi), card2_y + scale_px(12, state.dpi), &mon_title);

    let mon_enabled = state.settings.monitor_enabled;

    // "記錄運作日誌" Toggle Switch (ID 99) on the right of header row
    let sw_card_w = scale_px(210, state.dpi);
    let sw_x = x + w - sw_card_w;
    draw_row_toggle(hdc, state, 99, sw_x, card2_y + scale_px(8, state.dpi), sw_card_w, tr("記錄運作日誌", "Enable Logging"), mon_enabled, false);
    let (lock_x, lock_w) = monitor_lock_button_geom(state, sw_x);
    let lock_label = monitor_lock_label(&state.settings);
    draw_subtle_button(hdc, state, 97, lock_x, card2_y + scale_px(8, state.dpi), lock_w, scale_px(24, state.dpi), &lock_label, !mon_enabled);

    // Only render log contents if expanded
    if state.monitor_expand_anim > 0.02 {
        // Card-style seam between the header row and the log, like the expander cards.
        draw_expander_seam(hdc, state, x, card2_y + header_h, w);
        let rgn = CreateRectRgn(x, card2_y + header_h, x + w, card2_y + card2_h);
        let _ = SelectClipRgn(hdc, rgn);
        render_live_monitor_widget(hdc, state, x, card2_y, w, header_h, card2_h);
        let _ = SelectClipRgn(hdc, None);
        let _ = DeleteObject(rgn);

        // Card 2 bottom resizer bar (only visible when fully or mostly expanded)
        if state.monitor_expand_anim > 0.85 && mon_enabled {
            let resizer_y = card2_y + card2_h - scale_px(7, state.dpi);
            let resizer_w = scale_px(36, state.dpi);
            let resizer_x = x + (w - resizer_w) / 2;
            let r_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(th.scrollbar_thumb));
            let old_rb = SelectObject(hdc, r_brush);
            let _ = RoundRect(hdc, resizer_x, resizer_y, resizer_x + resizer_w, resizer_y + scale_px(3, state.dpi), 2, 2);
            SelectObject(hdc, old_rb);
            let _ = DeleteObject(r_brush);
        }
    }

    // Card 3: Drag & Drop Experiment Area
    let drop_y = card2_y + card2_h + scale_px(14, state.dpi);

    // Lay out the result text first (word-wrapped to the left of the preview box) so the card can
    // grow to fit long messages such as decoder errors instead of cutting them off.
    let prev_w = scale_px(140, state.dpi);
    let prev_h = scale_px(140, state.dpi);
    let prev_x = x + w - prev_w - scale_px(16, state.dpi);
    let text_left = x + scale_px(16, state.dpi);
    let text_right = (prev_x - scale_px(12, state.dpi)).max(text_left + scale_px(80, state.dpi));
    let line_gap = scale_px(1, state.dpi);
    let min_line_h = scale_px(18, state.dpi);
    let wrap_flags = DT_LEFT | DT_WORDBREAK | DT_EDITCONTROL | DT_NOPREFIX;
    let metrics_text = state.get_test_metrics_display();
    SelectObject(hdc, state.font_regular);
    let mut line_layout: Vec<(Vec<u16>, i32)> = Vec::new();
    let mut text_bottom = drop_y + scale_px(52, state.dpi);
    for line in metrics_text.lines() {
        let mut lw = to_wide(line);
        let mut calc = RECT { left: text_left, top: 0, right: text_right, bottom: 0 };
        let _ = DrawTextW(hdc, &mut lw, &mut calc, wrap_flags | DT_CALCRECT);
        let lh = (calc.bottom - calc.top).max(min_line_h);
        line_layout.push((lw, lh));
        text_bottom += lh + line_gap;
    }
    let drop_h = (text_bottom - drop_y + scale_px(14, state.dpi)).max(scale_px(175, state.dpi));
    state.drop_card_h.set(drop_h);
    draw_card_container(hdc, state, x, drop_y, w, drop_h);

    let th = theme();
    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_primary));
    let test_t = to_wide(tr("拖放檔案實驗區", "Drag & Drop Experiment Area"));
    let _ = TextOutW(hdc, x + scale_px(16, state.dpi), drop_y + scale_px(12, state.dpi), &test_t);

    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_muted));
    let hint_w = to_wide(tr("將檔案拖放到此視窗以執行探測並顯示各項指標與預覽圖", "Drag and drop a file here to probe and display metrics and thumbnail preview"));
    let _ = TextOutW(hdc, x + scale_px(16, state.dpi), drop_y + scale_px(32, state.dpi), &hint_w);

    // Right Side: Live Thumbnail Preview Box
    let prev_y = drop_y + scale_px(18, state.dpi);

    // "Update this file's thumbnail" button (ID 309) to the left of preview box
    let upd_w = scale_px(136, state.dpi);
    let upd_h = scale_px(24, state.dpi);
    let upd_x = prev_x - upd_w - scale_px(12, state.dpi);
    let upd_y = drop_y + scale_px(10, state.dpi);
    draw_rounded_button(
        hdc,
        state,
        309,
        upd_x,
        upd_y,
        upd_w,
        upd_h,
        tr("🔄 更新此檔案縮圖", "Refresh File Thumb"),
        false,
        state.test_path.is_none(),
    );

    let pb_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(th.sub_card_bg));
    let pb_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(th.sub_card_border));
    let old_b = SelectObject(hdc, pb_brush);
    let old_p = SelectObject(hdc, pb_pen);
    let _ = RoundRect(hdc, prev_x, prev_y, prev_x + prev_w, prev_y + prev_h, 8, 8);
    SelectObject(hdc, old_b);
    SelectObject(hdc, old_p);
    let _ = DeleteObject(pb_brush);
    let _ = DeleteObject(pb_pen);

    if let Some(hbmp) = state.test_thumb {
        let (tw, th) = state.test_thumb_dims;
        if tw > 0 && th > 0 {
            let pad = 6;
            let avail_w = prev_w - pad * 2;
            let avail_h = prev_h - pad * 2;
            let scale = (avail_w as f32 / tw as f32).min(avail_h as f32 / th as f32).min(1.0);
            let draw_w = (tw as f32 * scale).round() as i32;
            let draw_h = (th as f32 * scale).round() as i32;
            let draw_x = prev_x + (prev_w - draw_w) / 2;
            let draw_y = prev_y + (prev_h - draw_h) / 2;

            let mem_dc = CreateCompatibleDC(hdc);
            let old_bmp = SelectObject(mem_dc, hbmp);
            SetStretchBltMode(hdc, HALFTONE);
            let _ = StretchBlt(hdc, draw_x, draw_y, draw_w, draw_h, mem_dc, 0, 0, tw as i32, th as i32, SRCCOPY);
            SelectObject(mem_dc, old_bmp);
            let _ = DeleteDC(mem_dc);
        }
    } else {
        SelectObject(hdc, state.font_small);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_dim));
        let no_thumb = to_wide(tr("縮圖預覽窗格", "Thumbnail Preview"));
        let _ = TextOutW(hdc, prev_x + scale_px(35, state.dpi), prev_y + scale_px(62, state.dpi), &no_thumb);
    }

    // Left Side: Metrics text (constrained to left of preview box)
    if !metrics_text.is_empty() {
        SelectObject(hdc, state.font_regular);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(accent_light_bgr()));
        let mut ly = drop_y + scale_px(52, state.dpi);
        for (mut lw, lh) in line_layout {
            let mut lr = RECT { left: text_left, top: ly, right: text_right, bottom: ly + lh };
            let _ = DrawTextW(hdc, &mut lw, &mut lr, wrap_flags);
            ly += lh + line_gap;
        }
    } else {
        SelectObject(hdc, state.font_regular);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_dim));
        let wait_w = to_wide(tr("〔等待拖入檔案中...〕", "[ Waiting for dragged file... ]"));
        let _ = TextOutW(hdc, x + scale_px(16, state.dpi), drop_y + scale_px(75, state.dpi), &wait_w);
    }
}

fn format_monitor_row_details(item: &cbx_core::monitor::SnapshotHistoryItem, _lang: UiLang) -> (String, u32) {
    let th = theme();
    let text = normalize_details_display(&item.details);
    if is_details_no_cover(&item.details) {
        return (format!("✓ {}", text), th.text_muted);
    }
    if item.details.contains("解碼失敗")
        || item.details.contains("解碼錯誤")
        || item.details.contains("decode failed")
    {
        return (format!("✗ {}", text), th.status_err);
    }
    if is_details_timeout_or_aborted(&item.details) {
        return (format!("⚠️ {}", text), th.status_warn);
    }
    if item.success {
        (format!("✓ {}", text), th.text_muted)
    } else {
        (format!("✗ {}", text), th.status_err)
    }
}

unsafe fn render_live_monitor_widget(
    hdc: HDC,
    state: &AppState,
    card_x: i32,
    card_y: i32,
    card_w: i32,
    header_h: i32,
    card_h: i32,
) {
    let pad_x = scale_px(16, state.dpi);
    let top_y = card_y + header_h + scale_px(4, state.dpi);

    let (active_tasks, total_proc, avg_dur, cur_file, cur_stage, cur_elapsed, history) =
        if let Some(ref snap) = state.monitor_snapshot {
            (
                snap.active_tasks,
                snap.total_processed,
                snap.avg_duration_ms,
                snap.current_file.clone(),
                snap.current_stage.clone(),
                snap.current_elapsed_ms,
                snap.history.clone(),
            )
        } else {
            (0, 0, 0, String::new(), String::new(), 0, Vec::new())
        };

    let is_busy = active_tasks > 0;
    let banner_h = scale_px(38, state.dpi);
    let banner_w = card_w - pad_x * 2;

    let th = theme();
    // Permanent Status Banner: Always visible to prevent UI jitter and flickering
    let (bg_color, bdr_color) = if is_busy {
        if cur_elapsed > 5000 {
            (th.caution_bg, th.caution_border)
        } else {
            (accent_subtle_bg_bgr(), accent_base_bgr())
        }
    } else if th.dark {
        (0x001B2019, 0x0033482C) // Calm subtle dark-green
    } else {
        (0x00E6F4EA, 0x0081C995) // Calm subtle light-green
    };

    let bg_b = CreateSolidBrush(windows::Win32::Foundation::COLORREF(bg_color));
    let pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(bdr_color));
    let old_b = SelectObject(hdc, bg_b);
    let old_p = SelectObject(hdc, pen);
    let _ = RoundRect(
        hdc,
        card_x + pad_x,
        top_y,
        card_x + pad_x + banner_w,
        top_y + banner_h,
        6,
        6,
    );
    SelectObject(hdc, old_b);
    SelectObject(hdc, old_p);
    let _ = DeleteObject(bg_b);
    let _ = DeleteObject(pen);

    if is_busy {
        let warn_prefix = if cur_elapsed > 5000 {
            format!("⚠️ {} ({:.1}s) - ", tr("處理耗時較長", "Taking longer"), cur_elapsed as f32 / 1000.0)
        } else {
            format!("🔵 {} ({}) - ", tr("處理中", "Processing"), active_tasks)
        };
        let line1 = format!("{}{}: {}", warn_prefix, tr("檔案", "File"), cur_file);
        let line2 = format!("{}: {} ({} {} ms)", tr("階段", "Stage"), cur_stage, tr("已進行", "Elapsed"), cur_elapsed);

        SelectObject(hdc, state.font_bold);
        SetTextColor(hdc, if cur_elapsed > 5000 { windows::Win32::Foundation::COLORREF(th.caution_text) } else { windows::Win32::Foundation::COLORREF(th.text_bright) });
        let mut r1 = RECT {
            left: card_x + pad_x + scale_px(10, state.dpi),
            top: top_y + scale_px(4, state.dpi),
            right: card_x + pad_x + banner_w - scale_px(10, state.dpi),
            bottom: top_y + scale_px(20, state.dpi),
        };
        let mut l1_w = to_wide(&line1);
        let _ = DrawTextW(hdc, &mut l1_w, &mut r1, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);

        SelectObject(hdc, state.font_small);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_secondary));
        let mut r2 = RECT {
            left: card_x + pad_x + scale_px(10, state.dpi),
            top: top_y + scale_px(21, state.dpi),
            right: card_x + pad_x + banner_w - scale_px(10, state.dpi),
            bottom: top_y + scale_px(37, state.dpi),
        };
        let mut l2_w = to_wide(&line2);
        let _ = DrawTextW(hdc, &mut l2_w, &mut r2, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
    } else {
        // Permanent Idle "System Ready" display
        SelectObject(hdc, state.font_bold);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.status_ok));
        let ready_title = tr("🟢 系統就緒", "🟢 System Ready");
        let mut r1 = RECT {
            left: card_x + pad_x + scale_px(10, state.dpi),
            top: top_y + scale_px(4, state.dpi),
            right: card_x + pad_x + banner_w - scale_px(10, state.dpi),
            bottom: top_y + scale_px(20, state.dpi),
        };
        let mut l1_w = to_wide(ready_title);
        let _ = DrawTextW(hdc, &mut l1_w, &mut r1, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);

        SelectObject(hdc, state.font_small);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_muted));
        let ready_sub = tr("等待檔案總管發出縮圖擷取請求...", "Idle - Waiting for shell thumbnail requests...");
        let mut r2 = RECT {
            left: card_x + pad_x + scale_px(10, state.dpi),
            top: top_y + scale_px(21, state.dpi),
            right: card_x + pad_x + banner_w - scale_px(10, state.dpi),
            bottom: top_y + scale_px(37, state.dpi),
        };
        let mut l2_w = to_wide(ready_sub);
        let _ = DrawTextW(hdc, &mut l2_w, &mut r2, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
    }

    // Mini Metrics strip + Filter Buttons + Clear Log Button (Position is strictly invariant to avoid flickering)
    let metrics_y = top_y + banner_h + scale_px(8, state.dpi);
    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(theme().text_muted));

    let filtered_history: Vec<_> = history
        .iter()
        .filter(|item| monitor_filter_matches(item, state.monitor_filter, state.monitor_pipeline_filter))
        .cloned()
        .collect();

    let history_len = filtered_history.len();
    let row_h = scale_px(23, state.dpi);
    let rows_start_y = metrics_y + scale_px(40, state.dpi);
    let bottom_limit = card_y + card_h - scale_px(18, state.dpi);
    let visible_rows = (((bottom_limit - rows_start_y).max(0) / row_h).max(1) as usize).min(30);

    let scroll_offset = state.monitor_scroll.min(history_len.saturating_sub(1));
    let showing_end = (scroll_offset + visible_rows).min(history_len);
    let scroll_hint = if history_len > visible_rows {
        if matches!(current_lang(), UiLang::En) {
            format!(" ({}-{} / {})", scroll_offset + 1, showing_end, history_len)
        } else {
            format!(" (第 {}~{} 筆 / 共 {} 筆)", scroll_offset + 1, showing_end, history_len)
        }
    } else {
        String::new()
    };

    let metrics_str = format!(
        "{}: {} | {}: {} ms | {}: {}{}",
        tr("已處理", "Processed"), total_proc,
        tr("平均耗時", "Avg Latency"), avg_dur,
        tr("進行中", "Active"), active_tasks,
        scroll_hint
    );
    let mw = to_wide(&metrics_str);
    let _ = TextOutW(hdc, card_x + pad_x, metrics_y + scale_px(2, state.dpi), &mw);

    let mon_enabled = state.settings.monitor_enabled;

    // "Clear Log" button in the log metrics bar
    let clr_w = scale_px(85, state.dpi);
    let clr_h = scale_px(20, state.dpi);
    let clr_x = card_x + card_w - pad_x - clr_w;
    draw_subtle_button(hdc, state, 304, clr_x, metrics_y, clr_w, clr_h, tr("🗑 清除記錄", "🗑 Clear Log"), !mon_enabled);
    let exp_w = scale_px(85, state.dpi);
    let exp_x = clr_x - scale_px(6, state.dpi) - exp_w;
    draw_subtle_button(hdc, state, 306, exp_x, metrics_y, exp_w, clr_h, tr("📄 匯出記錄", "📄 Export"), !mon_enabled);

    // 2 Dropdown Filter Buttons on left of the log actions (matching language selector style)
    let dd_h = scale_px(20, state.dpi);
    let dd_gap = scale_px(6, state.dpi);
    let status_dd_w = scale_px(98, state.dpi);
    let pipe_dd_w = scale_px(104, state.dpi);
    let status_dd_x = exp_x - scale_px(17, state.dpi) - status_dd_w;
    let pipe_dd_x = status_dd_x - dd_gap - pipe_dd_w;

    state.monitor_dropdown_anchor.set((pipe_dd_x, status_dd_x, metrics_y, dd_h));

    let pipe_label = match state.monitor_pipeline_filter {
        MonitorPipelineFilter::All => tr("全部管道", "All Pipelines"),
        MonitorPipelineFilter::DirectFile => tr("檔案直讀", "Direct File"),
        MonitorPipelineFilter::IpcHit => tr("IPC 命中", "IPC Hit"),
        MonitorPipelineFilter::SearchHit => tr("搜尋命中", "Search Hit"),
        MonitorPipelineFilter::StreamRpc => tr("串流 RPC", "Stream RPC"),
    };
    let status_label = match state.monitor_filter {
        MonitorFilter::All => tr("全部狀態", "All Status"),
        MonitorFilter::Success => tr("正常", "OK"),
        MonitorFilter::Slow => tr("耗時", "Slow"),
        MonitorFilter::InterruptedOrSkipped => tr("中斷/跳過", "Errors"),
    };

    draw_monitor_dropdown_button(hdc, state, pipe_dd_x, metrics_y, pipe_dd_w, dd_h, pipe_label, state.open_monitor_dropdown == Some(0), state.pressed_monitor_dropdown == Some(0), !mon_enabled);
    draw_monitor_dropdown_button(hdc, state, status_dd_x, metrics_y, status_dd_w, dd_h, status_label, state.open_monitor_dropdown == Some(1), state.pressed_monitor_dropdown == Some(1), !mon_enabled);

    // Group separator between the filters and the log action (command-bar style).
    let sep_x = (status_dd_x + status_dd_w + exp_x) / 2;
    let sep_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(theme().divider));
    let old_sp = SelectObject(hdc, sep_pen);
    let _ = MoveToEx(hdc, sep_x, metrics_y, None);
    let _ = LineTo(hdc, sep_x, metrics_y + dd_h);
    SelectObject(hdc, old_sp);
    let _ = DeleteObject(sep_pen);

    // History Table Header: 6 columns (Time | Type | Duration | File Name | Pipeline | Status) — Win11 Task Manager style
    let table_y = metrics_y + scale_px(24, state.dpi);
    let col_time_x = card_x + pad_x;
    let col_ext_x = col_time_x + scale_px(68, state.dpi);
    let col_dur_x = col_ext_x + scale_px(50, state.dpi);
    let col_name_x = col_dur_x + scale_px(64, state.dpi);
    let col_pipe_w = scale_px(76, state.dpi);
    let col_det_w = ((card_w - pad_x * 2 - scale_px(250, state.dpi)) * 42 / 100).max(scale_px(165, state.dpi));
    let col_name_w = (card_w - pad_x * 2 - scale_px(194, state.dpi) - col_pipe_w - col_det_w).max(scale_px(120, state.dpi));
    let col_pipe_x = col_name_x + col_name_w + scale_px(10, state.dpi);
    let col_det_x = col_pipe_x + col_pipe_w + scale_px(8, state.dpi);

    let table_left = col_time_x - scale_px(4, state.dpi);
    let table_right = card_x + card_w - pad_x;
    let div_gap = scale_px(6, state.dpi);
    let col_dividers = [
        col_ext_x - div_gap,
        col_dur_x - div_gap,
        col_name_x - div_gap,
        col_pipe_x - div_gap,
        col_det_x - div_gap,
    ];
    let dur_right = col_name_x - div_gap - scale_px(6, state.dpi);

    let sep_y = table_y + scale_px(19, state.dpi);
    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_secondary));
    let th_time = to_wide(tr("時間", "Time"));
    let th_ext = to_wide(tr("格式", "Type"));
    let mut th_dur = to_wide(tr("耗時", "Duration"));
    let th_name = to_wide(tr("檔案名稱", "File Name"));
    let th_pipe = to_wide(tr("管道", "Pipeline"));
    let th_det = to_wide(tr("狀態", "Status"));
    let hdr_text_y = table_y + scale_px(1, state.dpi);
    let _ = TextOutW(hdc, col_time_x, hdr_text_y, &th_time);
    let _ = TextOutW(hdc, col_ext_x, hdr_text_y, &th_ext);
    let mut hdr_dur_rc = RECT {
        left: col_dur_x,
        top: table_y,
        right: dur_right,
        bottom: sep_y - scale_px(1, state.dpi),
    };
    let _ = DrawTextW(hdc, &mut th_dur, &mut hdr_dur_rc, DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
    let _ = TextOutW(hdc, col_name_x, hdr_text_y, &th_name);
    let _ = TextOutW(hdc, col_pipe_x, hdr_text_y, &th_pipe);
    let _ = TextOutW(hdc, col_det_x, hdr_text_y, &th_det);

    // Subtle horizontal separator under table header + vertical column dividers (Win11 Task Manager style)
    let sep_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(th.divider));
    let old_sp = SelectObject(hdc, sep_pen);
    let _ = windows::Win32::Graphics::Gdi::MoveToEx(hdc, table_left, sep_y, None);
    let _ = windows::Win32::Graphics::Gdi::LineTo(hdc, table_right, sep_y);
    for &dx in &col_dividers {
        let _ = windows::Win32::Graphics::Gdi::MoveToEx(hdc, dx, table_y - scale_px(2, state.dpi), None);
        let _ = windows::Win32::Graphics::Gdi::LineTo(hdc, dx, sep_y);
    }
    SelectObject(hdc, old_sp);
    let _ = DeleteObject(sep_pen);

    // History Rows (Win11 Task Manager flat table rows)
    let row_h = scale_px(23, state.dpi);
    let rows_start_y = sep_y + scale_px(2, state.dpi);

    if !mon_enabled {
        SelectObject(hdc, state.font_regular);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_dim));
        let dis_msg = tr("⚠️ 記錄運作日誌已關閉。開啟上方開關後將自動收集縮圖產生歷程與耗時指標。", "⚠️ Logging is disabled. Turn on the switch above to collect thumbnail telemetry.");
        let mut dis_w = to_wide(dis_msg);
        let mut dis_rect = RECT {
            left: col_time_x,
            top: rows_start_y + scale_px(20, state.dpi),
            right: card_x + card_w - pad_x,
            bottom: rows_start_y + scale_px(60, state.dpi),
        };
        let _ = DrawTextW(hdc, &mut dis_w, &mut dis_rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    } else if filtered_history.is_empty() {
        SelectObject(hdc, state.font_regular);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_dim));
        let empty_w = to_wide(tr("尚無符合篩選條件之記錄", "No matching records"));
        let _ = TextOutW(hdc, col_time_x, rows_start_y + scale_px(16, state.dpi), &empty_w);
    } else {
        let mut last_row_bottom = rows_start_y;
        for (idx, item) in filtered_history.iter().skip(scroll_offset).take(visible_rows).enumerate() {
            let ry = rows_start_y + (idx as i32 * row_h);
            if ry + row_h > bottom_limit {
                break;
            }
            last_row_bottom = ry + row_h;

            // Win11 Task Manager flat full-width row hover highlight
            let is_row_hovered = state.expanded_monitor_item.is_none()
                && state.open_monitor_dropdown.is_none()
                && state.mouse_pos.0 >= table_left
                && state.mouse_pos.0 <= table_right
                && state.mouse_pos.1 >= ry
                && state.mouse_pos.1 < ry + row_h;

            if is_row_hovered {
                let h_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(th.btn_hover_bg));
                let h_rc = RECT {
                    left: table_left,
                    top: ry,
                    right: table_right,
                    bottom: ry + row_h,
                };
                FillRect(hdc, &h_rc, h_brush);
                let _ = DeleteObject(h_brush);
            }

            // Time
            SelectObject(hdc, state.font_small);
            SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_dim));
            let tw = to_wide(&item.time_str);
            let _ = TextOutW(hdc, col_time_x, ry + scale_px(3, state.dpi), &tw);

            // Type Badge Pill
            let badge_text = item.ext.trim_start_matches('.').to_ascii_uppercase();
            let b_w = scale_px(36, state.dpi);
            let b_h = scale_px(16, state.dpi);
            let (b_bg, b_fg) = if th.dark {
                match badge_text.as_str() {
                    "MP4" | "MKV" | "WEBM" | "AVI" | "MOV" | "FLV" | "TS" => (0x00552D12, 0x0000D0FF),
                    "CBZ" | "ZIP" | "CBR" | "RAR" | "7Z" | "CBT" | "TAR" => (0x002D4522, 0x0000FF99),
                    "EPUB" | "MOBI" | "IBA" | "AZW" | "AZW3" | "PDF" => (0x001E3852, 0x0050C8FF),
                    "DOCX" | "DOC" | "XLSX" | "XLS" | "PSD" | "TXT" | "MD" => (0x00453820, 0x00FFE082),
                    "MP3" | "FLAC" | "M4A" | "WAV" | "OGG" => (0x004A1E42, 0x00FF88EE),
                    _ => (0x00333333, 0x00CCCCCC),
                }
            } else {
                match badge_text.as_str() {
                    "MP4" | "MKV" | "WEBM" | "AVI" | "MOV" | "FLV" | "TS" => (0x00FFE5CC, 0x008A4500),
                    "CBZ" | "ZIP" | "CBR" | "RAR" | "7Z" | "CBT" | "TAR" => (0x00DFF3E4, 0x001B5E20),
                    "EPUB" | "MOBI" | "IBA" | "AZW" | "AZW3" | "PDF" => (0x00D8ECFF, 0x0000509E),
                    "DOCX" | "DOC" | "XLSX" | "XLS" | "PSD" | "TXT" | "MD" => (0x00E0F2FE, 0x000369A1),
                    "MP3" | "FLAC" | "M4A" | "WAV" | "OGG" => (0x00F3E5F5, 0x006A1B9A),
                    _ => (0x00EAEAEA, 0x00444444),
                }
            };
            let b_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(b_bg));
            let old_bb = SelectObject(hdc, b_brush);
            let _ = RoundRect(hdc, col_ext_x, ry + 2, col_ext_x + b_w, ry + 2 + b_h, 4, 4);
            SelectObject(hdc, old_bb);
            let _ = DeleteObject(b_brush);

            SelectObject(hdc, state.font_small);
            SetTextColor(hdc, windows::Win32::Foundation::COLORREF(b_fg));
            let btw = to_wide(&badge_text);
            let _ = TextOutW(hdc, col_ext_x + scale_px(3, state.dpi), ry + scale_px(2, state.dpi), &btw);

            // Duration (right-aligned like Win11 Task Manager PID column)
            let (dur_text, dur_color) = if item.duration_ms >= 2000 {
                (format!("{:.1}s", item.duration_ms as f32 / 1000.0), th.status_warn)
            } else if item.duration_ms >= 500 {
                (format!("{}ms", item.duration_ms), accent_light_bgr())
            } else {
                (format!("{}ms", item.duration_ms), th.status_ok)
            };
            SetTextColor(hdc, windows::Win32::Foundation::COLORREF(dur_color));
            let mut dw = to_wide(&dur_text);
            let mut dur_rc = RECT {
                left: col_dur_x,
                top: ry,
                right: dur_right,
                bottom: ry + row_h,
            };
            let _ = DrawTextW(hdc, &mut dw, &mut dur_rc, DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);

            // File Name (strictly clipped in rect with ellipsis)
            SelectObject(hdc, state.font_regular);
            SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_primary));
            let mut fn_rect = RECT {
                left: col_name_x,
                top: ry + scale_px(2, state.dpi),
                right: col_pipe_x - div_gap - scale_px(4, state.dpi),
                bottom: ry + row_h,
            };
            let mut fn_w = to_wide(item.display_name());
            let _ = DrawTextW(hdc, &mut fn_w, &mut fn_rect, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);

            // Pipeline Column (without brackets)
            SelectObject(hdc, state.font_small);
            SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_secondary));
            let mut pipe_rect = RECT {
                left: col_pipe_x,
                top: ry + scale_px(3, state.dpi),
                right: col_det_x - div_gap - scale_px(4, state.dpi),
                bottom: ry + row_h,
            };
            let mut pipe_w = to_wide(&normalize_pipeline_display(&item.pipeline));
            let _ = DrawTextW(hdc, &mut pipe_w, &mut pipe_rect, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);

            // Details & Result (strictly clipped in remaining width)
            let (det_text, det_color) = format_monitor_row_details(item, current_lang());
            SetTextColor(hdc, windows::Win32::Foundation::COLORREF(det_color));
            let mut det_rect = RECT {
                left: col_det_x,
                top: ry + scale_px(2, state.dpi),
                right: table_right,
                bottom: ry + row_h,
            };
            let mut dtw = to_wide(&det_text);
            let _ = DrawTextW(hdc, &mut dtw, &mut det_rect, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
        }

        // Subtle Win11 Task Manager vertical column gridlines across rendered rows
        if last_row_bottom > rows_start_y {
            let grid_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(th.divider));
            let old_gp = SelectObject(hdc, grid_pen);
            for &dx in &col_dividers {
                let _ = windows::Win32::Graphics::Gdi::MoveToEx(hdc, dx, sep_y, None);
                let _ = windows::Win32::Graphics::Gdi::LineTo(hdc, dx, last_row_bottom);
            }
            SelectObject(hdc, old_gp);
            let _ = DeleteObject(grid_pen);
        }
    }
}

/// Background of a borderless command-bar ("subtle") button: nothing at rest, a fill on hover /
/// open, a slightly darker fill while pressed. Drawn 2 px taller than the hit row on both sides.
unsafe fn draw_subtle_button_bg(hdc: HDC, state: &AppState, x: i32, y: i32, w: i32, h: i32, hovered: bool, pressed: bool, open: bool) {
    let th = theme();
    let fill = if pressed {
        Some(lerp_color(th.card_bg, th.btn_hover_bg, 0.55))
    } else if hovered || open {
        Some(th.btn_hover_bg)
    } else {
        None
    };
    if let Some(col) = fill {
        let pad = scale_px(2, state.dpi);
        let r = scale_px(8, state.dpi);
        let br = CreateSolidBrush(windows::Win32::Foundation::COLORREF(col));
        let pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(col));
        let ob = SelectObject(hdc, br);
        let op = SelectObject(hdc, pen);
        let _ = RoundRect(hdc, x, y - pad, x + w, y + h + pad, r, r);
        SelectObject(hdc, ob);
        SelectObject(hdc, op);
        let _ = DeleteObject(br);
        let _ = DeleteObject(pen);
    }
}

fn is_mouse_over(state: &AppState, x: i32, y: i32, w: i32, h: i32) -> bool {
    state.mouse_pos.0 >= x && state.mouse_pos.0 <= x + w && state.mouse_pos.1 >= y && state.mouse_pos.1 <= y + h
}

/// Monitor filter trigger: borderless command-bar button with the current choice and a chevron.
#[allow(clippy::too_many_arguments)]
unsafe fn draw_monitor_dropdown_button(
    hdc: HDC,
    state: &AppState,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    label: &str,
    is_open: bool,
    is_pressed: bool,
    disabled: bool,
) {
    let th = theme();
    let is_hovered = !disabled && is_mouse_over(state, x, y, w, h);
    if !disabled {
        draw_subtle_button_bg(hdc, state, x, y, w, h, is_hovered, is_pressed, is_open);
    }
    let text_color = if disabled {
        th.text_disabled
    } else if is_pressed {
        th.text_secondary
    } else if is_open || is_hovered {
        th.text_bright
    } else {
        th.text_primary
    };
    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(text_color));
    let mut lw = to_wide(label);
    let mut text_rect = RECT {
        left: x + scale_px(8, state.dpi),
        top: y,
        right: x + w - scale_px(18, state.dpi),
        bottom: y + h,
    };
    let _ = DrawTextW(hdc, &mut lw, &mut text_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);

    let arrow_cx = x + w - scale_px(11, state.dpi);
    let arrow_cy = y + h / 2;
    let arrow_col = if is_open { accent_light_bgr() } else if disabled { th.text_disabled } else { th.text_muted };
    draw_chevron(hdc, state, arrow_cx, arrow_cy, is_open, arrow_col);
}

/// Borderless command-bar button with a centred label (e.g. "Clear Log").
#[allow(clippy::too_many_arguments)]
unsafe fn draw_subtle_button(hdc: HDC, state: &AppState, btn_id: u32, x: i32, y: i32, w: i32, h: i32, label: &str, disabled: bool) {
    let th = theme();
    let is_pressed = !disabled && state.pressed_btn == Some(btn_id);
    let is_hovered = !disabled && is_mouse_over(state, x, y, w, h);
    if !disabled {
        draw_subtle_button_bg(hdc, state, x, y, w, h, is_hovered, is_pressed, false);
    }
    let text_color = if disabled {
        th.text_disabled
    } else if is_pressed {
        th.text_secondary
    } else if is_hovered {
        th.text_bright
    } else {
        th.text_primary
    };
    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(text_color));
    let mut lw = to_wide(label);
    let mut rc = RECT { left: x, top: y, right: x + w, bottom: y + h };
    let _ = DrawTextW(hdc, &mut lw, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
}


fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn prewarm_size_button_geom(state: &AppState, x: i32, w: i32, card_y: i32) -> (i32, i32, i32, i32) {
    let bw = scale_px(130, state.dpi);
    (x + w - scale_px(16, state.dpi) - bw, card_y + scale_px(12, state.dpi), bw, scale_px(24, state.dpi))
}

/// Starts pre-generating thumbnails for `folder` (+ 3 sub-folder levels) into the system cache.
fn start_prewarm(state: &mut AppState, folder: PathBuf) {
    if state.prewarm.is_active() {
        return;
    }
    state.prewarm_shown = true;
    if cbx_core::media::is_network_path(&folder) && !state.cache_include_network {
        state.cache_status_kind = CacheStatusKind::Custom(format!(
            "⚠️ {}",
            tr("網路位置請先勾選「包含網路設備」", "Tick \"Include Network Devices\" for network folders")
        ));
        state.prewarm_shown = false;
        return;
    }
    let opts = cbx_core::prewarm::PrewarmOptions {
        size: state.prewarm_size,
        force: state.cache_force_refresh,
        threads: cbx_core::prewarm::default_threads(),
        max_depth: cbx_core::prewarm::MAX_PREWARM_DEPTH,
    };
    state.prewarm_last_done = 0;
    cbx_core::prewarm::start(folder, opts, std::sync::Arc::clone(&state.prewarm));
}

fn prewarm_status_text(state: &AppState) -> String {
    use std::sync::atomic::Ordering::Acquire;
    let p = &state.prewarm;
    let total = p.total.load(Acquire);
    let done = p.done.load(Acquire);
    let generated = p.generated.load(Acquire);
    let cached = p.cached.load(Acquire);
    let failed = p.failed.load(Acquire);
    if p.scanning.load(Acquire) {
        let root = p.root.lock().map(|r| r.clone()).unwrap_or_default();
        return format!("🔎 {} {root}", tr("掃描中…", "Scanning…"));
    }
    if p.running.load(Acquire) {
        let current = p.current.lock().map(|c| c.clone()).unwrap_or_default();
        return format!(
            "⏳ {done}/{total}（{} {generated}、{} {cached}、{} {failed}）{current}",
            tr("新產生", "new"),
            tr("已在快取", "cached"),
            tr("失敗", "failed")
        );
    }
    let secs = p.elapsed_ms.load(Acquire) as f64 / 1000.0;
    let head = if p.cancel.load(Acquire) { tr("⏹ 已停止", "⏹ Stopped") } else { tr("✅ 完成", "✅ Done") };
    format!(
        "{head} {done}/{total}：{} {generated}、{} {cached}、{} {failed}（{secs:.1} {}）",
        tr("新產生", "new"),
        tr("已在快取", "cached"),
        tr("失敗", "failed"),
        tr("秒", "s")
    )
}

/// Repaints while a pre-generation run makes progress (and once when it ends).
unsafe fn prewarm_tick(hwnd: HWND, state: &mut AppState) {
    let active = state.prewarm.is_active();
    let done = state.prewarm.done.load(std::sync::atomic::Ordering::Acquire);
    if (active && done != state.prewarm_last_done) || active != state.prewarm_was_active {
        state.prewarm_last_done = done;
        state.prewarm_was_active = active;
        if state.active_tab == ManagerTab::Advanced {
            let _ = InvalidateRect(hwnd, None, false);
        }
    }
}

#[repr(C)]
struct BrowseInfoW {
    hwnd_owner: *mut std::ffi::c_void,
    pidl_root: *const std::ffi::c_void,
    display_name: *mut u16,
    title: *const u16,
    flags: u32,
    callback: *const std::ffi::c_void,
    lparam: isize,
    image: i32,
}

#[link(name = "shell32")]
extern "system" {
    #[link_name = "SHBrowseForFolderW"]
    fn sh_browse_for_folder_w(bi: *const BrowseInfoW) -> *mut std::ffi::c_void;
    #[link_name = "SHGetPathFromIDListW"]
    fn sh_get_path_from_id_list_w(pidl: *const std::ffi::c_void, path: *mut u16) -> i32;
}

#[link(name = "ole32")]
extern "system" {
    #[link_name = "CoTaskMemFree"]
    fn co_task_mem_free(p: *mut std::ffi::c_void);
    #[link_name = "CoInitializeEx"]
    fn co_initialize_ex(reserved: *mut std::ffi::c_void, flags: u32) -> i32;
}

/// Folder picker for "pre-generate thumbnails".
unsafe fn pick_folder(hwnd: HWND) -> Option<PathBuf> {
    const BIF_RETURNONLYFSDIRS: u32 = 0x1;
    const BIF_NEWDIALOGSTYLE: u32 = 0x40;
    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    // The new-style dialog needs an STA; S_FALSE when the UI thread already has one.
    let _ = co_initialize_ex(std::ptr::null_mut(), COINIT_APARTMENTTHREADED);
    let mut name = [0u16; 260];
    let title = to_wide(tr(
        "選擇要預先產生縮圖的資料夾（含 3 層子資料夾）",
        "Choose a folder to pre-generate thumbnails for (3 sub-folder levels)",
    ));
    let bi = BrowseInfoW {
        hwnd_owner: hwnd.0,
        pidl_root: std::ptr::null(),
        display_name: name.as_mut_ptr(),
        title: title.as_ptr(),
        flags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
        callback: std::ptr::null(),
        lparam: 0,
        image: 0,
    };
    let pidl = sh_browse_for_folder_w(&bi);
    if pidl.is_null() {
        return None;
    }
    let mut path = [0u16; 1024];
    let ok = sh_get_path_from_id_list_w(pidl, path.as_mut_ptr()) != 0;
    co_task_mem_free(pidl);
    if !ok {
        return None;
    }
    let len = path.iter().position(|&c| c == 0).unwrap_or(path.len());
    Some(PathBuf::from(String::from_utf16_lossy(&path[..len])))
}

fn monitor_lock_button_geom(state: &AppState, toggle_x: i32) -> (i32, i32) {
    let w = scale_px(118, state.dpi);
    (toggle_x - scale_px(8, state.dpi) - w, w)
}

fn monitor_lock_label(settings: &Settings) -> String {
    if settings.monitor_locked {
        return tr("🔒 持續記錄", "🔒 Keep logging").to_string();
    }
    match settings.monitor_seconds_left(unix_now()) {
        Some(left) => {
            let m = left.div_ceil(60).max(1);
            if matches!(current_lang(), UiLang::ZhTw) {
                format!("🔓 剩 {m} 分鐘")
            } else {
                format!("🔓 {m} min left")
            }
        }
        None => tr("🔓 10 分鐘", "🔓 10 min").to_string(),
    }
}

/// Switches unlocked logging off once its 10-minute window has passed and keeps the
/// "minutes left" label current.
unsafe fn monitor_auto_off_tick(hwnd: HWND, state: &mut AppState) {
    let now = unix_now();
    if state.settings.monitor_enabled && !state.settings.monitor_effective(now) {
        state.settings.monitor_enabled = false;
        state.settings.monitor_locked = false;
        let _ = state.settings.save();
        state.monitor_anim_start = Some((std::time::Instant::now(), false));
        let _ = SetTimer(hwnd, 1, 16, None);
        trigger_toggle_animation(hwnd, state, 99, false);
        // History is kept (unlike the manual switch-off) so the log can still be exported.
        state.monitor_left_min = None;
        let _ = InvalidateRect(hwnd, None, false);
        return;
    }
    let left = state.settings.monitor_seconds_left(now).map(|s| s.div_ceil(60));
    if left != state.monitor_left_min {
        state.monitor_left_min = left;
        let _ = InvalidateRect(hwnd, None, false);
    }
}

fn about_update_card_h(dpi: u32) -> i32 {
    scale_px(104, dpi)
}

struct UpdateButton {
    id: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    label: String,
    disabled: bool,
}

/// Buttons of the "Software update" card; shared by painting and hit-testing.
fn about_update_buttons(state: &AppState, x: i32, w: i32, card_y: i32) -> Vec<UpdateButton> {
    let dpi = state.dpi;
    let phase = updater::phase();
    let busy = updater::is_busy();
    let configured = updater::is_configured();
    let by = card_y + scale_px(68, dpi);
    let bh = scale_px(24, dpi);
    let gap = scale_px(8, dpi);
    let mut bx = x + scale_px(16, dpi);
    let mut out = Vec::new();

    let check_w = scale_px(110, dpi);
    out.push(UpdateButton {
        id: 320,
        x: bx,
        y: by,
        w: check_w,
        h: bh,
        label: tr("🔄 檢查更新", "🔄 Check now").to_string(),
        disabled: busy || !configured,
    });
    bx += check_w + gap;

    if let updater::Phase::Available(m) = &phase {
        let inst_w = scale_px(170, dpi);
        out.push(UpdateButton {
            id: 321,
            x: bx,
            y: by,
            w: inst_w,
            h: bh,
            label: format!("{} v{}", tr("⬇ 下載並安裝", "⬇ Install"), m.version),
            disabled: busy,
        });
        bx += inst_w + gap;
    }

    let notes_w = scale_px(110, dpi);
    out.push(UpdateButton {
        id: 322,
        x: bx,
        y: by,
        w: notes_w,
        h: bh,
        label: tr("📄 版本資訊", "📄 Releases").to_string(),
        disabled: false,
    });

    let auto_w = scale_px(170, dpi);
    let auto_label = if state.settings.auto_check_updates {
        tr("自動檢查更新：開", "Auto check: On")
    } else {
        tr("自動檢查更新：關", "Auto check: Off")
    };
    out.push(UpdateButton {
        id: 323,
        x: x + w - scale_px(16, dpi) - auto_w,
        y: by,
        w: auto_w,
        h: bh,
        label: auto_label.to_string(),
        disabled: !configured,
    });
    out
}

fn update_status_text(state: &AppState) -> (String, bool) {
    let cur = APP_VERSION;
    match updater::phase() {
        _ if !updater::is_configured() => (
            tr(
                "此版本未設定更新簽章金鑰，無法自動更新（請至 GitHub 發行頁面下載）",
                "This build has no update signing key; download new versions from GitHub Releases",
            )
            .to_string(),
            false,
        ),
        updater::Phase::Idle => {
            let last = state.settings.last_update_check as u64;
            let mut text = format!("{} {cur}", tr("目前版本", "Current version"));
            if last != 0 {
                let (y, m, d) = update_core::unix_to_ymd(last);
                text.push_str(&format!(" · {} {y:04}-{m:02}-{d:02}", tr("上次檢查", "last checked")));
            }
            if updater::requires_approval() {
                text.push_str(tr(" · 更新需維護者 YubiKey 核准", " · updates require maintainer YubiKey approval"));
            }
            (text, false)
        }
        updater::Phase::Checking => (tr("正在檢查更新…", "Checking for updates…").to_string(), false),
        updater::Phase::UpToDate => (format!("{}（{cur}）", tr("已是最新版本", "You're up to date")), false),
        updater::Phase::Available(m) => (
            format!(
                "{} v{}{}（{} {cur}）",
                tr("有新版本", "New version"),
                m.version,
                tr(" 可供安裝", " is available"),
                tr("目前", "current")
            ),
            false,
        ),
        updater::Phase::Downloading { done, total, version } => {
            let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
            let text = if total > 0 {
                format!(
                    "{} v{version} … {}%（{:.1} / {:.1} MiB）",
                    tr("正在下載", "Downloading"),
                    done * 100 / total,
                    mb(done),
                    mb(total)
                )
            } else {
                format!("{} v{version} … {:.1} MiB", tr("正在下載", "Downloading"), mb(done))
            };
            (text, false)
        }
        updater::Phase::Launching => (
            tr("簽章驗證通過，正在啟動安裝程式…", "Signature verified; starting the installer…").to_string(),
            false,
        ),
        updater::Phase::Error(e) => (format!("{}{e}", tr("更新失敗：", "Update failed: ")), true),
    }
}

unsafe fn render_update_card(hdc: HDC, state: &AppState, x: i32, y: i32, w: i32) {
    let th = theme();
    let dpi = state.dpi;
    draw_card_container(hdc, state, x, y, w, about_update_card_h(dpi));

    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_bright));
    let title = to_wide(tr("軟體更新", "Software update"));
    let _ = TextOutW(hdc, x + scale_px(16, dpi), y + scale_px(12, dpi), &title);

    let (status, is_err) = update_status_text(state);
    SelectObject(hdc, state.font_regular);
    SetTextColor(
        hdc,
        windows::Win32::Foundation::COLORREF(if is_err { th.status_warn } else { th.text_secondary }),
    );
    let mut rc = RECT {
        left: x + scale_px(16, dpi),
        top: y + scale_px(36, dpi),
        right: x + w - scale_px(16, dpi),
        bottom: y + scale_px(60, dpi),
    };
    let mut sw = to_wide(&status);
    let _ = DrawTextW(hdc, &mut sw, &mut rc, DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);

    for b in about_update_buttons(state, x, w, y) {
        draw_subtle_button(hdc, state, b.id, b.x, b.y, b.w, b.h, &b.label, b.disabled);
    }
}

unsafe fn handle_update_button(hwnd: HWND, state: &mut AppState, id: u32) {
    match id {
        320 => updater::start_check(hwnd.0),
        321 => {
            if let updater::Phase::Available(m) = updater::phase() {
                use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, IDOK, MB_ICONQUESTION, MB_OKCANCEL};
                let body = to_wide(&format!(
                    "{} v{}{}",
                    tr("將下載並安裝 CBX Shell ", "CBX Shell "),
                    m.version,
                    tr(
                        "。\n\n安裝檔會先驗證數位簽章；安裝期間管理工具會關閉，完成後自動重新開啟，設定與檔案關聯都會保留。",
                        " will be downloaded and installed.\n\nThe installer's signature is verified first. The manager closes during the update and reopens afterwards; settings and file associations are kept."
                    )
                ));
                let title = to_wide(tr("安裝更新", "Install update"));
                let answer = MessageBoxW(hwnd, PCWSTR(body.as_ptr()), PCWSTR(title.as_ptr()), MB_OKCANCEL | MB_ICONQUESTION);
                if answer == IDOK {
                    updater::start_install(hwnd.0, m);
                }
            }
        }
        322 => {
            let version = match updater::phase() {
                updater::Phase::Available(m) => Some(m.version),
                _ => None,
            };
            updater::open_release_page(version.as_deref());
        }
        323 => {
            state.settings.auto_check_updates = !state.settings.auto_check_updates;
            let _ = state.settings.save();
        }
        _ => {}
    }
}

unsafe fn render_about_page(hdc: HDC, state: &AppState, x: i32, w: i32) {
    let offset = (if is_media_preview_installed() { scale_px(45, state.dpi) } else { 0 }) - state.scroll_y;

    SelectObject(hdc, state.font_regular);
    let summary = tr(
        "Windows 多媒體與圖檔封裝縮圖擴充套件。\n\n技術架構與設計參考：\n• 核心選影格演算法：參考 BabelSoft Media Preview 之選影格指標（位置定位、亮度門檻與圖像熵分析），以 Rust 重構實現。\n• 解碼管線：採用 Windows Media Foundation (Direct3D 11 / DXVA) 原生解碼與 FFmpeg 回退支援。\n• 圖檔封裝格式：Rust 串流解壓（ZIP、RAR、7z、TAR、EPUB、WebP、JPEG XL）。",
        "Shell extension engineered for Windows multimedia and image archive thumbnailing.\n\nArchitecture & Design References:\n• Frame Selection: References evaluation principles from BabelSoft Media Preview (seek position, luminance bounds, Shannon entropy), re-implemented in Rust.\n• Decoding Pipeline: Windows Media Foundation (Direct3D 11 / DXVA) native decoders with FFmpeg fallback support.\n• Image Archives & Formats: Rust streaming decoders (ZIP, RAR, 7z, TAR, EPUB, WebP, JPEG XL)."
    );

    // Dynamically calculate wrapped text height so Card 1 adapts to any window width
    let summary_max_w = (w - scale_px(32, state.dpi)).max(100);
    let mut rect_calc = RECT {
        left: 0,
        top: 0,
        right: summary_max_w,
        bottom: 0,
    };
    let mut sw_calc = to_wide(summary);
    let _ = DrawTextW(hdc, &mut sw_calc, &mut rect_calc, DT_CALCRECT | DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);
    let summary_text_h = (rect_calc.bottom - rect_calc.top).max(scale_px(95, state.dpi));

    // Card 1: Project overview and architectural references
    let card1_y = scale_px(75, state.dpi) + offset;
    let card1_h = scale_px(58, state.dpi) + summary_text_h + scale_px(16, state.dpi);
    state.about_card1_h.set(card1_h);
    draw_card_container(hdc, state, x, card1_y, w, card1_h);

    let th = theme();
    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_bright));
    let name_w = to_wide(&format!("CBX Shell {APP_VERSION} (cbxshell-rs)"));
    let _ = TextOutW(hdc, x + scale_px(16, state.dpi), card1_y + scale_px(14, state.dpi), &name_w);

    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_muted));
    let lic_w = to_wide("License: GNU General Public License v3.0 (GPL-3.0-or-later)");
    let _ = TextOutW(hdc, x + scale_px(16, state.dpi), card1_y + scale_px(34, state.dpi), &lic_w);

    SelectObject(hdc, state.font_regular);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_secondary));
    let mut rect_summary = RECT {
        left: x + scale_px(16, state.dpi),
        top: card1_y + scale_px(54, state.dpi),
        right: x + w - scale_px(16, state.dpi),
        bottom: card1_y + card1_h - scale_px(8, state.dpi),
    };
    let mut sw = to_wide(summary);
    let _ = DrawTextW(hdc, &mut sw, &mut rect_summary, DT_LEFT | DT_WORDBREAK | DT_NOPREFIX);

    // Card: Software update (version check / signed auto-update)
    let upd_y = card1_y + card1_h + scale_px(14, state.dpi);
    render_update_card(hdc, state, x, upd_y, w);

    // Card 2: Software Bill of Materials (SBOM) Table
    let card2_y = upd_y + about_update_card_h(state.dpi) + scale_px(14, state.dpi);
    let card2_h = scale_px(404, state.dpi);
    draw_card_container(hdc, state, x, card2_y, w, card2_h);

    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_bright));
    let sbom_title = to_wide(tr("軟體物料清單 (SBOM)", "Software Bill of Materials (SBOM)"));
    let _ = TextOutW(hdc, x + scale_px(16, state.dpi), card2_y + scale_px(12, state.dpi), &sbom_title);

    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_dim));
    let sbom_sub = to_wide(tr("（核心依賴函式庫、版本、開源條款與職責角色）", "(Core dependencies, versions, licenses, and roles)"));
    let _ = TextOutW(hdc, x + scale_px(360, state.dpi), card2_y + scale_px(14, state.dpi), &sbom_sub);

    // Table Header
    let pad_x = scale_px(16, state.dpi);
    let th_y = card2_y + scale_px(36, state.dpi);
    let col_name_x = x + pad_x;
    let col_name_w = scale_px(135, state.dpi);
    let col_ver_x = col_name_x + col_name_w;
    let col_ver_w = scale_px(75, state.dpi);
    let col_lic_x = col_ver_x + col_ver_w;
    let col_lic_w = scale_px(140, state.dpi);
    let col_role_x = col_lic_x + col_lic_w;

    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_secondary));
    let mut h_name = to_wide(tr("組件名稱", "Component"));
    let mut h_ver = to_wide(tr("版本", "Version"));
    let mut h_lic = to_wide(tr("授權條款", "License"));
    let mut h_role = to_wide(tr("核心職責與功能", "Role / Function"));

    let mut r_hn = RECT { left: col_name_x, top: th_y, right: col_name_x + col_name_w, bottom: th_y + scale_px(18, state.dpi) };
    let mut r_hv = RECT { left: col_ver_x, top: th_y, right: col_ver_x + col_ver_w, bottom: th_y + scale_px(18, state.dpi) };
    let mut r_hl = RECT { left: col_lic_x, top: th_y, right: col_lic_x + col_lic_w, bottom: th_y + scale_px(18, state.dpi) };
    let mut r_hr = RECT { left: col_role_x, top: th_y, right: x + w - pad_x, bottom: th_y + scale_px(18, state.dpi) };

    let _ = DrawTextW(hdc, &mut h_name, &mut r_hn, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    let _ = DrawTextW(hdc, &mut h_ver, &mut r_hv, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    let _ = DrawTextW(hdc, &mut h_lic, &mut r_hl, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    let _ = DrawTextW(hdc, &mut h_role, &mut r_hr, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

    // Separator line
    let sep_y = th_y + scale_px(20, state.dpi);
    let sep_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(th.divider));
    let old_sp = SelectObject(hdc, sep_pen);
    let _ = windows::Win32::Graphics::Gdi::MoveToEx(hdc, col_name_x, sep_y, None);
    let _ = windows::Win32::Graphics::Gdi::LineTo(hdc, x + w - pad_x, sep_y);
    SelectObject(hdc, old_sp);
    let _ = DeleteObject(sep_pen);

    // Rows
    let sbom_items: [(&str, &str, &str, &str, &str); 11] = [
        ("windows-rs", "0.58.0", "MIT / Apache-2.0", "Windows Shell COM 原生介面與系統底層整合", "Native Windows Shell COM APIs & Direct Integration"),
        ("image", "0.25.10", "MIT / Apache-2.0", "點陣圖色彩空間轉換、高質量縮放與影像解碼", "Bitmap color transforms, scaling & core image codecs"),
        ("image-webp", "0.2.4", "MIT / Apache-2.0", "靜態與動態 WebP 影像串流解碼", "Static and animated WebP streaming decoder"),
        ("jxl-oxide", "0.12.6", "MIT / Apache-2.0", "JPEG XL (JXL) 影像解碼與色彩處理", "JPEG XL image decoder & color processing"),
        ("zip", "2.6.1", "MIT / Apache-2.0", "ZIP / CBZ / EPUB 封裝解析與大檔 ZIP64 支援", "ZIP / CBZ / EPUB archive parser with ZIP64 support"),
        ("sevenz-rust2", "0.25.0", "Apache-2.0", "7z / CB7 封裝格式多演算法串流解壓", "7z / CB7 multi-codec streaming decompressor"),
        ("rar-codec", "0.1.10", "Apache-2.0", "RAR / CBR 壓縮串流原生解碼（無需外部 DLL）", "Pure Rust RAR / CBR streaming codec without DLLs"),
        ("tar", "0.4.46", "MIT / Apache-2.0", "TAR / CBT 封存格式串流表頭掃描與解包", "TAR / CBT streaming header parser & extractor"),
        ("quick-xml", "0.37.5", "MIT", "EPUB 電子書標準 OPF 清單與封面詮釋資料解析", "EPUB standard OPF manifest & cover metadata parser"),
        ("everything-ipc", "0.1.4", "MIT", "Everything 桌面搜尋引擎 IPC 實體路徑反查介面", "Everything desktop search IPC path resolution"),
        ("Media Foundation", "System", "Microsoft Corp.", "系統原生 Direct3D 11 / DXVA 硬體解碼管線", "OS native Direct3D 11 / DXVA hardware decoding pipeline"),
    ];

    let row_h = scale_px(29, state.dpi);
    let rows_start_y = sep_y + scale_px(4, state.dpi);

    for (idx, (comp, ver, lic, role_zh, role_en)) in sbom_items.iter().enumerate() {
        let ry = rows_start_y + (idx as i32 * row_h);

        // Alternating row background
        if idx % 2 == 1 {
            let row_bg = CreateSolidBrush(windows::Win32::Foundation::COLORREF(th.table_row_alt_bg));
            let r_row = RECT { left: col_name_x - scale_px(4, state.dpi), top: ry, right: x + w - pad_x + scale_px(4, state.dpi), bottom: ry + row_h };
            FillRect(hdc, &r_row, row_bg);
            let _ = DeleteObject(row_bg);
        }

        // Component name
        SelectObject(hdc, state.font_bold);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(accent_light_bgr()));
        let mut r_n = RECT { left: col_name_x, top: ry, right: col_name_x + col_name_w, bottom: ry + row_h };
        let mut cn = to_wide(comp);
        let _ = DrawTextW(hdc, &mut cn, &mut r_n, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);

        // Version
        SelectObject(hdc, state.font_regular);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_muted));
        let mut r_v = RECT { left: col_ver_x, top: ry, right: col_ver_x + col_ver_w, bottom: ry + row_h };
        let mut cv = to_wide(ver);
        let _ = DrawTextW(hdc, &mut cv, &mut r_v, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);

        // License
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.status_warn));
        let mut r_l = RECT { left: col_lic_x, top: ry, right: col_lic_x + col_lic_w, bottom: ry + row_h };
        let mut cl = to_wide(lic);
        let _ = DrawTextW(hdc, &mut cl, &mut r_l, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);

        // Role / Function
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_secondary));
        let mut r_r = RECT { left: col_role_x, top: ry, right: x + w - pad_x, bottom: ry + row_h };
        let mut cr = to_wide(tr(role_zh, role_en));
        let _ = DrawTextW(hdc, &mut cr, &mut r_r, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS);
    }
}

// -----------------------------------------------------------------------------
// Fluent Widgets: Checkbox Grid, Border Selector & Live Preview
// -----------------------------------------------------------------------------

unsafe fn render_extensions_grid(
    hdc: HDC,
    state: &AppState,
    card_x: i32,
    card_y: i32,
    card_w: i32,
    cat: MediaCategory,
    btn_base_id: u32,
    title: &str,
) {
    let th = theme();
    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_primary));
    let tw = to_wide(title);
    let _ = TextOutW(hdc, card_x + scale_px(16, state.dpi), card_y + scale_px(14, state.dpi), &tw);

    let formats: Vec<&MediaFormatInfo> = SUPPORTED_MEDIA_FORMATS
        .iter()
        .filter(|fmt| fmt.category == cat)
        .collect();

    let cols = 5;
    let col_w = (card_w - scale_px(40, state.dpi)) / cols;
    let item_h = scale_px(26, state.dpi);
    let start_x = card_x + scale_px(20, state.dpi);
    let start_y = card_y + scale_px(45, state.dpi);

    let cat_cb_base: u32 = match cat {
        MediaCategory::ImageArchive => 50_000,
        MediaCategory::Video => 50_100,
        MediaCategory::Audio => 50_200,
        MediaCategory::Ebook => 50_300,
        MediaCategory::Other => 50_400,
        MediaCategory::Syntax => 50_500,
    };

    for (i, fmt) in formats.iter().enumerate() {
        let col = (i % cols as usize) as i32;
        let row = (i / cols as usize) as i32;
        let ix = start_x + col * col_w;
        let iy = start_y + row * item_h;

        let is_on = state.pending_associations.get(fmt.ext).copied()
            .unwrap_or_else(|| is_associated(HKEY_CURRENT_USER, fmt.ext));

        // Checkbox square (16x16) with Win11 hover color + left-to-right checkmark animation
        let cb_size = scale_px(16, state.dpi);
        let is_hovered = state.expanded_monitor_item.is_none()
            && state.mouse_pos.0 >= ix
            && state.mouse_pos.0 < ix + col_w - scale_px(6, state.dpi)
            && state.mouse_pos.1 >= iy
            && state.mouse_pos.1 < iy + item_h;
        draw_tracked_checkbox_box(
            hdc,
            state,
            cat_cb_base + i as u32,
            ix,
            iy + 2,
            cb_size,
            is_on,
            is_hovered,
            false,
        );

        // Ext label
        let th = theme();
        SelectObject(hdc, state.font_regular);
        SetTextColor(hdc, if is_on { windows::Win32::Foundation::COLORREF(th.text_primary) } else { windows::Win32::Foundation::COLORREF(th.text_muted) });
        let ext_w = to_wide(fmt.ext);
        let _ = TextOutW(hdc, ix + cb_size + scale_px(8, state.dpi), iy + scale_px(2, state.dpi), &ext_w);
    }

    // Select all / Deselect all / Apply buttons
    let total_rows = (formats.len() as i32 + cols - 1) / cols;
    let btn_y = start_y + total_rows * item_h + scale_px(8, state.dpi);
    let btn_w = scale_px(90, state.dpi);
    let btn_h = scale_px(30, state.dpi);

    draw_rounded_button(hdc, state, btn_base_id, start_x, btn_y, btn_w, btn_h, tr("全選", "Select All"), false, false);
    draw_rounded_button(hdc, state, btn_base_id + 1, start_x + btn_w + scale_px(10, state.dpi), btn_y, btn_w, btn_h, tr("全不選", "Deselect All"), false, false);
    draw_rounded_button(hdc, state, btn_base_id + 2, start_x + (btn_w + scale_px(10, state.dpi)) * 2, btn_y, scale_px(95, state.dpi), btn_h, tr("🛡️ 套用", "🛡️ Apply"), true, false);
}

const BORDER_STYLE_OPTIONS: [(u32, &str, &str); 6] = [
    (0, "無外框", "No Frame"),
    (5, "無外框圓弧", "Round Only"),
    (1, "影片捲軸", "Filmstrip"),
    (2, "柔和投影", "Drop Shadow"),
    (3, "相框風格", "Photo Frame"),
    (4, "書籍封面", "Book Cover"),
];

unsafe fn render_border_preview_widget(
    hdc: HDC,
    state: &AppState,
    card_x: i32,
    card_y: i32,
    card_w: i32,
    title: &str,
    border_style: u32,
    show_badge: bool,
    badge_toggle_label: &str,
    badge_label: &'static str,
) {
    let th = theme();
    SelectObject(hdc, state.font_bold);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_primary));
    let tw = to_wide(title);
    let _ = TextOutW(hdc, card_x + scale_px(16, state.dpi), card_y + scale_px(14, state.dpi), &tw);

    // 6 Style buttons
    let b_y = card_y + scale_px(42, state.dpi);
    let b_w = scale_px(70, state.dpi);
    let b_h = scale_px(32, state.dpi);
    let b_gap = scale_px(5, state.dpi);

    for (idx, (style_id, name_zh, name_en)) in BORDER_STYLE_OPTIONS.iter().enumerate() {
        let bx = card_x + scale_px(16, state.dpi) + (idx as i32 * (b_w + b_gap));
        let is_selected = border_style == *style_id;
        let name = tr(name_zh, name_en);
        draw_selectable_button(hdc, state, 210 + idx as u32, bx, b_y, b_w, b_h, name, is_selected, false);
    }

    // Corner badge checkbox (rendered when badge_toggle_label is non-empty)
    if !badge_toggle_label.is_empty() {
        let badge_y = card_y + scale_px(95, state.dpi);
        let chk_x = card_x + scale_px(16, state.dpi);
        let badge_cb_id = 360 + state.active_tab.to_index() as u32;
        draw_checkbox(hdc, state, badge_cb_id, chk_x, badge_y, badge_toggle_label, show_badge);
    }

    // Live Thumbnail Preview Card on the right
    let prev_w = scale_px(130, state.dpi);
    let prev_h = scale_px(98, state.dpi);
    let prev_x = card_x + card_w - prev_w - scale_px(16, state.dpi);
    let prev_y = card_y + scale_px(15, state.dpi);

    // Background of preview box
    let prev_bg_color = if th.dark { 0x00161414 } else { th.sub_card_bg };
    let prev_border_color = if th.dark { 0x00332D2D } else { th.sub_card_border };
    let prev_bg = CreateSolidBrush(windows::Win32::Foundation::COLORREF(prev_bg_color));
    let prev_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(prev_border_color));
    let old_b = SelectObject(hdc, prev_bg);
    let old_p = SelectObject(hdc, prev_pen);
    let _ = windows::Win32::Graphics::Gdi::RoundRect(hdc, prev_x, prev_y, prev_x + prev_w, prev_y + prev_h, 8, 8);
    SelectObject(hdc, old_b);
    SelectObject(hdc, old_p);
    let _ = DeleteObject(prev_bg);
    let _ = DeleteObject(prev_pen);

    // Render simulated thumbnail inside preview box
    let thumb_w = scale_px(80, state.dpi);
    let thumb_h = scale_px(65, state.dpi);
    let thumb_x = prev_x + (prev_w - thumb_w) / 2;
    let thumb_y = prev_y + (prev_h - thumb_h) / 2;

    let corner_r = match border_style {
        1 => scale_px(8, state.dpi),
        2 => scale_px(9, state.dpi),
        3 => scale_px(6, state.dpi),
        4 => scale_px(9, state.dpi),
        5 => scale_px(14, state.dpi),
        _ => 4,
    };

    if border_style == 2 {
        // Win11 Drop Shadow: layered soft rounded elevation shadow
        draw_win11_drop_shadow_layers(hdc, thumb_x, thumb_y, thumb_w, thumb_h, corner_r);
    }

    if border_style == 3 {
        // Win11 Photo Frame: outer soft shadow + rounded pearl-gradient mat border
        let frame_pad = scale_px(4, state.dpi);
        let r_frame = scale_px(10, state.dpi);
        let fx = thumb_x - frame_pad;
        let fy = thumb_y - frame_pad;
        let fw = thumb_w + frame_pad * 2;
        let fh = thumb_h + frame_pad * 2;
        draw_win11_drop_shadow_layers(hdc, fx, fy, fw, fh, r_frame);

        let outer_b = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00ECEEF2));
        let outer_p = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(0x00C4CAD4));
        let ofb = SelectObject(hdc, outer_b);
        let ofp = SelectObject(hdc, outer_p);
        let _ = RoundRect(hdc, fx, fy, fx + fw, fy + fh, r_frame, r_frame);
        SelectObject(hdc, ofb);
        SelectObject(hdc, ofp);
        let _ = DeleteObject(outer_b);
        let _ = DeleteObject(outer_p);

        // Pearl top-to-bottom lighting gradient clipped inside the outer frame rim
        let saved = windows::Win32::Graphics::Gdi::SaveDC(hdc);
        let fr_rgn = CreateRoundRectRgn(fx + 1, fy + 1, fx + fw, fy + fh, r_frame, r_frame);
        let _ = windows::Win32::Graphics::Gdi::ExtSelectClipRgn(hdc, fr_rgn, windows::Win32::Graphics::Gdi::RGN_AND);
        for dy in 1..(fh - 1) {
            let t = dy as f32 / (fh.max(1) as f32);
            let r_c = (252.0 + (224.0 - 252.0) * t).round() as u32;
            let g_c = (252.0 + (228.0 - 252.0) * t).round() as u32;
            let b_c = (254.0 + (236.0 - 254.0) * t).round() as u32;
            let col = (b_c << 16) | (g_c << 8) | r_c;
            let ln_b = CreateSolidBrush(windows::Win32::Foundation::COLORREF(col));
            let lr = RECT { left: fx + 1, top: fy + dy, right: fx + fw - 1, bottom: fy + dy + 1 };
            FillRect(hdc, &lr, ln_b);
            let _ = DeleteObject(ln_b);
        }
        let _ = windows::Win32::Graphics::Gdi::RestoreDC(hdc, saved);
        let _ = DeleteObject(fr_rgn);
    }

    if border_style == 4 {
        // Windows 11 style rounded book cover: soft elevation shadow + layered warm paper stack
        let r_book = scale_px(10, state.dpi);
        let shadow_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x000A0A0C));
        let shadow_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(0x000E0E12));
        let old_sb = SelectObject(hdc, shadow_brush);
        let old_sp = SelectObject(hdc, shadow_pen);
        let _ = RoundRect(
            hdc,
            thumb_x + 4,
            thumb_y + 4,
            thumb_x + thumb_w + 5,
            thumb_y + thumb_h + 5,
            r_book,
            r_book,
        );
        SelectObject(hdc, old_sb);
        SelectObject(hdc, old_sp);
        let _ = DeleteObject(shadow_brush);
        let _ = DeleteObject(shadow_pen);

        let pages_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00D8DFE6));
        let pages_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(0x00ADB6BF));
        let old_pb = SelectObject(hdc, pages_brush);
        let old_pp = SelectObject(hdc, pages_pen);
        let _ = RoundRect(
            hdc,
            thumb_x + 2,
            thumb_y + 2,
            thumb_x + thumb_w + 3,
            thumb_y + thumb_h + 3,
            r_book,
            r_book,
        );
        SelectObject(hdc, old_pb);
        SelectObject(hdc, old_pp);
        let _ = DeleteObject(pages_brush);
        let _ = DeleteObject(pages_pen);
    }

    // Inner thumbnail sample card
    let sample_border_col = match border_style {
        1 => 0x00585C66u32,
        2 => 0x00B8866Au32,
        3 => 0x00AEB6C2u32,
        5 => 0x00804020u32,
        _ => 0x00965838u32,
    };
    let sample_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00804020)); // warm slate
    let sample_pen = if border_style == 5 {
        CreatePen(windows::Win32::Graphics::Gdi::PS_NULL, 0, windows::Win32::Foundation::COLORREF(0))
    } else {
        CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(sample_border_col))
    };
    let old_tb = SelectObject(hdc, sample_brush);
    let old_tp = SelectObject(hdc, sample_pen);
    let _ = RoundRect(
        hdc,
        thumb_x,
        thumb_y,
        thumb_x + thumb_w,
        thumb_y + thumb_h,
        corner_r,
        corner_r,
    );
    SelectObject(hdc, old_tb);
    SelectObject(hdc, old_tp);
    let _ = DeleteObject(sample_brush);
    let _ = DeleteObject(sample_pen);

    if border_style == 2 || border_style == 3 {
        // Subtle Win11 vertical lighting gradient inside the rounded sample artwork
        let saved = windows::Win32::Graphics::Gdi::SaveDC(hdc);
        let rgn = CreateRoundRectRgn(thumb_x + 1, thumb_y + 1, thumb_x + thumb_w, thumb_y + thumb_h, corner_r, corner_r);
        let _ = windows::Win32::Graphics::Gdi::ExtSelectClipRgn(hdc, rgn, windows::Win32::Graphics::Gdi::RGN_AND);
        for dy in 1..(thumb_h - 1) {
            let t = dy as f32 / (thumb_h.max(1) as f32);
            let light = 1.12 - 0.24 * t;
            let r_c = ((32.0 * light).round() as u32).min(255);
            let g_c = ((64.0 * light).round() as u32).min(255);
            let b_c = ((128.0 * light).round() as u32).min(255);
            let col = (b_c << 16) | (g_c << 8) | r_c;
            let ln_b = CreateSolidBrush(windows::Win32::Foundation::COLORREF(col));
            let lr = RECT { left: thumb_x + 1, top: thumb_y + dy, right: thumb_x + thumb_w - 1, bottom: thumb_y + dy + 1 };
            FillRect(hdc, &lr, ln_b);
            let _ = DeleteObject(ln_b);
        }
        let _ = windows::Win32::Graphics::Gdi::RestoreDC(hdc, saved);
        let _ = DeleteObject(rgn);
    }

    if border_style == 4 {
        // Subtle cylindrical spine shading gradient + soft crease highlight
        let spine_w = scale_px(10, state.dpi);
        let saved = windows::Win32::Graphics::Gdi::SaveDC(hdc);
        let rgn = CreateRoundRectRgn(
            thumb_x,
            thumb_y,
            thumb_x + thumb_w + 1,
            thumb_y + thumb_h + 1,
            corner_r,
            corner_r,
        );
        let _ = windows::Win32::Graphics::Gdi::ExtSelectClipRgn(hdc, rgn, windows::Win32::Graphics::Gdi::RGN_AND);
        for sx in 0..spine_w {
            let t = sx as f32 / (spine_w.max(1) as f32);
            let cyl = (t * std::f32::consts::PI).sin();
            // Smooth multi-stop cover base (RGB 0x20, 0x40, 0x80 -> BGR 0x00804020) shifted subtly by cylindrical normal
            let shade = 0.72 + 0.42 * cyl;
            let r_c = ((32.0 * shade).round() as u32).min(255);
            let g_c = ((64.0 * shade).round() as u32).min(255);
            let b_c = ((128.0 * shade).round() as u32).min(255);
            let col = (b_c << 16) | (g_c << 8) | r_c;
            let col_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(col));
            let old_p = SelectObject(hdc, col_pen);
            let _ = MoveToEx(hdc, thumb_x + 1 + sx, thumb_y + 1, None);
            let _ = LineTo(hdc, thumb_x + 1 + sx, thumb_y + thumb_h - 1);
            SelectObject(hdc, old_p);
            let _ = DeleteObject(col_pen);
        }
        let crease_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(0x00B87C54));
        let old_cp = SelectObject(hdc, crease_pen);
        let _ = MoveToEx(hdc, thumb_x + spine_w, thumb_y + 2, None);
        let _ = LineTo(hdc, thumb_x + spine_w, thumb_y + thumb_h - 2);
        SelectObject(hdc, old_cp);
        let _ = DeleteObject(crease_pen);
        let _ = windows::Win32::Graphics::Gdi::RestoreDC(hdc, saved);
        let _ = DeleteObject(rgn);
    }

    if border_style == 1 {
        // Filmstrip bands clipped cleanly to Win11 rounded corners with rounded sprocket holes
        let band_w = scale_px(9, state.dpi);
        let saved = windows::Win32::Graphics::Gdi::SaveDC(hdc);
        let rgn = CreateRoundRectRgn(
            thumb_x + 1,
            thumb_y + 1,
            thumb_x + thumb_w,
            thumb_y + thumb_h,
            corner_r,
            corner_r,
        );
        let _ = windows::Win32::Graphics::Gdi::ExtSelectClipRgn(hdc, rgn, windows::Win32::Graphics::Gdi::RGN_AND);

        for dy in 0..thumb_h {
            let t = if thumb_h > 1 { dy as f32 / (thumb_h - 1) as f32 } else { 0.0 };
            let r = (31.0 + (66.0 - 31.0) * (1.0 - t)).round() as u8;
            let g = (33.0 + (70.0 - 33.0) * (1.0 - t)).round() as u8;
            let b = (37.0 + (79.0 - 37.0) * (1.0 - t)).round() as u8;
            let line_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(
                (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
            ));
            let r_l = RECT { left: thumb_x, top: thumb_y + dy, right: thumb_x + band_w, bottom: thumb_y + dy + 1 };
            let r_r = RECT { left: thumb_x + thumb_w - band_w, top: thumb_y + dy, right: thumb_x + thumb_w, bottom: thumb_y + dy + 1 };
            FillRect(hdc, &r_l, line_brush);
            FillRect(hdc, &r_r, line_brush);
            let _ = DeleteObject(line_brush);
        }

        // Crisp seam lines beside left/right bands
        let seam_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(0x00181515));
        let osp = SelectObject(hdc, seam_pen);
        let _ = MoveToEx(hdc, thumb_x + band_w - 1, thumb_y + 1, None);
        let _ = LineTo(hdc, thumb_x + band_w - 1, thumb_y + thumb_h - 1);
        let _ = MoveToEx(hdc, thumb_x + thumb_w - band_w, thumb_y + 1, None);
        let _ = LineTo(hdc, thumb_x + thumb_w - band_w, thumb_y + thumb_h - 1);
        SelectObject(hdc, osp);
        let _ = DeleteObject(seam_pen);

        let hole_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00FCF8F4));
        let hole_pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(0x00D8D2CC));
        let num_holes = 4;
        let hole_h = scale_px(6, state.dpi);
        let hole_w = scale_px(4, state.dpi);
        let step = (thumb_h - scale_px(10, state.dpi)) / num_holes;
        let ohb = SelectObject(hdc, hole_brush);
        let ohp = SelectObject(hdc, hole_pen);
        for h_idx in 0..num_holes {
            let hy = thumb_y + scale_px(6, state.dpi) + h_idx * step;
            let lx = thumb_x + scale_px(2, state.dpi);
            let rx = thumb_x + thumb_w - band_w + scale_px(2, state.dpi);
            let _ = RoundRect(hdc, lx, hy, lx + hole_w, hy + hole_h, 2, 2);
            let _ = RoundRect(hdc, rx, hy, rx + hole_w, hy + hole_h, 2, 2);
        }
        SelectObject(hdc, ohb);
        SelectObject(hdc, ohp);
        let _ = DeleteObject(hole_brush);
        let _ = DeleteObject(hole_pen);

        let _ = windows::Win32::Graphics::Gdi::RestoreDC(hdc, saved);
        let _ = DeleteObject(rgn);
    }

    if show_badge {
        // Corner badge in bottom right
        let b_bw = scale_px(28, state.dpi);
        let b_bh = scale_px(14, state.dpi);
        let b_bx = thumb_x + thumb_w - b_bw - 2;
        let b_by = thumb_y + thumb_h - b_bh - 2;

        let badge_brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x00101010));
        let old_bb = SelectObject(hdc, badge_brush);
        let _ = windows::Win32::Graphics::Gdi::RoundRect(hdc, b_bx, b_by, b_bx + b_bw, b_by + b_bh, 4, 4);
        SelectObject(hdc, old_bb);
        let _ = DeleteObject(badge_brush);

        SelectObject(hdc, state.font_small);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(0x00FFFFFF));
        let bw_str = to_wide(badge_label);
        let _ = TextOutW(hdc, b_bx + scale_px(4, state.dpi), b_by + scale_px(1, state.dpi), &bw_str);
    }
}

// -----------------------------------------------------------------------------
// Fluent Control Drawing Helpers: Card, Toggle, Slider, Buttons with RoundRect
// -----------------------------------------------------------------------------

/// Win11 expander seam: instead of an inset hairline, the expanded card is cut across its full
/// width (borders included) by a strip of the page background, so header and content read as
/// two stacked sub-cards.
unsafe fn draw_expander_seam(hdc: HDC, state: &AppState, x: i32, y: i32, w: i32) {
    let seam_h = scale_px(2, state.dpi).max(2);
    let top = y - seam_h / 2;
    let brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(theme().win_bg));
    // One extra pixel each side also cuts the outline of a hover-enlarged card.
    let g = scale_px(1, state.dpi).max(1);
    let rc = RECT { left: x - g, top, right: x + w + g, bottom: top + seam_h };
    let _ = FillRect(hdc, &rc, brush);
    let _ = DeleteObject(brush);
}

unsafe fn draw_collapsible_header_hover(
    hdc: HDC,
    state: &AppState,
    x: i32,
    card_y: i32,
    w: i32,
    header_h: i32,
    card_h: i32,
    is_expanded_anim: bool,
    is_hovered: bool,
    is_pressed: bool,
    press_anim: f32,
) {
    let th = theme();
    let r = scale_px(12, state.dpi);
    let hover_bg = if is_expanded_anim {
        lerp_color(th.card_bg, th.btn_hover_bg, 0.65)
    } else {
        th.btn_hover_bg
    };
    let press_bg = lerp_color(th.card_bg, th.btn_hover_bg, 0.35);
    let fill_col = if is_pressed {
        let from_col = if is_hovered { hover_bg } else { th.card_bg };
        lerp_color(from_col, press_bg, press_anim.clamp(0.0, 1.0))
    } else {
        hover_bg
    };
    let hov_b = CreateSolidBrush(windows::Win32::Foundation::COLORREF(fill_col));
    let hov_p = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(th.card_border));
    let ob = SelectObject(hdc, hov_b);
    let op = SelectObject(hdc, hov_p);
    if !is_expanded_anim {
        let _ = RoundRect(hdc, x, card_y, x + w, card_y + card_h, r, r);
    } else {
        let saved = windows::Win32::Graphics::Gdi::SaveDC(hdc);
        let rgn = CreateRectRgn(x, card_y, x + w + 1, card_y + header_h);
        let _ = windows::Win32::Graphics::Gdi::ExtSelectClipRgn(hdc, rgn, windows::Win32::Graphics::Gdi::RGN_AND);
        let _ = RoundRect(hdc, x, card_y, x + w, card_y + header_h + r, r, r);
        let _ = windows::Win32::Graphics::Gdi::RestoreDC(hdc, saved);
        let _ = DeleteObject(rgn);
    }
    SelectObject(hdc, ob);
    SelectObject(hdc, op);
    let _ = DeleteObject(hov_b);
    let _ = DeleteObject(hov_p);
}

unsafe fn draw_win11_drop_shadow_layers(hdc: HDC, x: i32, y: i32, w: i32, h: i32, r: i32) {
    let layers = [
        (3, 4, 2, 0x00131112u32),
        (3, 3, 1, 0x000E0C0Eu32),
        (2, 2, 0, 0x00080709u32),
    ];
    for (dx, dy, exp, col) in layers {
        let br = CreateSolidBrush(windows::Win32::Foundation::COLORREF(col));
        let pn = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(col));
        let ob = SelectObject(hdc, br);
        let op = SelectObject(hdc, pn);
        let _ = RoundRect(
            hdc,
            x + dx - exp,
            y + dy - exp,
            x + w + dx + exp,
            y + h + dy + exp,
            r + exp,
            r + exp,
        );
        SelectObject(hdc, ob);
        SelectObject(hdc, op);
        let _ = DeleteObject(br);
        let _ = DeleteObject(pn);
    }
}

/// Height of the network-share rows in the Everything card body (taller while the warning shows).
fn net_share_rows_height(state: &AppState) -> i32 {
    scale_px(if state.settings.allow_network_shares { 140 } else { 116 }, state.dpi)
}

/// Toggle positions of the network-share rows: (switch x, row-1 switch y, row-2 switch y).
/// Same right inset as the toggles of other expander bodies.
fn net_share_toggle_pos(state: &AppState, row_x: i32, rows_y: i32, row_w: i32) -> (i32, i32, i32) {
    let sw_x = row_x + row_w - scale_px(40, state.dpi) - scale_px(52, state.dpi);
    (sw_x, rows_y + scale_px(19, state.dpi), rows_y + scale_px(19 + 58, state.dpi))
}

/// Network-share settings as expander-body rows, separated from the note above and from each
/// other by card seams.
unsafe fn render_net_share_rows(hdc: HDC, state: &AppState, card_x: i32, card_w: i32, row_x: i32, rows_y: i32, row_w: i32) {
    let th = theme();
    let allow = state.settings.allow_network_shares;
    let (sw_x, sw1_y, sw2_y) = net_share_toggle_pos(state, row_x, rows_y, row_w);
    let text_x = row_x + scale_px(16, state.dpi);
    let text_r = sw_x - scale_px(16, state.dpi);
    let line = |top: i32, bottom: i32| RECT { left: text_x, top: rows_y + scale_px(top, state.dpi), right: text_r, bottom: rows_y + scale_px(bottom, state.dpi) };
    let flags = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS;

    draw_expander_seam(hdc, state, card_x, rows_y, card_w);

    // Row 1: allow network shares
    SelectObject(hdc, state.font_regular);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_secondary));
    let mut t1 = to_wide(tr("允許讀取網路共用資料夾", "Allow reading network shares"));
    let mut r1 = line(10, 30);
    let _ = DrawTextW(hdc, &mut t1, &mut r1, flags);
    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.text_dim));
    let mut d1 = to_wide(tr(
        "UNC 路徑與對應的網路磁碟機可直接開檔，Everything 對網路共用的索引結果也會被採用；關閉時只透過檔案總管提供的串流讀取。",
        "Opens files on UNC paths and mapped network drives directly and accepts Everything hits there; when off, only the stream Explorer provides is read.",
    ));
    let mut rd1 = line(30, 50);
    let _ = DrawTextW(hdc, &mut d1, &mut rd1, flags);
    draw_toggle_switch(hdc, state, 15, sw_x, sw1_y, allow, false);

    draw_expander_seam(hdc, state, card_x, rows_y + scale_px(58, state.dpi), card_w);

    // Row 2: LAN only (only adjustable while row 1 is on)
    SelectObject(hdc, state.font_regular);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(if allow { th.text_secondary } else { th.text_disabled }));
    let mut t2 = to_wide(tr("只能讀取區域網路", "Local network only"));
    let mut r2 = line(68, 88);
    let _ = DrawTextW(hdc, &mut t2, &mut r2, flags);
    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(if allow { th.text_dim } else { th.text_disabled }));
    let mut d2 = to_wide(tr(
        "只接受私有 IP、單一名稱（如 NAS）與 .local／.lan 等區域網路名稱的伺服器；不做 DNS 查詢。",
        "Only servers with private IPs, single-label names (e.g. NAS) or .local / .lan style names; no DNS lookups.",
    ));
    let mut rd2 = line(88, 108);
    let _ = DrawTextW(hdc, &mut d2, &mut rd2, flags);
    draw_toggle_switch(hdc, state, 16, sw_x, sw2_y, state.settings.network_lan_only, !allow);

    if allow {
        SelectObject(hdc, state.font_small);
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(th.status_warn));
        let mut wt = to_wide(tr(
            "⚠ 已允許網路讀取：縮圖程序會以您的 Windows 認證連線到這些伺服器，請只對信任的共用資料夾使用。",
            "⚠ Network reads on: the thumbnail host connects to these servers with your Windows credentials. Use only with trusted shares.",
        ));
        let mut wr = RECT { left: text_x, top: rows_y + scale_px(110, state.dpi), right: row_x + row_w - scale_px(16, state.dpi), bottom: rows_y + scale_px(130, state.dpi) };
        let _ = DrawTextW(hdc, &mut wt, &mut wr, flags);
    }
}

/// Click handling for the network-share rows; `true` when the click was consumed.
unsafe fn handle_net_share_click(hwnd: HWND, state: &mut AppState, row_x: i32, rows_y: i32, row_w: i32, x: i32, y: i32) -> bool {
    let (sw_x, sw1_y, sw2_y) = net_share_toggle_pos(state, row_x, rows_y, row_w);
    let sw_w = scale_px(40, state.dpi);
    let row_h = scale_px(20, state.dpi);
    let pad = scale_px(4, state.dpi);
    let hit = |sy: i32| x >= sw_x - pad && x <= sw_x + sw_w + pad && y >= sy - pad && y <= sy + row_h + pad;
    if hit(sw1_y) {
        commit_active_edit(state);
        let enable = !state.settings.allow_network_shares;
        if enable {
            let title = to_wide(tr("允許讀取網路共用資料夾", "Allow reading network shares"));
            let body = to_wide(tr(
                "開啟後，產生縮圖時會直接開啟網路共用資料夾（UNC 路徑或對應的網路磁碟機）中的檔案，並採用 Everything 對網路位置的索引結果。\n\n• 縮圖程序會以您的 Windows 認證連線到檔案所在的伺服器；若伺服器不可信，可能被用來蒐集您的認證資訊。\n• 網路緩慢或伺服器離線時，縮圖產生可能變慢。\n\n建議同時保持「只能讀取區域網路」開啟。確定要開啟嗎？",
                "When enabled, thumbnails open files on network shares (UNC paths or mapped network drives) directly and accept Everything index hits on network locations.\n\n• The thumbnail host connects to the file's server with your Windows credentials; an untrusted server could use this to harvest them.\n• Slow networks or offline servers can slow down thumbnail generation.\n\nKeeping \"Local network only\" on is recommended. Enable network reads?",
            ));
            use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, IDYES, MB_DEFBUTTON2, MB_ICONWARNING, MB_YESNO};
            let answer = MessageBoxW(hwnd, PCWSTR(body.as_ptr()), PCWSTR(title.as_ptr()), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2);
            if answer != IDYES {
                let _ = InvalidateRect(hwnd, None, false);
                return true;
            }
        }
        state.settings.allow_network_shares = enable;
        let _ = state.settings.save();
        trigger_toggle_animation(hwnd, state, 15, enable);
        return true;
    }
    if hit(sw2_y) && state.settings.allow_network_shares {
        commit_active_edit(state);
        state.settings.network_lan_only = !state.settings.network_lan_only;
        let _ = state.settings.save();
        trigger_toggle_animation(hwnd, state, 16, state.settings.network_lan_only);
        return true;
    }
    false
}

unsafe fn draw_card_container(hdc: HDC, state: &AppState, x: i32, y: i32, w: i32, h: i32) {
    let th = theme();
    let r = scale_px(12, state.dpi);
    let bg = CreateSolidBrush(windows::Win32::Foundation::COLORREF(th.card_bg));
    let pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(th.card_border));
    let old_b = SelectObject(hdc, bg);
    let old_p = SelectObject(hdc, pen);
    let _ = windows::Win32::Graphics::Gdi::RoundRect(hdc, x, y, x + w, y + h, r, r);
    SelectObject(hdc, old_b);
    SelectObject(hdc, old_p);
    let _ = DeleteObject(bg);
    let _ = DeleteObject(pen);
}

/// Expander card: while its header is hovered the whole card grows 1 px on every side (on top of
/// the header fill change), so the card itself reads as the interactive surface.
#[allow(clippy::too_many_arguments)]
unsafe fn draw_expander_card_container(hdc: HDC, state: &AppState, x: i32, y: i32, w: i32, h: i32, hovered: bool) {
    let g = if hovered { scale_px(1, state.dpi).max(1) } else { 0 };
    draw_card_container(hdc, state, x - g, y - g, w + g * 2, h + g * 2);
}

unsafe fn draw_sub_card(hdc: HDC, state: &AppState, x: i32, y: i32, w: i32, h: i32) {
    let th = theme();
    let r = scale_px(6, state.dpi);
    let bg = CreateSolidBrush(windows::Win32::Foundation::COLORREF(th.sub_card_bg));
    let pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(th.sub_card_border));
    let old_b = SelectObject(hdc, bg);
    let old_p = SelectObject(hdc, pen);
    let _ = windows::Win32::Graphics::Gdi::RoundRect(hdc, x, y, x + w, y + h, r, r);
    SelectObject(hdc, old_b);
    SelectObject(hdc, old_p);
    let _ = DeleteObject(bg);
    let _ = DeleteObject(pen);
}

unsafe fn draw_divider_line(hdc: HDC, x1: i32, x2: i32, y: i32) {
    let pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(theme().divider));
    let old_p = SelectObject(hdc, pen);
    let _ = windows::Win32::Graphics::Gdi::MoveToEx(hdc, x1, y, None);
    let _ = windows::Win32::Graphics::Gdi::LineTo(hdc, x2, y);
    SelectObject(hdc, old_p);
    let _ = DeleteObject(pen);
}

unsafe fn draw_chevron(hdc: HDC, state: &AppState, cx: i32, cy: i32, is_up: bool, color: u32) {
    let old_f = SelectObject(hdc, state.font_chevron);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(color));
    let ch = if is_up { "\u{E70D}" } else { "\u{E70E}" };
    let mut wch = to_wide(ch);
    let sz = scale_px(16, state.dpi);
    let mut rc = RECT {
        left: cx - sz / 2,
        top: cy - sz / 2,
        right: cx + sz / 2,
        bottom: cy + sz / 2,
    };
    let _ = DrawTextW(hdc, &mut wch, &mut rc, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    SelectObject(hdc, old_f);
}

unsafe fn draw_tracked_checkbox_box(
    hdc: HDC,
    state: &AppState,
    id: u32,
    x: i32,
    y: i32,
    size: i32,
    is_checked: bool,
    is_hovered: bool,
    is_disabled: bool,
) {
    let check_progress = {
        let mut prev_map = state.checkbox_states.borrow_mut();
        let mut anim_map = state.checkbox_anims.borrow_mut();
        let was_checked = prev_map.insert(id, is_checked);
        if was_checked == Some(false) && is_checked {
            anim_map.insert(id, (0.0, std::time::Instant::now()));
            if !state.checkbox_anim_hwnd.0.is_null() {
                let _ = SetTimer(state.checkbox_anim_hwnd, 1, 10, None);
            }
        } else if !is_checked {
            anim_map.remove(&id);
        }
        if is_checked {
            anim_map.get(&id).map(|(p, _)| *p).unwrap_or(1.0)
        } else {
            0.0
        }
    };
    render_aa::draw_aa_checkbox(
        hdc,
        x,
        y,
        size,
        is_checked,
        is_hovered,
        is_disabled,
        check_progress,
    );
}

unsafe fn draw_checkbox(
    hdc: HDC,
    state: &AppState,
    id: u32,
    x: i32,
    y: i32,
    label: &str,
    is_checked: bool,
) {
    let cb_size = scale_px(16, state.dpi);
    SelectObject(hdc, state.font_regular);
    let mut calc_rc = RECT::default();
    let mut lw_calc = to_wide(label);
    let _ = DrawTextW(hdc, &mut lw_calc, &mut calc_rc, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
    let total_w = cb_size + scale_px(10, state.dpi) + (calc_rc.right - calc_rc.left);
    let is_hovered = state.expanded_monitor_item.is_none()
        && state.mouse_pos.0 >= x
        && state.mouse_pos.0 <= x + total_w
        && state.mouse_pos.1 >= y - scale_px(2, state.dpi)
        && state.mouse_pos.1 <= y + cb_size + scale_px(4, state.dpi);

    draw_tracked_checkbox_box(hdc, state, id, x, y, cb_size, is_checked, is_hovered, false);

    SelectObject(hdc, state.font_regular);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(theme().text_secondary));
    let lw = to_wide(label);
    let _ = TextOutW(hdc, x + cb_size + scale_px(8, state.dpi), y, &lw);
}

unsafe fn draw_row_toggle(
    hdc: HDC,
    state: &AppState,
    id: u32,
    x: i32,
    y: i32,
    w: i32,
    label: &str,
    is_on: bool,
    is_disabled: bool,
) {
    let th = theme();
    SelectObject(hdc, state.font_regular);
    SetTextColor(
        hdc,
        if is_disabled {
            windows::Win32::Foundation::COLORREF(th.text_disabled)
        } else {
            windows::Win32::Foundation::COLORREF(th.text_secondary)
        },
    );
    let lw = to_wide(label);
    let _ = TextOutW(hdc, x + scale_px(16, state.dpi), y + scale_px(4, state.dpi), &lw);

    let sw_w = scale_px(40, state.dpi);
    let sw_x = x + w - sw_w - scale_px(52, state.dpi);
    let sw_y = y + scale_px(3, state.dpi);
    draw_toggle_switch(hdc, state, id, sw_x, sw_y, is_on, is_disabled);
}

unsafe fn draw_toggle_switch(
    hdc: HDC,
    state: &AppState,
    id: u32,
    sw_x: i32,
    sw_y: i32,
    is_on: bool,
    is_disabled: bool,
) {
    let sw_w = scale_px(40, state.dpi);
    let sw_h = scale_px(20, state.dpi);

    let anim_val = state.get_toggle_anim(id, is_on);

    let dark = is_dark_theme();
    // Track: smooth anti-aliased pill capsule
    let (track_fill, fill_alpha, track_border, border_alpha, border_w) = if is_disabled {
        if dark {
            (0x00222226, 0.4, 0x00444448, 0.8, 1.0)
        } else {
            (0x00E6E6E6, 0.8, 0x00CCCCCC, 0.9, 1.0)
        }
    } else {
        let off_fill = if dark { 0x00222226 } else { 0x00F5F5F5 };
        let off_border = if dark { 0x009A9A9E } else { 0x00606060 };
        let accent = accent_base_rgb();
        let on_fill = accent;
        let on_border = accent;

        let fill = render_aa::lerp_color_rgb(off_fill, on_fill, anim_val);
        let border = render_aa::lerp_color_rgb(off_border, on_border, anim_val);
        let fa = if dark { 0.5 + 0.5 * anim_val } else { 1.0 };
        (fill, fa, border, 1.0, 1.0)
    };

    render_aa::draw_aa_pill(
        hdc,
        sw_x,
        sw_y,
        sw_w,
        sw_h,
        track_fill,
        fill_alpha,
        track_border,
        border_alpha,
        border_w,
    );

    // Knob: smooth anti-aliased pure circle without any jagged edges
    let r_min = scale_px(5, state.dpi) as f32;
    let r_max = scale_px(6, state.dpi) as f32 + 0.5;

    // Fixed center calculation based on base_r so expansion is strictly centered
    let base_r = r_min + (r_max - r_min) * anim_val;
    let margin = scale_px(4, state.dpi) as f32;
    let travel = (sw_w as f32) - margin * 2.0 - base_r * 2.0;
    let knob_cx = (sw_x as f32 + margin + base_r + travel * anim_val).round() as i32;
    let knob_cy = sw_y + sw_h / 2;

    // Win11 Hover feedback: expands outward by exactly 0.8px centered on (knob_cx, knob_cy)
    let is_hovered = !is_disabled
        && state.mouse_pos.0 >= sw_x - scale_px(3, state.dpi)
        && state.mouse_pos.0 <= sw_x + sw_w + scale_px(3, state.dpi)
        && state.mouse_pos.1 >= sw_y - scale_px(3, state.dpi)
        && state.mouse_pos.1 <= sw_y + sw_h + scale_px(3, state.dpi);
    let hover_grow = if is_hovered { 0.8f32 } else { 0.0f32 };
    let knob_r = base_r + hover_grow;

    let (knob_color, knob_alpha) = if is_disabled {
        if dark { (0x00555558, 0.8) } else { (0x009E9E9E, 0.85) }
    } else {
        let off_knob = if dark { 0x00CECECE } else { 0x00606060 };
        let on_knob = if dark { 0x00222226 } else { accent_contrast_text_rgb() };
        (render_aa::lerp_color_rgb(off_knob, on_knob, anim_val), 1.0)
    };

    render_aa::draw_aa_circle(
        hdc,
        knob_cx,
        knob_cy,
        knob_r,
        knob_color,
        knob_alpha,
        0,
        0.0,
        0.0,
    );
}

unsafe fn draw_risk_checkbox_row(
    hdc: HDC,
    state: &AppState,
    id: u32,
    x: i32,
    y: i32,
    w: i32,
    label: &str,
    is_checked: bool,
    is_disabled: bool,
) {
    let cb_sz = scale_px(18, state.dpi);
    let cb_x = x + scale_px(36, state.dpi); // Indented from card left edge to create clear hierarchy
    let row_h = scale_px(31, state.dpi);
    let cb_y = y + (row_h - cb_sz) / 2;

    let is_hovered = !is_disabled
        && state.expanded_monitor_item.is_none()
        && state.mouse_pos.0 >= x + scale_px(20, state.dpi)
        && state.mouse_pos.0 <= x + w - scale_px(16, state.dpi)
        && state.mouse_pos.1 >= y
        && state.mouse_pos.1 < y + row_h;

    draw_tracked_checkbox_box(
        hdc,
        state,
        id,
        cb_x,
        cb_y,
        cb_sz,
        is_checked,
        is_hovered,
        is_disabled,
    );

    // Label text on the right
    let th = theme();
    SelectObject(hdc, state.font_regular);
    let text_color = if is_disabled {
        th.text_disabled
    } else if is_checked {
        th.text_bright
    } else {
        th.text_secondary
    };
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(text_color));
    let mut lw = to_wide(label);
    let text_x = cb_x + cb_sz + scale_px(12, state.dpi);
    let mut text_rect = RECT {
        left: text_x,
        top: y,
        right: x + w - scale_px(16, state.dpi),
        bottom: y + row_h,
    };
    let _ = DrawTextW(hdc, &mut lw, &mut text_rect, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
}

/// Win11 TextBox chrome: subtle fill and hairline border, a stronger bottom stroke at rest, a
/// lighter fill on hover, and a darker fill with a 2 px accent bottom stroke while focused.
#[allow(clippy::too_many_arguments)]
unsafe fn draw_win11_textbox_frame(hdc: HDC, state: &AppState, x: i32, y: i32, w: i32, h: i32, focused: bool, hovered: bool, disabled: bool) {
    let th = theme();
    let base = th.card_bg;
    let (white, black) = (0x00FFFFFFu32, 0x00000000u32);
    let (fill, side, bottom) = if th.dark {
        let fill = if disabled {
            lerp_color(base, white, 0.03)
        } else if focused {
            lerp_color(base, black, 0.28)
        } else if hovered {
            lerp_color(base, white, 0.09)
        } else {
            lerp_color(base, white, 0.06)
        };
        (fill, lerp_color(base, white, 0.08), lerp_color(base, white, if disabled { 0.08 } else { 0.45 }))
    } else {
        let fill = if disabled {
            lerp_color(base, black, 0.03)
        } else if focused {
            white
        } else if hovered {
            lerp_color(base, black, 0.035)
        } else {
            lerp_color(base, black, 0.015)
        };
        (fill, lerp_color(base, black, 0.07), lerp_color(base, black, if disabled { 0.07 } else { 0.45 }))
    };
    let r = scale_px(8, state.dpi);
    let br = CreateSolidBrush(windows::Win32::Foundation::COLORREF(fill));
    let pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(side));
    let ob = SelectObject(hdc, br);
    let op = SelectObject(hdc, pen);
    let _ = RoundRect(hdc, x, y, x + w, y + h, r, r);
    SelectObject(hdc, ob);
    SelectObject(hdc, op);
    let _ = DeleteObject(br);
    let _ = DeleteObject(pen);

    // Bottom stroke, inset by the corner radius so it follows the rounded outline.
    let (stroke_col, stroke_h) = if focused && !disabled { (accent_base_bgr(), scale_px(2, state.dpi).max(2)) } else { (bottom, 1) };
    let inset = scale_px(3, state.dpi);
    let sb = CreateSolidBrush(windows::Win32::Foundation::COLORREF(stroke_col));
    let rc = RECT { left: x + inset, top: y + h - stroke_h, right: x + w - inset, bottom: y + h };
    let _ = FillRect(hdc, &rc, sb);
    let _ = DeleteObject(sb);
}

/// Width (physical px) reserved at the right of a text box for its clear button.
fn textbox_clear_w(state: &AppState) -> i32 {
    scale_px(24, state.dpi)
}

/// Clear ("X") button inside a focused, non-empty text box: subtle hover / pressed fill. Its
/// rectangle is recorded so the click can be handled before the box's own focus logic.
unsafe fn draw_textbox_clear_button(hdc: HDC, state: &AppState, box_x: i32, box_y: i32, box_w: i32, box_h: i32) {
    let th = theme();
    let pad = scale_px(3, state.dpi);
    let bw = textbox_clear_w(state) - pad;
    let rc = RECT { left: box_x + box_w - pad - bw, top: box_y + pad, right: box_x + box_w - pad, bottom: box_y + box_h - pad };
    state.edit_clear_rect.set(Some(rc));
    let hovered = state.mouse_pos.0 >= rc.left && state.mouse_pos.0 < rc.right && state.mouse_pos.1 >= rc.top && state.mouse_pos.1 < rc.bottom;
    let base = if th.dark { lerp_color(th.card_bg, 0, 0.28) } else { 0x00FFFFFF };
    let fill = if state.edit_clear_pressed {
        Some(if th.dark { lerp_color(base, 0x00FFFFFF, 0.04) } else { lerp_color(base, 0, 0.03) })
    } else if hovered {
        Some(if th.dark { lerp_color(base, 0x00FFFFFF, 0.08) } else { lerp_color(base, 0, 0.06) })
    } else {
        None
    };
    if let Some(col) = fill {
        let br = CreateSolidBrush(windows::Win32::Foundation::COLORREF(col));
        let pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(col));
        let ob = SelectObject(hdc, br);
        let op = SelectObject(hdc, pen);
        let r = scale_px(6, state.dpi);
        let _ = RoundRect(hdc, rc.left, rc.top, rc.right, rc.bottom, r, r);
        SelectObject(hdc, ob);
        SelectObject(hdc, op);
        let _ = DeleteObject(br);
        let _ = DeleteObject(pen);
    }
    let old_f = SelectObject(hdc, state.font_chevron);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(if state.edit_clear_pressed { th.text_muted } else { th.text_secondary }));
    let mut gw = to_wide("\u{E894}");
    let mut grc = rc;
    let _ = DrawTextW(hdc, &mut gw, &mut grc, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
    SelectObject(hdc, old_f);
}

fn text_width(hdc: HDC, text: &str) -> i32 {
    if text.is_empty() {
        return 0;
    }
    let mut sz = windows::Win32::Foundation::SIZE::default();
    let raw: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: valid DC and UTF-16 slice.
    let _ = unsafe { windows::Win32::Graphics::Gdi::GetTextExtentPoint32W(hdc, &raw, &mut sz) };
    sz.cx
}

/// Current value of a field as shown when it gains focus.
fn edit_value_string(state: &AppState, target: EditTarget) -> String {
    let st = &state.settings;
    match target {
        EditTarget::SeekFixedSec => st.seek_fixed_sec.to_string(),
        EditTarget::SeekThresholdMin => st.seek_threshold_min.to_string(),
        EditTarget::StepProbeSec => st.step_probe_sec.to_string(),
        EditTarget::MaxGopHops => st.max_gop_hops.to_string(),
        EditTarget::ConsecutiveLimit => st.consecutive_unacceptable_limit.to_string(),
        EditTarget::WatchdogTimeoutSec => st.watchdog_timeout_sec.to_string(),
        EditTarget::CustomTextExtensions => st.custom_text_extensions.clone(),
    }
}

/// Caret index in `text` closest to screen x inside the box `g`.
unsafe fn edit_hit_index(hwnd: HWND, g: &EditBoxGeom, text: &str, x: i32) -> usize {
    let hdc = windows::Win32::Graphics::Gdi::GetDC(hwnd);
    let old = SelectObject(hdc, g.font);
    let mut best = 0usize;
    let mut best_d = i32::MAX;
    for i in 0..=text.len() {
        if !text.is_char_boundary(i) {
            continue;
        }
        let d = (g.text_left + text_width(hdc, &text[..i]) - x).abs();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    SelectObject(hdc, old);
    let _ = windows::Win32::Graphics::Gdi::ReleaseDC(hwnd, hdc);
    best
}

/// Text of a text box. Focused: the selected range gets an accent highlight with contrast text,
/// otherwise a 1 px caret; nothing is highlighted just because the box has focus.
#[allow(clippy::too_many_arguments)]
unsafe fn draw_edit_text(hdc: HDC, state: &AppState, font: HFONT, text: &str, color: u32, left: i32, right: i32, box_y: i32, box_h: i32, focused: bool) {
    let old = SelectObject(hdc, font);
    let flags = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;
    if !focused {
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(color));
        let mut w = to_wide(text);
        let mut rc = RECT { left, top: box_y, right, bottom: box_y + box_h };
        let _ = DrawTextW(hdc, &mut w, &mut rc, flags | DT_END_ELLIPSIS);
        SelectObject(hdc, old);
        return;
    }
    let (sel_s, sel_e) = state.edit_sel();
    let (sel_s, sel_e) = (sel_s.min(text.len()), sel_e.min(text.len()));
    let x_of = |i: usize| left + text_width(hdc, &text[..i]);
    let draw_seg = |from: usize, to: usize, col: u32| {
        if from >= to {
            return;
        }
        SetTextColor(hdc, windows::Win32::Foundation::COLORREF(col));
        let mut w = to_wide(&text[from..to]);
        let mut rc = RECT { left: x_of(from), top: box_y, right, bottom: box_y + box_h };
        let _ = DrawTextW(hdc, &mut w, &mut rc, flags);
    };
    if sel_s < sel_e {
        let mut sz = windows::Win32::Foundation::SIZE::default();
        let probe: Vec<u16> = "Ag".encode_utf16().collect();
        let _ = windows::Win32::Graphics::Gdi::GetTextExtentPoint32W(hdc, &probe, &mut sz);
        let sel_rc = RECT {
            left: x_of(sel_s),
            top: box_y + (box_h - sz.cy) / 2,
            right: x_of(sel_e).min(right),
            bottom: box_y + (box_h + sz.cy) / 2,
        };
        let br = CreateSolidBrush(windows::Win32::Foundation::COLORREF(accent_base_bgr()));
        let _ = FillRect(hdc, &sel_rc, br);
        let _ = DeleteObject(br);
    }
    draw_seg(0, sel_s, color);
    draw_seg(sel_s, sel_e, accent_contrast_text_bgr());
    draw_seg(sel_e, text.len(), color);
    if sel_s == sel_e {
        let caret = state.edit_fix(state.edit_caret).min(text.len());
        draw_textbox_caret(hdc, state, &text[..caret], left, right, box_y, box_h);
    }
    SelectObject(hdc, old);
}

/// Text-box caret (1 px, `text_primary`) after `text` drawn from `left`.
unsafe fn draw_textbox_caret(hdc: HDC, state: &AppState, text: &str, left: i32, right: i32, box_y: i32, box_h: i32) {
    let mut sz = windows::Win32::Foundation::SIZE::default();
    let raw: Vec<u16> = text.encode_utf16().collect();
    let _ = windows::Win32::Graphics::Gdi::GetTextExtentPoint32W(hdc, &raw, &mut sz);
    let caret_x = (left + sz.cx + 1).min(right - 1);
    let pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(theme().text_primary));
    let op = SelectObject(hdc, pen);
    let inset = (box_h / 4).max(scale_px(5, state.dpi).min(box_h / 3));
    let _ = MoveToEx(hdc, caret_x, box_y + inset, None);
    let _ = LineTo(hdc, caret_x, box_y + box_h - inset);
    SelectObject(hdc, op);
    let _ = DeleteObject(pen);
}

/// Clear button action: empty the field, or restore its default value when it has one.
fn clear_active_edit_to_default(state: &mut AppState) {
    let Some(target) = state.active_edit else { return };
    let d = Settings::default();
    state.edit_buffer = match target {
        EditTarget::SeekFixedSec => d.seek_fixed_sec.to_string(),
        EditTarget::SeekThresholdMin => d.seek_threshold_min.to_string(),
        EditTarget::StepProbeSec => d.step_probe_sec.to_string(),
        EditTarget::MaxGopHops => d.max_gop_hops.to_string(),
        EditTarget::ConsecutiveLimit => d.consecutive_unacceptable_limit.to_string(),
        EditTarget::WatchdogTimeoutSec => d.watchdog_timeout_sec.to_string(),
        EditTarget::CustomTextExtensions => d.custom_text_extensions.clone(),
    };
    state.edit_caret_to_end();
    state.has_unapplied_text_edits = true;
}

unsafe fn draw_row_number_input(
    hdc: HDC,
    state: &AppState,
    target: EditTarget,
    x: i32,
    y: i32,
    _w: i32,
    label: &str,
    val: u32,
    unit: &str,
    is_disabled: bool,
) {
    let label_offset = scale_px(32, state.dpi);
    let th = theme();
    SelectObject(hdc, state.font_regular);
    SetTextColor(
        hdc,
        if is_disabled {
            windows::Win32::Foundation::COLORREF(th.text_disabled)
        } else {
            windows::Win32::Foundation::COLORREF(th.text_secondary)
        },
    );
    let lw = to_wide(label);
    let _ = TextOutW(hdc, x + label_offset, y + scale_px(4, state.dpi), &lw);

    let box_w = scale_px(75, state.dpi);
    let box_h = scale_px(24, state.dpi);
    let box_x = x + scale_px(250, state.dpi);
    let box_y = y + scale_px(2, state.dpi);

    let is_focused = state.active_edit == Some(target);
    let (mx, my) = state.mouse_pos;
    let is_hovered = !is_disabled && mx >= box_x && mx <= box_x + box_w && my >= box_y && my <= box_y + box_h;

    // Win11 TextBox chrome (+ clear button once the focused box has content)
    draw_win11_textbox_frame(hdc, state, box_x, box_y, box_w, box_h, is_focused, is_hovered, is_disabled);
    let show_clear = is_focused && !is_disabled && !state.edit_buffer.is_empty();
    let text_right = box_x + box_w - if show_clear { textbox_clear_w(state) + scale_px(2, state.dpi) } else { scale_px(6, state.dpi) };


    // Text: value, or the edit buffer with caret / mouse selection while focused
    let text_val = if is_focused { state.edit_buffer.clone() } else { val.to_string() };
    let text_left = box_x + scale_px(8, state.dpi);
    if !is_disabled {
        state.edit_boxes.borrow_mut().push(EditBoxGeom {
            target,
            rect: RECT { left: box_x, top: box_y, right: box_x + box_w, bottom: box_y + box_h },
            text_left,
            text_right,
            font: state.font_bold,
        });
    }
    let text_col = if is_disabled { th.text_disabled } else { th.text_bright };
    draw_edit_text(hdc, state, state.font_bold, &text_val, text_col, text_left, text_right, box_y, box_h, is_focused);
    if show_clear {
        draw_textbox_clear_button(hdc, state, box_x, box_y, box_w, box_h);
    }

    // Unit label outside box
    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(if is_disabled { th.text_disabled } else { th.text_muted }));
    let uw = to_wide(unit);
    let _ = TextOutW(hdc, box_x + box_w + scale_px(8, state.dpi), y + scale_px(4, state.dpi), &uw);

    // Stepper buttons [-] and [+]
    let btn_sz = scale_px(22, state.dpi);
    let minus_x = box_x + box_w + scale_px(32, state.dpi);
    let plus_x = minus_x + btn_sz + scale_px(4, state.dpi);
    let btn_y = box_y + (box_h - btn_sz) / 2;

    let (minus_id, plus_id) = match target {
        EditTarget::SeekFixedSec => (360, 361),
        EditTarget::SeekThresholdMin => (370, 371),
        EditTarget::StepProbeSec => (380, 381),
        EditTarget::MaxGopHops => (385, 386),
        EditTarget::ConsecutiveLimit => (390, 391),
        EditTarget::WatchdogTimeoutSec => (400, 401),
        EditTarget::CustomTextExtensions => (0, 0),
    };

    if !is_disabled {
        draw_rounded_button(hdc, state, minus_id, minus_x, btn_y, btn_sz, btn_sz, "-", false, false);
        draw_rounded_button(hdc, state, plus_id, plus_x, btn_y, btn_sz, btn_sz, "+", false, false);
    }
}

unsafe fn draw_row_slider(
    hdc: HDC,
    state: &AppState,
    slider_id: usize,
    x: i32,
    y: i32,
    _w: i32,
    label: &str,
    val: u32,
    unit: &str,
    is_disabled: bool,
    is_indented: bool,
) {
    let label_offset = if is_indented { scale_px(32, state.dpi) } else { scale_px(16, state.dpi) };
    let th = theme();
    SelectObject(hdc, state.font_regular);
    SetTextColor(
        hdc,
        if is_disabled {
            windows::Win32::Foundation::COLORREF(th.text_disabled)
        } else {
            windows::Win32::Foundation::COLORREF(th.text_secondary)
        },
    );
    let lw = to_wide(label);
    let _ = TextOutW(hdc, x + label_offset, y + scale_px(4, state.dpi), &lw);

    let track_x = x + scale_px(250, state.dpi);
    let track_w = scale_px(180, state.dpi);
    let track_y = y + scale_px(12, state.dpi);
    let track_h = scale_px(4, state.dpi).max(3);

    let ratio = (val as f32 / 100.0).clamp(0.0, 1.0);
    let fill_w = ((track_w as f32 * ratio).round() as i32).clamp(track_h, track_w);

    let track_top = track_y - track_h / 2;

    // Determine hover / active state for slider thumb
    let is_active = !is_disabled && (
        state.hover_slider == Some(slider_id)
        || state.dragging_slider == Some(slider_id)
        || (state.mouse_pos.0 >= track_x - scale_px(8, state.dpi)
            && state.mouse_pos.0 <= track_x + track_w + scale_px(8, state.dpi)
            && state.mouse_pos.1 >= track_y - scale_px(10, state.dpi)
            && state.mouse_pos.1 <= track_y + scale_px(10, state.dpi))
    );

    // 1. Unfilled background track: smooth dark rounded capsule
    // Slider thumb uses Win11 soft off-white (#D0D0D0 / #E8E8E8 on hover) instead of harsh pure white
    let (bg_fill, fill_accent, thumb_fill, thumb_border) = if is_disabled {
        if th.dark {
            (0x00333336, 0x00555558, 0x0066666A, 0x002B2B30)
        } else {
            (0x00D8D8DC, 0x00B8B8BE, 0x00E8E8EC, 0x00C0C0C6)
        }
    } else if th.dark {
        (
            0x003D3D44,
            accent_base_rgb(),
            if is_active { 0x00E8E8E8 } else { 0x00D0D0D0 },
            0x00333333,
        )
    } else {
        (
            0x008A8A8E,
            accent_base_rgb(),
            0x00FFFFFF,
            0x00CCCCCC,
        )
    };

    render_aa::draw_aa_pill(
        hdc,
        track_x,
        track_top,
        track_w,
        track_h,
        bg_fill,
        1.0,
        0,
        0.0,
        0.0,
    );

    // 2. Filled track (start to thumb position): Win11 Accent Blue capsule
    if fill_w > track_h / 2 {
        render_aa::draw_aa_pill(
            hdc,
            track_x,
            track_top,
            fill_w,
            track_h,
            fill_accent,
            1.0,
            0,
            0.0,
            0.0,
        );
    }

    // 3. Slider Thumb: Win11 Pure Circular AA Knob
    // When hovered/dragging and not disabled, enlarge by 0.8px from center to show it is adjustable
    let base_r = scale_px(9, state.dpi) as f32;
    let thumb_r = if is_active { base_r + 0.8 } else { base_r };
    let thumb_cx = track_x + (track_w as f32 * ratio).round() as i32;
    let thumb_cy = track_y;

    render_aa::draw_aa_circle(
        hdc,
        thumb_cx,
        thumb_cy,
        thumb_r,
        thumb_fill,
        1.0,
        thumb_border,
        0.8,
        1.0,
    );

    let val_str = format!("[ {}{} ]", val, unit);
    let vw = to_wide(&val_str);
    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(accent_light_bgr()));
    let _ = TextOutW(hdc, track_x + track_w + scale_px(15, state.dpi), y + scale_px(4, state.dpi), &vw);
}

fn lerp_color(c1: u32, c2: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let r1 = (c1 & 0xFF) as f32;
    let g1 = ((c1 >> 8) & 0xFF) as f32;
    let b1 = ((c1 >> 16) & 0xFF) as f32;

    let r2 = (c2 & 0xFF) as f32;
    let g2 = ((c2 >> 8) & 0xFF) as f32;
    let b2 = ((c2 >> 16) & 0xFF) as f32;

    let r = (r1 + (r2 - r1) * t).round() as u32;
    let g = (g1 + (g2 - g1) * t).round() as u32;
    let b = (b1 + (b2 - b1) * t).round() as u32;

    r | (g << 8) | (b << 16)
}

unsafe fn draw_rounded_button(
    hdc: HDC,
    state: &AppState,
    btn_id: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    label: &str,
    is_active: bool,
    is_disabled: bool,
) {
    let r = scale_px(5, state.dpi);
    let is_pressed = !is_disabled && state.pressed_btn == Some(btn_id);
    let is_hovered = !is_disabled && state.mouse_pos.0 >= x && state.mouse_pos.0 <= x + w && state.mouse_pos.1 >= y && state.mouse_pos.1 <= y + h;

    // Win11 Fluent Button Styling:
    // If disabled: dark gray flat background, subdued border, disabled text color, no hover or press reaction.
    // Standard: subtle dark card, fine 1px border.
    // Active (Primary): Windows 11 Accent Blue (#0078D4 / BGR 0x00D47800).
    // Hover: slight brightness lift on background and border.
    // Pressed: background darkens subtly, TEXT NEVER SHIFTS, BORDER NEVER THICKENS.
    let th = theme();
    let (bg_color, border_color, text_color) = if is_disabled {
        (th.btn_disabled_bg, th.btn_disabled_border, th.text_disabled)
    } else if is_active {
        let c_txt = accent_contrast_text_bgr();
        if is_pressed {
            let col = accent_pressed_bgr();
            (col, col, c_txt)
        } else if is_hovered {
            let col = accent_hover_bgr();
            (col, col, c_txt)
        } else {
            let col = accent_base_bgr();
            (col, col, c_txt)
        }
    } else {
        if is_pressed {
            (th.sub_card_bg, th.btn_border, th.text_secondary)
        } else if is_hovered {
            (th.btn_hover_bg, th.btn_hover_border, th.text_bright)
        } else {
            (th.btn_bg, th.btn_border, th.text_primary)
        }
    };

    let draw_y = y;

    let bg = CreateSolidBrush(windows::Win32::Foundation::COLORREF(bg_color));
    let pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(border_color));
    let old_b = SelectObject(hdc, bg);
    let old_p = SelectObject(hdc, pen);
    let _ = windows::Win32::Graphics::Gdi::RoundRect(hdc, x, draw_y, x + w, draw_y + h, r, r);
    SelectObject(hdc, old_b);
    SelectObject(hdc, old_p);
    let _ = DeleteObject(bg);
    let _ = DeleteObject(pen);

    SelectObject(hdc, state.font_regular);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(text_color));
    let mut lw = to_wide(label);
    let mut rect = RECT { left: x, top: draw_y, right: x + w, bottom: draw_y + h };
    let _ = DrawTextW(hdc, &mut lw, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
}

/// Language selector at the bottom of the rail, styled like a navigation item: transparent until
/// hovered / pressed / open, globe centred in the shared icon column, then the language name and
/// an up-chevron (hidden when the rail is collapsed).
unsafe fn draw_language_button(
    hdc: HDC,
    dib: &DibSurface,
    htheme: HTHEME,
    state: &AppState,
    btn_id: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) {
    let th = theme();
    let is_pressed = state.pressed_btn == Some(btn_id) || state.lang_btn_pressed;
    let is_hovered = state.mouse_pos.0 >= x && state.mouse_pos.0 <= x + w && state.mouse_pos.1 >= y && state.mouse_pos.1 <= y + h;

    let pill_a: u8 = if is_pressed {
        if th.dark { 10 } else { 7 }
    } else if is_hovered || state.is_lang_menu_open {
        if th.dark { 16 } else { 11 }
    } else {
        0
    };
    if pill_a > 0 {
        let pill_bgr = if th.dark { 0x00FFFFFF } else { 0x00000000 };
        dib.fill_rounded_translucent(x, y, w, h, scale_px(8, state.dpi), pill_bgr, pill_a, pill_bgr, 0);
    }
    let text_color = if is_pressed {
        if th.dark { 0x00A6A6A6 } else { 0x00666666 }
    } else if th.dark {
        0x00FFFFFF
    } else {
        0x001A1A1A
    };

    SelectObject(hdc, state.font_regular);
    let icon_col = scale_px(RAIL_ICON_COL_W, state.dpi);
    draw_composited_text(
        hdc,
        htheme,
        "🌐",
        RECT { left: x, top: y, right: x + icon_col, bottom: y + h },
        text_color,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );
    if w < scale_px(90, state.dpi) {
        return;
    }

    let cur_lang = current_lang();
    let cur_name = AVAILABLE_LOCALES.iter().find(|l| l.lang == cur_lang).map(|l| l.display_name).unwrap_or("繁體中文");
    draw_composited_text(
        hdc,
        htheme,
        cur_name,
        RECT {
            left: x + icon_col + scale_px(4, state.dpi),
            top: y,
            right: x + w - scale_px(28, state.dpi),
            bottom: y + h,
        },
        text_color,
        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
    );

    // Up-chevron: signals the flyout opens upwards; lifts 2 px while pressed or open.
    let arrow_offset_y = if state.is_lang_menu_open || is_pressed { -scale_px(2, state.dpi) } else { 0 };
    let arrow_cx = x + w - scale_px(16, state.dpi);
    let arrow_cy = y + h / 2 + arrow_offset_y;
    let sz = scale_px(16, state.dpi);
    let arrow_col = if state.is_lang_menu_open { accent_light_bgr() } else { th.text_muted };
    let old_f = SelectObject(hdc, state.font_chevron);
    draw_composited_text(
        hdc,
        htheme,
        "\u{E70D}",
        RECT { left: arrow_cx - sz / 2, top: arrow_cy - sz / 2, right: arrow_cx + sz / 2, bottom: arrow_cy + sz / 2 },
        arrow_col,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );
    SelectObject(hdc, old_f);
}


unsafe fn draw_selectable_button(
    hdc: HDC,
    state: &AppState,
    btn_id: u32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    label: &str,
    is_selected: bool,
    is_disabled: bool,
) {
    let r = scale_px(5, state.dpi);
    let is_pressed = !is_disabled && state.pressed_btn == Some(btn_id);
    let is_hovered = !is_disabled && state.mouse_pos.0 >= x && state.mouse_pos.0 <= x + w && state.mouse_pos.1 >= y && state.mouse_pos.1 <= y + h;

    let th = theme();
    // Windows 11 Fluent Selectable Segment / Tab:
    let (bg_color, border_color, text_color) = if is_disabled {
        if is_selected {
            if th.dark {
                (0x003A322C, 0x004E443C, 0x00908888)
            } else {
                (0x00E4E4E8, 0x00D0D0D6, 0x008A8A8E)
            }
        } else {
            (th.btn_disabled_bg, th.btn_disabled_border, th.text_disabled)
        }
    } else if is_selected {
        let c_txt = accent_contrast_text_bgr();
        if is_pressed {
            let col = accent_pressed_bgr();
            (col, col, c_txt)
        } else if is_hovered {
            let col = accent_hover_bgr();
            (col, col, c_txt)
        } else {
            let col = accent_base_bgr();
            (col, col, c_txt)
        }
    } else {
        if is_pressed {
            (th.sub_card_bg, th.btn_border, th.text_secondary)
        } else if is_hovered {
            (th.sel_unselected_hover_bg, th.btn_hover_border, th.text_bright)
        } else {
            (th.sel_unselected_bg, th.sel_unselected_border, th.sel_unselected_text)
        }
    };

    let draw_y = y;

    let bg = CreateSolidBrush(windows::Win32::Foundation::COLORREF(bg_color));
    let pen = CreatePen(PS_SOLID, 1, windows::Win32::Foundation::COLORREF(border_color));
    let old_b = SelectObject(hdc, bg);
    let old_p = SelectObject(hdc, pen);
    let _ = windows::Win32::Graphics::Gdi::RoundRect(hdc, x, draw_y, x + w, draw_y + h, r, r);
    SelectObject(hdc, old_b);
    SelectObject(hdc, old_p);
    let _ = DeleteObject(bg);
    let _ = DeleteObject(pen);

    SelectObject(hdc, state.font_small);
    SetTextColor(hdc, windows::Win32::Foundation::COLORREF(text_color));
    let mut lw = to_wide(label);
    let mut rect = RECT { left: x, top: draw_y, right: x + w, bottom: draw_y + h };
    let _ = DrawTextW(hdc, &mut lw, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
}

fn scale_px(px: i32, dpi: u32) -> i32 {
    (px * dpi as i32 + 48) / 96
}
