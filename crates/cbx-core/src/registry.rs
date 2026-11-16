use crate::settings::{
    to_wide, APP_REG_KEY, CLSID_CBXSHELL_RS_STR, CLSID_LEGACY_CBXSHELL_STR,
    SHELLEX_EXTRACT_IMAGE, SHELLEX_QUERY_INFO, SHELLEX_THUMBNAIL_PROVIDER,
};
use windows::core::PCWSTR;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteKeyExW, RegDeleteKeyValueW, RegDeleteTreeW, RegEnumValueW, RegGetValueW,
    RegOpenKeyExW, RegSetKeyValueW, HKEY, HKEY_CLASSES_ROOT, HKEY_CURRENT_USER,
    HKEY_LOCAL_MACHINE, KEY_READ, REG_DWORD, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::Shell::{
    IThumbnailCache, IShellItem, SHChangeNotify, SHCreateItemFromParsingName,
    SHCNE_ASSOCCHANGED, SHCNE_UPDATEITEM, SHCNF_FLUSHNOWAIT, SHCNF_IDLIST, SHCNF_PATHW,
    WTS_FORCEEXTRACTION,
};

/// Checks if the current process has elevated administrator privileges.
/// Returns the Windows directory as reported by the OS (`GetSystemWindowsDirectoryW`), falling
/// back to `%SystemRoot%`. Never hard-codes a drive letter.
pub fn system_root_dir() -> std::path::PathBuf {
    let mut buf = [0u16; 520];
    // SAFETY: `buf` is a valid writable UTF-16 buffer.
    let n = unsafe {
        windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW(Some(&mut buf))
    } as usize;
    if n > 0 && n < buf.len() {
        return std::path::PathBuf::from(String::from_utf16_lossy(&buf[..n]));
    }
    std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}

/// Restricts run-time `LoadLibrary` searches of the *current executable* to System32
/// (plus explicitly absolute paths). Call first thing in `main` of every `.exe`.
/// Never call this from the shell-extension DLL: it would change the host process (Explorer).
pub fn harden_process_dll_search() {
    use windows::Win32::System::LibraryLoader::{
        SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_SYSTEM32,
    };
    // SAFETY: plain Win32 call with a constant flag.
    unsafe {
        let _ = SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
    }
}

pub fn is_elevated() -> bool {
    unsafe {
        let mut token = windows::Win32::Foundation::HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_ok() {
            let mut elevation = TOKEN_ELEVATION::default();
            let mut ret_len = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some((&mut elevation as *mut TOKEN_ELEVATION).cast()),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut ret_len,
            );
            let _ = windows::Win32::Foundation::CloseHandle(token);
            if ok.is_ok() {
                return elevation.TokenIsElevated != 0;
            }
        }
    }
    false
}

fn ext_key(ext: &str, slot: &str) -> String {
    format!(r"Software\Classes\{ext}\ShellEx\{slot}")
}

fn sys_assoc_key(ext: &str, slot: &str) -> String {
    format!(r"Software\Classes\SystemFileAssociations\{ext}\ShellEx\{slot}")
}

fn backup_key() -> String {
    format!(r"{APP_REG_KEY}\Backup")
}

pub fn read_reg_sz(root: HKEY, subkey: &str, value_name: Option<&str>) -> Option<String> {
    let subkey_w = to_wide(subkey);
    let val_w = value_name.map(to_wide);
    let val_ptr = val_w
        .as_ref()
        .map(|v| PCWSTR(v.as_ptr()))
        .unwrap_or(PCWSTR::null());
    let mut buf = [0u16; 260];
    let mut cb = (buf.len() * 2) as u32;
    // SAFETY: buf is valid for cb bytes.
    let status = unsafe {
        RegGetValueW(
            root,
            PCWSTR(subkey_w.as_ptr()),
            val_ptr,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut cb),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

pub fn write_reg_sz(root: HKEY, subkey: &str, value_name: Option<&str>, data: &str) -> bool {
    let subkey_w = to_wide(subkey);
    let val_w = value_name.map(to_wide);
    let val_ptr = val_w
        .as_ref()
        .map(|v| PCWSTR(v.as_ptr()))
        .unwrap_or(PCWSTR::null());
    let data_w = to_wide(data);
    // SAFETY: data_w is null-terminated UTF-16.
    unsafe {
        RegSetKeyValueW(
            root,
            PCWSTR(subkey_w.as_ptr()),
            val_ptr,
            REG_SZ.0,
            Some(data_w.as_ptr().cast()),
            (data_w.len() * 2) as u32,
        ) == ERROR_SUCCESS
    }
}

fn is_our_clsid(v: &str) -> bool {
    v.eq_ignore_ascii_case(CLSID_CBXSHELL_RS_STR)
}

fn is_our_or_legacy_clsid(v: &str) -> bool {
    is_our_clsid(v) || v.eq_ignore_ascii_case(CLSID_LEGACY_CBXSHELL_STR)
}

fn delete_if_ours(root: HKEY, path: &str) {
    if let Some(cur) = read_reg_sz(root, path, None) {
        if is_our_or_legacy_clsid(&cur) {
            let path_w = to_wide(path);
            // SAFETY: path_w is null-terminated.
            let _ = unsafe { RegDeleteKeyExW(root, PCWSTR(path_w.as_ptr()), 0, 0) };
        }
    }
}

pub fn is_associated(root: HKEY, ext: &str) -> bool {
    read_reg_sz(root, &ext_key(ext, SHELLEX_THUMBNAIL_PROVIDER), None)
        .map(|v| is_our_clsid(&v))
        .unwrap_or(false)
}

pub fn set_infotip(root: HKEY, ext: &str, enable: bool) {
    let key = ext_key(ext, SHELLEX_QUERY_INFO);
    if enable {
        write_reg_sz(root, &key, None, CLSID_CBXSHELL_RS_STR);
    } else {
        delete_if_ours(root, &key);
    }
}

pub fn enum_reg_value_names(root: HKEY, subkey: &str) -> Vec<String> {
    let subkey_w = to_wide(subkey);
    let mut hkey = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(subkey_w.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        )
    };
    if status != ERROR_SUCCESS {
        return Vec::new();
    }
    let mut names = Vec::new();
    let mut idx = 0u32;
    loop {
        let mut buf = [0u16; 260];
        let mut cch = buf.len() as u32;
        let res = unsafe {
            RegEnumValueW(
                hkey,
                idx,
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut cch,
                None,
                None,
                None,
                None,
            )
        };
        if res != ERROR_SUCCESS {
            break;
        }
        let s = String::from_utf16_lossy(&buf[..cch as usize]).trim().to_string();
        if !s.is_empty() {
            names.push(s);
        }
        idx += 1;
    }
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    names
}

pub fn reg_key_exists(root: HKEY, subkey: &str) -> bool {
    let subkey_w = to_wide(subkey);
    let mut hkey = HKEY::default();
    let status = unsafe { RegOpenKeyExW(root, PCWSTR(subkey_w.as_ptr()), 0, KEY_READ, &mut hkey) };
    if status == ERROR_SUCCESS {
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        true
    } else {
        false
    }
}

pub fn get_progids_for_extension(ext: &str) -> Vec<String> {
    let mut progids = Vec::new();
    let mut push_unique = |s: &str| {
        let t = s.trim();
        if !t.is_empty() && !progids.iter().any(|existing: &String| existing.eq_ignore_ascii_case(t)) {
            progids.push(t.to_string());
        }
    };

    // 1. Windows 11 24H2 UserChoiceLatest\ProgId
    let ucl_key = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\{ext}\UserChoiceLatest\ProgId");
    if let Some(p) = read_reg_sz(HKEY_CURRENT_USER, &ucl_key, Some("ProgId")) {
        push_unique(&p);
    }
    // 2. Classic UserChoice\ProgId
    let user_choice_key = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\{ext}\UserChoice");
    if let Some(p) = read_reg_sz(HKEY_CURRENT_USER, &user_choice_key, Some("ProgId")) {
        push_unique(&p);
    }
    // 3. HKCU\Software\Classes\{ext} (Default)
    if let Some(p) = read_reg_sz(HKEY_CURRENT_USER, &format!(r"Software\Classes\{ext}"), None) {
        push_unique(&p);
    }
    // 4. HKCR\{ext} (Default)
    if let Some(p) = read_reg_sz(HKEY_CLASSES_ROOT, ext, None) {
        push_unique(&p);
    }
    // 5. OpenWithProgids in User FileExts & HKCR
    let owp_user = format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\{ext}\OpenWithProgids");
    for name in enum_reg_value_names(HKEY_CURRENT_USER, &owp_user) {
        push_unique(&name);
    }
    for name in enum_reg_value_names(HKEY_CLASSES_ROOT, &format!(r"{ext}\OpenWithProgids")) {
        push_unique(&name);
    }
    // 6. Standard fallback text ProgIDs (unconditional so HKCU/HKLM split on Win11 is always covered)
    if ext.eq_ignore_ascii_case(".txt")
        || ext.eq_ignore_ascii_case(".md")
        || ext.eq_ignore_ascii_case(".log")
        || ext.eq_ignore_ascii_case(".ini")
    {
        for txt_pid in ["txtfile", "txtfilelegacy"] {
            push_unique(txt_pid);
        }
    }
    progids
}

fn should_suppress_explorer_type_overlay(ext: &str) -> bool {
    !crate::media::is_image_archive_extension(ext)
        && !crate::media::is_video_extension(ext)
        && !crate::media::is_audio_extension(ext)
}

pub fn associate_extension(root: HKEY, ext: &str, enable_infotip: bool) {
    let thumb_key = ext_key(ext, SHELLEX_THUMBNAIL_PROVIDER);
    if let Some(prev) = read_reg_sz(root, &thumb_key, None) {
        if !prev.is_empty() && !is_our_clsid(&prev) {
            write_reg_sz(HKEY_CURRENT_USER, &backup_key(), Some(ext), &prev);
        }
    }
    write_reg_sz(root, &thumb_key, None, CLSID_CBXSHELL_RS_STR);
    write_reg_sz(
        root,
        &sys_assoc_key(ext, SHELLEX_THUMBNAIL_PROVIDER),
        None,
        CLSID_CBXSHELL_RS_STR,
    );
    if ext.eq_ignore_ascii_case(".txt") {
        write_reg_sz(
            root,
            &sys_assoc_key("text", SHELLEX_THUMBNAIL_PROVIDER),
            None,
            CLSID_CBXSHELL_RS_STR,
        );
    }
    set_infotip(root, ext, enable_infotip);
    // Clean up legacy IExtractImage slot if it pointed to old CBXShell
    delete_if_ours(root, &ext_key(ext, SHELLEX_EXTRACT_IMAGE));

    // When CBXShell renders its own high-res opening-app corner icon (E-books & Documents/Other),
    // suppress Explorer's duplicate small TypeOverlay icon so icons never appear twice.
    let suppress_overlay = should_suppress_explorer_type_overlay(ext);
    if suppress_overlay {
        write_reg_sz(root, &format!(r"Software\Classes\{ext}"), Some("TypeOverlay"), "");
        write_reg_sz(
            root,
            &format!(r"Software\Classes\SystemFileAssociations\{ext}"),
            Some("TypeOverlay"),
            "",
        );
    }

    // Register under any active 3rd-party ProgIDs (e.g. mpc-be64.mp4, VLC.mp4, Word.Document.12, AppX...)
    // so associated apps don't suppress thumbnails with their generic file icons.
    for progid in get_progids_for_extension(ext) {
        let progid_thumb_key = format!(r"Software\Classes\{progid}\ShellEx\{SHELLEX_THUMBNAIL_PROVIDER}");
        write_reg_sz(root, &progid_thumb_key, None, CLSID_CBXSHELL_RS_STR);
        if suppress_overlay {
            write_reg_sz(root, &format!(r"Software\Classes\{progid}"), Some("TypeOverlay"), "");
        }
    }
}

pub fn disassociate_extension(root: HKEY, ext: &str) {
    let thumb_key = ext_key(ext, SHELLEX_THUMBNAIL_PROVIDER);
    delete_if_ours(root, &thumb_key);
    delete_if_ours(root, &sys_assoc_key(ext, SHELLEX_THUMBNAIL_PROVIDER));
    if ext.eq_ignore_ascii_case(".txt") {
        delete_if_ours(root, &sys_assoc_key("text", SHELLEX_THUMBNAIL_PROVIDER));
    }
    set_infotip(root, ext, false);
    delete_if_ours(root, &ext_key(ext, SHELLEX_EXTRACT_IMAGE));

    let overlay_w = to_wide("TypeOverlay");
    let del_empty_overlay = |subkey: &str| {
        if let Some(val) = read_reg_sz(root, subkey, Some("TypeOverlay")) {
            if val.is_empty() {
                let sk_w = to_wide(subkey);
                unsafe {
                    let _ = RegDeleteKeyValueW(root, PCWSTR(sk_w.as_ptr()), PCWSTR(overlay_w.as_ptr()));
                }
            }
        }
    };
    del_empty_overlay(&format!(r"Software\Classes\{ext}"));
    del_empty_overlay(&format!(r"Software\Classes\SystemFileAssociations\{ext}"));

    for progid in get_progids_for_extension(ext) {
        let progid_thumb_key = format!(r"Software\Classes\{progid}\ShellEx\{SHELLEX_THUMBNAIL_PROVIDER}");
        delete_if_ours(root, &progid_thumb_key);
        del_empty_overlay(&format!(r"Software\Classes\{progid}"));
    }

    if let Some(prev) = read_reg_sz(HKEY_CURRENT_USER, &backup_key(), Some(ext)) {
        if !prev.is_empty() {
            write_reg_sz(root, &thumb_key, None, &prev);
        }
        let bk_w = to_wide(&backup_key());
        let ext_w = to_wide(ext);
        // SAFETY: null-terminated wide strings.
        let _ = unsafe {
            RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                PCWSTR(bk_w.as_ptr()),
                PCWSTR(ext_w.as_ptr()),
            )
        };
    }
}

/// Configures the thumbnail execution mode (Safe Process Isolation vs In-Process loading).
/// If `sandbox_mode` is true: sets DisableProcessIsolation = 0 and registers AppID with DllSurrogate = "".
/// If `sandbox_mode` is false: sets DisableProcessIsolation = 1 and removes AppID / DllSurrogate.
pub fn apply_execution_mode(sandbox_mode: bool) {
    let appid_key = format!(r"Software\Classes\AppID\{CLSID_CBXSHELL_RS_STR}");
    let clsid_key = format!(r"Software\Classes\CLSID\{CLSID_CBXSHELL_RS_STR}");
    let clsid_w = to_wide(&clsid_key);
    let dpi_w = to_wide("DisableProcessIsolation");
    let appid_w = to_wide(&appid_key);
    let appid_val_w = to_wide("AppID");

    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        let inproc_key = format!(r"{clsid_key}\InprocServer32");
        if read_reg_sz(root, &inproc_key, None).is_some() {
            write_reg_sz(root, &inproc_key, Some("ThreadingModel"), "Free");
        }
        if sandbox_mode {
            let zero: u32 = 0;
            unsafe {
                let _ = RegSetKeyValueW(
                    root,
                    PCWSTR(clsid_w.as_ptr()),
                    PCWSTR(dpi_w.as_ptr()),
                    REG_DWORD.0,
                    Some((&zero as *const u32).cast()),
                    4,
                );
            }
            write_reg_sz(root, &clsid_key, Some("AppID"), CLSID_CBXSHELL_RS_STR);
            write_reg_sz(root, &appid_key, None, "cbxshell-rs Thumbnail Provider");
            write_reg_sz(root, &appid_key, Some("DllSurrogate"), "");
        } else {
            let one: u32 = 1;
            unsafe {
                let _ = RegSetKeyValueW(
                    root,
                    PCWSTR(clsid_w.as_ptr()),
                    PCWSTR(dpi_w.as_ptr()),
                    REG_DWORD.0,
                    Some((&one as *const u32).cast()),
                    4,
                );
                let _ = RegDeleteTreeW(root, PCWSTR(appid_w.as_ptr()));
                let _ = RegDeleteKeyValueW(root, PCWSTR(clsid_w.as_ptr()), PCWSTR(appid_val_w.as_ptr()));
            }
        }
    }
    notify_shell_associations_changed();
}

pub fn notify_shell_associations_changed() {
    // SAFETY: standard Explorer notification with null item lists.
    unsafe {
        SHChangeNotify(
            SHCNE_ASSOCCHANGED,
            SHCNF_IDLIST | SHCNF_FLUSHNOWAIT,
            None,
            None,
        );
    }
}

/// Forces Windows Explorer to re-extract and update the cached thumbnail for a single file
/// without clearing the entire system thumbnail cache.
pub fn update_single_file_shell_thumbnail(path: &std::path::Path) -> Result<(), String> {
    use windows::core::{GUID, PCWSTR};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };

    if !path.exists() {
        return Err(crate::i18n::t("error.target_not_found"));
    }

    let wide = to_wide(&path.to_string_lossy());
    unsafe {
        let hr_init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let should_uninit = hr_init.is_ok();

        let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(wide.as_ptr()), None)
            .map_err(|e| crate::i18n::t_fmt("error.sh_create_item_failed", &[("err", &e.to_string())]))?;

        // CLSID_LocalThumbnailCache = {50ef4544-ac9f-4a8e-b21b-8a26180db13f}
        let clsid_local_cache = GUID::from_u128(0x50ef4544_ac9f_4a8e_b21b_8a26180db13f);
        if let Ok(cache) =
            CoCreateInstance::<_, IThumbnailCache>(&clsid_local_cache, None, CLSCTX_INPROC_SERVER)
        {
            for size in [256u32, 96u32] {
                let mut bmp = None;
                let mut flags = Default::default();
                let mut id = Default::default();
                let _ = cache.GetThumbnail(
                    &item,
                    size,
                    WTS_FORCEEXTRACTION,
                    Some(&mut bmp),
                    Some(&mut flags),
                    Some(&mut id),
                );
            }
        }

        SHChangeNotify(
            SHCNE_UPDATEITEM,
            SHCNF_PATHW | SHCNF_FLUSHNOWAIT,
            Some(wide.as_ptr().cast()),
            None,
        );

        if should_uninit {
            CoUninitialize();
        }
    }
    Ok(())
}

/// Clears Windows Explorer thumbnail cache databases (`%LocalAppData%\Microsoft\Windows\Explorer\thumbcache_*.db`)
/// and notifies the Windows Shell to refresh open folder views.
/// Returns `(files_deleted, bytes_freed)`.
pub fn clear_thumbnail_cache() -> (usize, u64) {
    let mut deleted_files = 0usize;
    let mut freed_bytes = 0u64;

    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        let explorer_cache_dir = std::path::Path::new(&local_app_data)
            .join("Microsoft")
            .join("Windows")
            .join("Explorer");

        let mut locked_found = false;
        if let Ok(entries) = std::fs::read_dir(&explorer_cache_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                let lower = fname.to_ascii_lowercase();
                if (lower.starts_with("thumbcache_") || lower.starts_with("iconcache_"))
                    && lower.ends_with(".db")
                {
                    let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    if std::fs::remove_file(&path).is_ok() {
                        deleted_files += 1;
                        freed_bytes = freed_bytes.saturating_add(len);
                    } else {
                        locked_found = true;
                    }
                }
            }
        }

        // If explorer has locked active cache files, restart explorer to release locks and purge completely
        if locked_found {
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                const CREATE_NO_WINDOW: u32 = 0x08000000;
                let sys_root = system_root_dir();
                let sys32 = sys_root.join("System32");
                let taskkill_exe = sys32.join("taskkill.exe");
                let explorer_exe = sys_root.join("explorer.exe");
                let _ = std::process::Command::new(&taskkill_exe)
                    .args(["/f", "/im", "explorer.exe"])
                    .current_dir(&sys32)
                    .creation_flags(CREATE_NO_WINDOW)
                    .output();
                std::thread::sleep(std::time::Duration::from_millis(250));
                if let Ok(entries) = std::fs::read_dir(&explorer_cache_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        let Some(fname) = path.file_name().and_then(|s| s.to_str()) else {
                            continue;
                        };
                        let lower = fname.to_ascii_lowercase();
                        if (lower.starts_with("thumbcache_") || lower.starts_with("iconcache_"))
                            && lower.ends_with(".db")
                        {
                            let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
                            if std::fs::remove_file(&path).is_ok() {
                                deleted_files += 1;
                                freed_bytes = freed_bytes.saturating_add(len);
                            }
                        }
                    }
                }
                let _ = std::process::Command::new(&explorer_exe)
                    .current_dir(&sys_root)
                    .spawn();
            }
        }
    }

    notify_shell_associations_changed();
    (deleted_files, freed_bytes)
}

/// Repairs and re-registers all supported media extensions to cbxshell-rs.
/// Returns number of repaired associations.
pub fn repair_all_associations(_root: HKEY, formats: &[&str], enable_infotip: bool) -> usize {
    let mut repaired = 0;
    for &ext in formats {
        associate_extension(HKEY_CURRENT_USER, ext, enable_infotip);
        associate_extension(HKEY_LOCAL_MACHINE, ext, enable_infotip);
        repaired += 1;
    }
    notify_shell_associations_changed();
    repaired
}

/// Pre-fills / warms up thumbnail cache for media files in a directory.
/// Calls `callback(current_file_name, current_index, total_files)`.
pub fn prefill_cache_directory<F: FnMut(&str, usize, usize)>(
    dir: &std::path::Path,
    recursive: bool,
    mut callback: F,
) -> usize {
    let mut files = Vec::new();
    collect_media_files(dir, recursive, &mut files);
    let total = files.len();
    let settings = crate::settings::Settings::load();

    for (idx, path) in files.iter().enumerate() {
        if let Some(fname) = path.file_name().and_then(|s| s.to_str()) {
            callback(fname, idx + 1, total);
        }
        let ext = path.extension().and_then(|e| e.to_str()).map(|e| format!(".{}", e.to_ascii_lowercase())).unwrap_or_default();
        if crate::media::is_video_extension(&ext) {
            let _ = crate::media::video_mf::extract_video_thumbnail_from_path(path, 256, &settings.smart_render_config());
        }
    }
    total
}

fn collect_media_files(dir: &std::path::Path, recursive: bool, list: &mut Vec<std::path::PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() && recursive {
                collect_media_files(&p, recursive, list);
            } else if p.is_file() {
                if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                    let ext_str = format!(".{}", ext.to_ascii_lowercase());
                    if crate::media::is_video_extension(&ext_str) || crate::media::is_audio_extension(&ext_str) || crate::media::is_image_archive_extension(&ext_str) {
                        list.push(p);
                    }
                }
            }
        }
    }
}
