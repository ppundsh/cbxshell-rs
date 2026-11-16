#![allow(non_snake_case)]

use cbx_core::registry::{
    associate_extension, disassociate_extension, notify_shell_associations_changed,
    write_reg_sz,
};
use cbx_core::settings::{
    to_wide, Settings, CLSID_CBXSHELL_RS, CLSID_CBXSHELL_RS_STR,
};
use cbx_core::{build_infotip, create_universal_thumbnail};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, AtomicU8, AtomicUsize, Ordering};
use std::sync::Mutex;
use windows::core::{
    implement, interface, ComObject, Error, IUnknownImpl, Interface, Result, GUID, HRESULT, PCWSTR, PWSTR,
};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_INVALIDARG, E_NOTIMPL, E_POINTER,
    E_UNEXPECTED, ERROR_ALREADY_INITIALIZED, HINSTANCE, HMODULE, MAX_PATH, S_FALSE, S_OK,
};
use windows::Win32::Graphics::Gdi::HBITMAP;
use windows::Win32::System::Com::{
    CoInitializeEx, CoTaskMemAlloc, CoTaskMemFree, CoUninitialize, IClassFactory,
    IClassFactory_Impl, IPersistFile, IPersistFile_Impl, IPersist_Impl, IStream,
    COINIT_MULTITHREADED, STATFLAG_DEFAULT, STATSTG, STGM,
};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegDeleteTreeW, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE,
};
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::Win32::UI::Shell::PropertiesSystem::{
    IInitializeWithFile, IInitializeWithFile_Impl, IInitializeWithStream,
    IInitializeWithStream_Impl,
};
use windows::Win32::UI::Shell::{
    IThumbnailProvider, IThumbnailProvider_Impl, IShellItem, SIGDN_FILESYSPATH,
    WTSAT_ARGB, WTSAT_UNKNOWN, WTS_ALPHATYPE,
};

#[interface("7f73803c-4479-4045-9a77-8860b1275a14")]
unsafe trait IInitializeWithItem: windows::core::IUnknown {
    unsafe fn Initialize(&self, psi: Option<&IShellItem>, grfmode: u32) -> HRESULT;
}

// Define IQueryInfo directly so we don't depend on unstable Win32_UI_Shell feature flags.
#[interface("00021500-0000-0000-C000-000000000046")]
unsafe trait IQueryInfo: windows::core::IUnknown {
    unsafe fn GetInfoTip(&self, dwflags: u32, ppwsztip: *mut PWSTR) -> HRESULT;
    unsafe fn GetInfoFlags(&self, pdwflags: *mut u32) -> HRESULT;
}

static MODULE_HANDLE: AtomicUsize = AtomicUsize::new(0);
static ACTIVE_OBJECTS: AtomicI32 = AtomicI32::new(0);

struct ObjectCountGuard;
impl ObjectCountGuard {
    fn new() -> Self {
        ACTIVE_OBJECTS.fetch_add(1, Ordering::SeqCst);
        Self
    }
}
impl Drop for ObjectCountGuard {
    fn drop(&mut self) {
        ACTIVE_OBJECTS.fetch_sub(1, Ordering::SeqCst);
    }
}

const PIPELINE_STREAM_RPC: u8 = 0;
const PIPELINE_DIRECT_FILE: u8 = 1;
const PIPELINE_IPC_HIT: u8 = 2;
const PIPELINE_SEARCH_HIT: u8 = 3;
const PIPELINE_SEARCH_LAST_DIR: u8 = 10;
const PIPELINE_SEARCH_CWD: u8 = 11;
const PIPELINE_SEARCH_TYPED_PATHS: u8 = 12;
const PIPELINE_SEARCH_RECENT: u8 = 13;
const PIPELINE_SEARCH_USER_FOLDERS: u8 = 14;
const PIPELINE_SEARCH_DRIVE_ROOT: u8 = 15;

fn pipeline_label(kind: u8) -> String {
    let key = match kind {
        PIPELINE_DIRECT_FILE => "pipeline.direct_file",
        PIPELINE_IPC_HIT => "pipeline.ipc_hit",
        PIPELINE_SEARCH_HIT
        | PIPELINE_SEARCH_LAST_DIR
        | PIPELINE_SEARCH_CWD
        | PIPELINE_SEARCH_TYPED_PATHS
        | PIPELINE_SEARCH_RECENT
        | PIPELINE_SEARCH_USER_FOLDERS
        | PIPELINE_SEARCH_DRIVE_ROOT => "pipeline.search_hit",
        _ => "pipeline.stream_rpc",
    };
    cbx_core::i18n::t(key)
}

fn resolve_trace_label(kind: u8) -> String {
    use cbx_core::media::ResolveSource;
    match kind {
        PIPELINE_DIRECT_FILE => ResolveSource::DirectPath.trace_description(),
        PIPELINE_IPC_HIT => ResolveSource::EverythingIpc.trace_description(),
        PIPELINE_SEARCH_LAST_DIR => ResolveSource::LastDirCache.trace_description(),
        PIPELINE_SEARCH_CWD => ResolveSource::WorkingDir.trace_description(),
        PIPELINE_SEARCH_TYPED_PATHS => ResolveSource::TypedPaths.trace_description(),
        PIPELINE_SEARCH_RECENT => ResolveSource::RecentHistory.trace_description(),
        PIPELINE_SEARCH_USER_FOLDERS => ResolveSource::UserFolders.trace_description(),
        PIPELINE_SEARCH_DRIVE_ROOT => ResolveSource::DriveRoot.trace_description(),
        PIPELINE_SEARCH_HIT => cbx_core::i18n::t("trace.search_hit_local"),
        _ => cbx_core::i18n::t("trace.com_stream"),
    }
}

#[implement(IInitializeWithFile, IInitializeWithItem, IInitializeWithStream, IThumbnailProvider, IPersistFile, IQueryInfo)]
struct CbxShellProvider {
    _guard: ObjectCountGuard,
    stream: Mutex<Option<IStream>>,
    file_path: Mutex<Option<PathBuf>>,
    pipeline_kind: AtomicU8,
    settings: Settings,
}

impl CbxShellProvider {
    fn new() -> Self {
        Self {
            _guard: ObjectCountGuard::new(),
            stream: Mutex::new(None),
            file_path: Mutex::new(None),
            pipeline_kind: AtomicU8::new(PIPELINE_STREAM_RPC),
            settings: Settings::load(),
        }
    }
}

impl IInitializeWithFile_Impl for CbxShellProvider_Impl {
    fn Initialize(&self, pszfilepath: &PCWSTR, _grfmode: u32) -> Result<()> {
        if pszfilepath.is_null() {
            return Err(Error::from(E_INVALIDARG));
        }
        let path_str = unsafe { pszfilepath.to_string() }.map_err(|_| Error::from(E_INVALIDARG))?;
        if let Ok(mut fp) = self.file_path.lock() {
            *fp = Some(PathBuf::from(path_str));
            self.pipeline_kind.store(PIPELINE_DIRECT_FILE, Ordering::SeqCst);
        }
        Ok(())
    }
}

impl IInitializeWithItem_Impl for CbxShellProvider_Impl {
    unsafe fn Initialize(&self, psi: Option<&IShellItem>, _grfmode: u32) -> HRESULT {
        let Some(item) = psi else {
            return E_INVALIDARG;
        };
        if let Ok(name_pwstr) = item.GetDisplayName(SIGDN_FILESYSPATH) {
            if let Ok(path_str) = name_pwstr.to_string() {
                if !path_str.is_empty() {
                    if let Ok(mut fp) = self.file_path.lock() {
                        *fp = Some(PathBuf::from(path_str));
                        self.pipeline_kind.store(PIPELINE_DIRECT_FILE, Ordering::SeqCst);
                    }
                }
            }
            CoTaskMemFree(Some(name_pwstr.0.cast()));
        }
        S_OK
    }
}

impl IInitializeWithStream_Impl for CbxShellProvider_Impl {
    fn Initialize(&self, pstream: Option<&IStream>, _grfmode: u32) -> Result<()> {
        let Some(s) = pstream else {
            return Err(Error::from(E_INVALIDARG));
        };
        let mut guard = self.stream.lock().map_err(|_| Error::from(E_FAIL))?;
        if guard.is_some() {
            return Err(Error::from(HRESULT::from_win32(ERROR_ALREADY_INITIALIZED.0)));
        }
        *guard = Some(s.clone());
        drop(guard);

        // Query stream STATSTG name so we know the exact file extension (.cbz, .zip, .cbr, .rar, .cb7, .7z, .cbt, .tar, .epub)
        unsafe {
            let mut stat = STATSTG::default();
            if s.Stat(&mut stat, STATFLAG_DEFAULT).is_ok() && !stat.pwcsName.is_null() {
                if let Ok(name_str) = stat.pwcsName.to_string() {
                    if !name_str.is_empty() {
                        let path = PathBuf::from(&name_str);
                        let ext = path.extension()
                            .and_then(|e| e.to_str())
                            .map(|e| format!(".{}", e.to_ascii_lowercase()))
                            .unwrap_or_default();

                        // Architectural Optimization:
                        // Image archives (ZIP, RAR, 7Z, TAR, EPUB) and Audio files have NO GPU/hardware-decoding requirements.
                        // They stream-decode directly via ComStreamReader in memory without disk seeks.
                        // ONLY video files benefit from native URL hardware pipelines (Direct3D 11 / DXVA).
                        // Therefore, skip path resolution entirely for non-video files to achieve instant 0ms handshake!
                        let is_video = cbx_core::media::is_video_extension(&ext);

                        // STATSTG names are often bare file names (or names chosen by a third-party
                        // namespace extension). Only an absolute local path may be opened directly;
                        // anything else is used purely as an extension hint for the stream pipeline.
                        let (resolved, kind) = if cbx_core::media::is_trusted_local_abs_path(&path) && path.is_file() {
                            (Some(path), PIPELINE_DIRECT_FILE)
                        } else if is_video {
                            if let Some((p, src)) = cbx_core::media::resolve_file_from_name_with_source(&name_str, stat.cbSize) {
                                let k = match src {
                                    cbx_core::media::ResolveSource::DirectPath => PIPELINE_DIRECT_FILE,
                                    cbx_core::media::ResolveSource::EverythingIpc => PIPELINE_IPC_HIT,
                                    cbx_core::media::ResolveSource::LastDirCache => PIPELINE_SEARCH_LAST_DIR,
                                    cbx_core::media::ResolveSource::WorkingDir => PIPELINE_SEARCH_CWD,
                                    cbx_core::media::ResolveSource::TypedPaths => PIPELINE_SEARCH_TYPED_PATHS,
                                    cbx_core::media::ResolveSource::RecentHistory => PIPELINE_SEARCH_RECENT,
                                    cbx_core::media::ResolveSource::UserFolders => PIPELINE_SEARCH_USER_FOLDERS,
                                    cbx_core::media::ResolveSource::DriveRoot => PIPELINE_SEARCH_DRIVE_ROOT,
                                };
                                (Some(p), k)
                            } else {
                                (Some(path), PIPELINE_STREAM_RPC)
                            }
                        } else {
                            // Non-video files (Archives, Ebook, Audio, Other): instant 0ms return, no disk scanning!
                            (Some(path), PIPELINE_STREAM_RPC)
                        };
                        if let Ok(mut fp) = self.file_path.lock() {
                            if fp.is_none() {
                                *fp = resolved;
                                self.pipeline_kind.store(kind, Ordering::SeqCst);
                            }
                        }
                    }
                }
                CoTaskMemFree(Some(stat.pwcsName.0.cast()));
            }

            // If possible, recover absolute file system path via IPersistFile
            if let Ok(pf) = s.cast::<IPersistFile>() {
                if let Ok(cur) = pf.GetCurFile() {
                    if !cur.is_null() {
                        if let Ok(full) = cur.to_string() {
                            if !full.is_empty()
                                && cbx_core::media::is_trusted_local_abs_path(Path::new(&full))
                                && Path::new(&full).is_file()
                            {
                                if let Ok(mut fp) = self.file_path.lock() {
                                    *fp = Some(PathBuf::from(full));
                                    self.pipeline_kind.store(PIPELINE_DIRECT_FILE, Ordering::SeqCst);
                                }
                            }
                        }
                        CoTaskMemFree(Some(cur.0.cast()));
                    }
                }
            }
        }
        Ok(())
    }
}

struct DecodedImagePayload {
    width: u32,
    height: u32,
    bgra: Vec<u8>,
    pipeline: String,
    search_trace: String,
    details: String,
}

fn hbitmap_to_bgra(h: HBITMAP) -> Result<(u32, u32, Vec<u8>)> {
    let mut bmp_info = windows::Win32::Graphics::Gdi::BITMAP::default();
    unsafe {
        windows::Win32::Graphics::Gdi::GetObjectW(
            h,
            std::mem::size_of::<windows::Win32::Graphics::Gdi::BITMAP>() as i32,
            Some(&mut bmp_info as *mut _ as *mut _),
        );
        let w = bmp_info.bmWidth as u32;
        let h_px = (bmp_info.bmHeight.abs()) as u32;
        let mut buf = vec![0u8; (w * h_px * 4) as usize];
        let hdc = windows::Win32::Graphics::Gdi::CreateCompatibleDC(None);
        let old_b = windows::Win32::Graphics::Gdi::SelectObject(hdc, h);
        let mut bi = windows::Win32::Graphics::Gdi::BITMAPINFO {
            bmiHeader: windows::Win32::Graphics::Gdi::BITMAPINFOHEADER {
                biSize: std::mem::size_of::<windows::Win32::Graphics::Gdi::BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h_px as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0,
                ..Default::default()
            },
            ..Default::default()
        };
        let _ = windows::Win32::Graphics::Gdi::GetDIBits(
            hdc, h, 0, h_px, Some(buf.as_mut_ptr() as *mut _), &mut bi,
            windows::Win32::Graphics::Gdi::DIB_RGB_COLORS,
        );
        windows::Win32::Graphics::Gdi::SelectObject(hdc, old_b);
        let _ = windows::Win32::Graphics::Gdi::DeleteDC(hdc);
        let _ = windows::Win32::Graphics::Gdi::DeleteObject(h);
        Ok((w, h_px, buf))
    }
}

impl IThumbnailProvider_Impl for CbxShellProvider_Impl {
    fn GetThumbnail(
        &self,
        cx: u32,
        phbmp: *mut HBITMAP,
        pdwalpha: *mut WTS_ALPHATYPE,
    ) -> Result<()> {
        if phbmp.is_null() || pdwalpha.is_null() {
            return Err(Error::from(E_POINTER));
        }
        // SAFETY: caller-supplied non-null out-pointers.
        unsafe {
            *phbmp = HBITMAP::default();
            *pdwalpha = WTSAT_UNKNOWN;
        }
        // Any in-process client can call GetThumbnail with an arbitrary size. Clamp it so the
        // requested edge can never drive an oversized (or overflowing) bitmap allocation.
        let cx = cx.clamp(16, 2560);

        let (fname, ext) = {
            let path_opt = self.file_path.lock().ok().and_then(|g| g.clone());
            if let Some(ref path) = path_opt {
                let full_str = path.to_string_lossy().to_string();
                let ext = path.extension().and_then(|e| e.to_str()).map(|e| format!(".{}", e.to_ascii_lowercase())).unwrap_or_default();
                (full_str, ext)
            } else {
                ("Stream".to_string(), ".data".to_string())
            }
        };

        let task_guard = cbx_core::monitor::record_task_begin(&fname, &ext);

        let path_opt = self.file_path.lock().ok().and_then(|g| g.clone());
        let stream_opt = self.stream.lock().ok().and_then(|g| g.clone());
        let settings = self.settings.clone();
        let ext_clone = ext.clone();

        // 12-Second Hard Timeout Watchdog
        // Guarantees explorer.exe / the dllhost.exe COM surrogate never hangs indefinitely
        let (tx, rx) = std::sync::mpsc::channel();
        let worker_path = path_opt.clone();
        let _worker_ext = ext_clone.clone();
        let worker_settings = settings.clone();
        let p_kind = self.pipeline_kind.load(Ordering::SeqCst);

        // Pass raw COM pointer value safely to worker thread
        let stream_raw_val = stream_opt.as_ref().map(|s| s.as_raw() as usize);

        std::thread::spawn(move || {
            struct ComMtaGuard(bool);
            impl Drop for ComMtaGuard {
                fn drop(&mut self) {
                    if self.0 {
                        unsafe { CoUninitialize(); }
                    }
                }
            }
            let _com_guard = unsafe { ComMtaGuard(CoInitializeEx(None, COINIT_MULTITHREADED).is_ok()) };

            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<DecodedImagePayload> {
                let worker_stream: Option<IStream> = stream_raw_val.map(|ptr| unsafe {
                    let mut s: Option<IStream> = None;
                    let p_unk = ptr as *mut std::ffi::c_void;
                    if !p_unk.is_null() {
                        let unk = windows_core::IUnknown::from_raw(p_unk);
                        std::mem::forget(unk.clone());
                        s = unk.cast().ok();
                    }
                    s
                }).flatten();

                let path_pipeline = pipeline_label(p_kind);
                let path_trace = resolve_trace_label(p_kind);

                if let Some(ref path) = worker_path {
                    // A path that only came from the stream's STATSTG name is an extension hint,
                    // never something to open relative to the host process's working directory.
                    let path_is_openable = p_kind != PIPELINE_STREAM_RPC && path.is_file();
                    if path_is_openable {
                        let ext = path.extension().and_then(|e| e.to_str()).map(|e| format!(".{}", e.to_ascii_lowercase())).unwrap_or_default();
                        if cbx_core::media::is_video_extension(&ext) {
                            let res = cbx_core::media::video_mf::extract_video_thumbnail_from_path(
                                path, cx, &worker_settings.smart_render_config()
                            );
                            let (mut rgba, rep_details) = match res {
                                Ok((rgba, rep)) => (
                                    rgba,
                                    cbx_core::i18n::t_fmt(
                                        "details.video_metrics",
                                        &[
                                            ("w", &rep.original_width.to_string()),
                                            ("h", &rep.original_height.to_string()),
                                            ("lum", &format!("{:.0}", rep.metrics.luminance_percent)),
                                            ("ent", &format!("{:.0}", rep.metrics.entropy_percent)),
                                            ("retries", &rep.attempts_made.to_string()),
                                        ],
                                    ),
                                ),
                                Err(e) => {
                                    let label = cbx_core::static_ext_label(&ext);
                                    (
                                        cbx_core::generate_procedural_video_card(cx, label, true),
                                        cbx_core::i18n::t_fmt("details.video_decode_failed_err", &[("err", &e.to_string())]),
                                    )
                                }
                            };
                            rgba = cbx_core::badge::apply_border_decoration(&rgba, worker_settings.border_video_enum());
                            if worker_settings.badge_video {
                                cbx_core::badge::draw_media_corner_badge(&mut rgba, &ext.trim_start_matches('.').to_ascii_uppercase(), true);
                            }
                            let bgra = cbx_core::rgba_to_bgra(&rgba);
                            return Ok(DecodedImagePayload {
                                width: rgba.width(),
                                height: rgba.height(),
                                bgra,
                                pipeline: path_pipeline,
                                search_trace: path_trace,
                                details: rep_details,
                            });
                        } else if let Ok(mut f) = std::fs::File::open(path) {
                            let (h, detail) = cbx_core::create_thumbnail_from_reader_unified(&mut f, cx, &worker_settings, Some(&path.to_string_lossy()))
                                .map_err(|_| Error::from(E_FAIL))?;
                            let (w, h_px, buf) = hbitmap_to_bgra(h)?;
                            return Ok(DecodedImagePayload {
                                width: w,
                                height: h_px,
                                bgra: buf,
                                pipeline: path_pipeline,
                                search_trace: path_trace,
                                details: detail,
                            });
                        } else if let Some(ref stream) = worker_stream {
                            let h = create_universal_thumbnail(stream, cx, &worker_settings, Some(&path.to_string_lossy()))
                                .map_err(|_| Error::from(E_FAIL))?;
                            let (w, h_px, buf) = hbitmap_to_bgra(h)?;
                            return Ok(DecodedImagePayload {
                                width: w,
                                height: h_px,
                                bgra: buf,
                                pipeline: pipeline_label(PIPELINE_STREAM_RPC),
                                search_trace: resolve_trace_label(PIPELINE_STREAM_RPC),
                                details: cbx_core::i18n::t_fmt("details.generic_stream", &[("cx", &cx.to_string())]),
                            });
                        }
                    } else if let Some(ref stream) = worker_stream {
                        let ext = path.extension().and_then(|e| e.to_str()).map(|e| format!(".{}", e.to_ascii_lowercase())).unwrap_or_default();
                        if cbx_core::media::is_video_extension(&ext) {
                            let h = create_universal_thumbnail(stream, cx, &worker_settings, Some(&path.to_string_lossy()))
                                .map_err(|_| Error::from(E_FAIL))?;
                            let (w, h_px, buf) = hbitmap_to_bgra(h)?;
                            return Ok(DecodedImagePayload {
                                width: w,
                                height: h_px,
                                bgra: buf,
                                pipeline: pipeline_label(PIPELINE_STREAM_RPC),
                                search_trace: resolve_trace_label(PIPELINE_STREAM_RPC),
                                details: cbx_core::i18n::t_fmt("details.video_stream", &[("cx", &cx.to_string())]),
                            });
                        } else {
                            let mut reader = cbx_core::ComStreamReader::new(stream.clone());
                            let (h, detail) = cbx_core::create_thumbnail_from_reader_unified(&mut reader, cx, &worker_settings, Some(&path.to_string_lossy()))
                                .map_err(|_| Error::from(E_FAIL))?;
                            let (w, h_px, buf) = hbitmap_to_bgra(h)?;
                            return Ok(DecodedImagePayload {
                                width: w,
                                height: h_px,
                                bgra: buf,
                                pipeline: pipeline_label(PIPELINE_STREAM_RPC),
                                search_trace: resolve_trace_label(PIPELINE_STREAM_RPC),
                                details: detail,
                            });
                        }
                    }
                } else if let Some(ref stream) = worker_stream {
                    let mut reader = cbx_core::ComStreamReader::new(stream.clone());
                    let (h, detail) = cbx_core::create_thumbnail_from_reader_unified(&mut reader, cx, &worker_settings, None)
                        .map_err(|_| Error::from(E_FAIL))?;
                    let (w, h_px, buf) = hbitmap_to_bgra(h)?;
                    return Ok(DecodedImagePayload {
                        width: w,
                        height: h_px,
                        bgra: buf,
                        pipeline: pipeline_label(PIPELINE_STREAM_RPC),
                        search_trace: resolve_trace_label(PIPELINE_STREAM_RPC),
                        details: detail,
                    });
                }
                Err(Error::from(E_FAIL))
            }));
            let _ = tx.send(res);
        });

        let timeout_sec = settings.watchdog_timeout_sec.clamp(3, 60);
        let watchdog_limit = std::time::Duration::from_secs(timeout_sec as u64);
        match rx.recv_timeout(watchdog_limit) {
            Ok(Ok(Ok(payload))) => {
                let hbmp = cbx_core::image_dec::create_hbitmap_from_bgra(payload.width, payload.height, &payload.bgra)
                    .map_err(|_| Error::from(E_FAIL))?;
                unsafe {
                    *phbmp = hbmp;
                    *pdwalpha = WTSAT_ARGB;
                }
                if let Some(guard) = task_guard {
                    cbx_core::monitor::record_task_end_with_trace(guard, true, &payload.pipeline, &payload.search_trace, &payload.details);
                }
                Ok(())
            }
            Ok(Ok(Err(_))) | Ok(Err(_)) => {
                let err_pipeline = pipeline_label(p_kind);
                let err_trace = resolve_trace_label(p_kind);
                if !cbx_core::media::is_video_extension(&ext_clone) {
                    // User Requirement:
                    // For archive files without images (e.g. a backup archive holding only documents),
                    // NEVER generate procedural fallback cards!
                    // Return E_FAIL immediately so Explorer preserves the original archive icon.
                    if let Some(guard) = task_guard {
                        cbx_core::monitor::record_task_end_with_trace(guard, true, &err_pipeline, &err_trace, &cbx_core::i18n::t("details.no_cover_image"));
                    }
                    return Err(Error::from(E_FAIL));
                }

                // Fallback card on decode error for video files
                let label = cbx_core::static_ext_label(&ext_clone);
                let mut rgba = cbx_core::generate_procedural_video_card(cx, label, true);
                rgba = cbx_core::badge::apply_border_decoration(&rgba, settings.border_video_enum());
                if settings.badge_video {
                    cbx_core::badge::draw_media_corner_badge(&mut rgba, &ext_clone.trim_start_matches('.').to_ascii_uppercase(), true);
                }
                let bgra = cbx_core::rgba_to_bgra(&rgba);
                if let Ok(h) = cbx_core::image_dec::create_hbitmap_from_bgra(rgba.width(), rgba.height(), &bgra) {
                    unsafe {
                        *phbmp = h;
                        *pdwalpha = WTSAT_ARGB;
                    }
                }
                if let Some(guard) = task_guard {
                    cbx_core::monitor::record_task_end_with_trace(guard, false, &err_pipeline, &err_trace, &cbx_core::i18n::t("details.video_decode_failed"));
                }
                Ok(())
            }
            Err(_) => {
                // TIMEOUT WATCHDOG TRIGGERED!
                let err_pipeline = pipeline_label(p_kind);
                let err_trace = resolve_trace_label(p_kind);
                let cur_stage = cbx_core::monitor::get_current_stage();
                let stage_desc = if cur_stage.is_empty() {
                    cbx_core::i18n::t("details.stream_or_decode_timeout")
                } else {
                    cur_stage
                };
                let timeout_details = cbx_core::i18n::t_fmt(
                    "details.timeout_skipped",
                    &[("sec", &format!("{:.0}", timeout_sec)), ("stage", &stage_desc)],
                );

                if !cbx_core::media::is_video_extension(&ext_clone) {
                    if let Some(guard) = task_guard {
                        cbx_core::monitor::record_task_end_with_trace(guard, false, &err_pipeline, &err_trace, &timeout_details);
                    }
                    return Err(Error::from(E_FAIL));
                }

                let label = cbx_core::static_ext_label(&ext_clone);
                let mut rgba = cbx_core::generate_procedural_video_card(cx, label, true);
                rgba = cbx_core::badge::apply_border_decoration(&rgba, settings.border_video_enum());
                if settings.badge_video {
                    cbx_core::badge::draw_media_corner_badge(&mut rgba, &ext_clone.trim_start_matches('.').to_ascii_uppercase(), true);
                }
                let bgra = cbx_core::rgba_to_bgra(&rgba);
                if let Ok(h) = cbx_core::image_dec::create_hbitmap_from_bgra(rgba.width(), rgba.height(), &bgra) {
                    unsafe {
                        *phbmp = h;
                        *pdwalpha = WTSAT_ARGB;
                    }
                }
                if let Some(guard) = task_guard {
                    cbx_core::monitor::record_task_end_with_trace(guard, false, &err_pipeline, &err_trace, &timeout_details);
                }
                Ok(())
            }
        }
    }
}

impl IPersist_Impl for CbxShellProvider_Impl {
    fn GetClassID(&self) -> Result<GUID> {
        Ok(CLSID_CBXSHELL_RS)
    }
}

impl IPersistFile_Impl for CbxShellProvider_Impl {
    fn IsDirty(&self) -> HRESULT {
        S_FALSE
    }

    fn Load(&self, pszfilename: &PCWSTR, _dwmode: STGM) -> Result<()> {
        if pszfilename.is_null() {
            return Err(Error::from(E_INVALIDARG));
        }
        // SAFETY: pszfilename is a valid null-terminated UTF-16 string from the Shell.
        let s = unsafe { pszfilename.to_string() }.map_err(|_| Error::from(E_INVALIDARG))?;
        if s.is_empty() {
            return Err(Error::from(E_INVALIDARG));
        }
        let mut guard = self.file_path.lock().map_err(|_| Error::from(E_FAIL))?;
        *guard = Some(PathBuf::from(s));
        self.pipeline_kind.store(PIPELINE_DIRECT_FILE, Ordering::SeqCst);
        Ok(())
    }

    fn Save(&self, _pszfilename: &PCWSTR, _fremember: windows::Win32::Foundation::BOOL) -> Result<()> {
        Err(Error::from(E_NOTIMPL))
    }

    fn SaveCompleted(&self, _pszfilename: &PCWSTR) -> Result<()> {
        Err(Error::from(E_NOTIMPL))
    }

    fn GetCurFile(&self) -> Result<PWSTR> {
        let guard = self.file_path.lock().map_err(|_| Error::from(E_FAIL))?;
        let Some(ref p) = *guard else {
            return Err(Error::from(E_FAIL));
        };
        alloc_co_task_wstr(&p.to_string_lossy())
    }
}

impl IQueryInfo_Impl for CbxShellProvider_Impl {
    unsafe fn GetInfoTip(&self, _dwflags: u32, ppwsztip: *mut PWSTR) -> HRESULT {
        if ppwsztip.is_null() {
            return E_POINTER;
        }
        // SAFETY: non-null out-pointer.
        unsafe { *ppwsztip = PWSTR::null() };

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<PWSTR> {
            let path = {
                let guard = self.file_path.lock().map_err(|_| Error::from(E_FAIL))?;
                guard.clone().ok_or_else(|| Error::from(E_UNEXPECTED))?
            };
            let tip = build_infotip(&path).map_err(|_| Error::from(E_FAIL))?;
            alloc_co_task_wstr(&tip)
        }));

        match res {
            Ok(Ok(ptr)) => {
                // SAFETY: non-null out-pointer.
                unsafe { *ppwsztip = ptr };
                S_OK
            }
            Ok(Err(e)) => e.code(),
            Err(_) => E_UNEXPECTED,
        }
    }

    unsafe fn GetInfoFlags(&self, pdwflags: *mut u32) -> HRESULT {
        if !pdwflags.is_null() {
            // SAFETY: non-null out-pointer.
            unsafe { *pdwflags = 0 };
        }
        E_NOTIMPL
    }
}

#[implement(IClassFactory)]
struct CbxClassFactory {
    _guard: ObjectCountGuard,
}

impl CbxClassFactory {
    fn new() -> Self {
        Self {
            _guard: ObjectCountGuard::new(),
        }
    }
}

impl IClassFactory_Impl for CbxClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Option<&windows::core::IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> Result<()> {
        if ppvobject.is_null() || riid.is_null() {
            return Err(Error::from(E_POINTER));
        }
        // SAFETY: non-null out-pointer.
        unsafe { *ppvobject = std::ptr::null_mut() };
        if punkouter.is_some() {
            return Err(Error::from(CLASS_E_NOAGGREGATION));
        }
        let obj: ComObject<CbxShellProvider> = CbxShellProvider::new().into();
        // SAFETY: riid and ppvobject were checked above.
        unsafe { obj.QueryInterface(riid, ppvobject) }.ok()
    }

    fn LockServer(&self, flock: windows::Win32::Foundation::BOOL) -> Result<()> {
        if flock.as_bool() {
            ACTIVE_OBJECTS.fetch_add(1, Ordering::SeqCst);
        } else {
            ACTIVE_OBJECTS.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

fn alloc_co_task_wstr(s: &str) -> Result<PWSTR> {
    let wide = to_wide(s);
    let byte_len = wide.len() * 2;
    // SAFETY: CoTaskMemAlloc allocates COM-compatible heap memory freed by the Shell.
    let ptr = unsafe { CoTaskMemAlloc(byte_len) }.cast::<u16>();
    if ptr.is_null() {
        return Err(Error::from(E_FAIL));
    }
    // SAFETY: `ptr` has space for `wide.len()` u16 elements.
    unsafe {
        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
    }
    Ok(PWSTR(ptr))
}

fn current_dll_path() -> Option<String> {
    let hmod = HMODULE(MODULE_HANDLE.load(Ordering::Relaxed) as *mut c_void);
    let mut buf = [0u16; MAX_PATH as usize * 4];
    // SAFETY: buf is valid for `buf.len()` wide chars.
    let len = unsafe { GetModuleFileNameW(hmod, &mut buf) } as usize;
    if len == 0 || len >= buf.len() {
        return None;
    }
    Some(String::from_utf16_lossy(&buf[..len]))
}

// ---------------------------------------------------------------------------
// Standard COM DLL exports

#[no_mangle]
pub unsafe extern "system" fn DllMain(
    hinst: HINSTANCE,
    reason: u32,
    _reserved: *mut c_void,
) -> i32 {
    if reason == DLL_PROCESS_ATTACH {
        MODULE_HANDLE.store(hinst.0 as usize, Ordering::Relaxed);
        // NOTE: Do NOT call DisableThreadLibraryCalls in a Rust DLL because Rust's standard
        // library and CRT rely on DLL_THREAD_ATTACH / DLL_THREAD_DETACH notifications for TLS cleanup.
    }
    1
}

#[no_mangle]
pub unsafe extern "system" fn DllCanUnloadNow() -> HRESULT {
    if ACTIVE_OBJECTS.load(Ordering::SeqCst) <= 0 {
        S_OK
    } else {
        S_FALSE
    }
}

#[no_mangle]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if rclsid.is_null() || riid.is_null() || ppv.is_null() {
        return E_POINTER;
    }
    // SAFETY: pointers checked for null above.
    unsafe {
        *ppv = std::ptr::null_mut();
        if *rclsid != CLSID_CBXSHELL_RS {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
    }
    let res = std::panic::catch_unwind(|| {
        let factory: ComObject<CbxClassFactory> = CbxClassFactory::new().into();
        // SAFETY: riid and ppv checked above.
        unsafe { factory.QueryInterface(riid, ppv) }
    });
    res.unwrap_or(E_UNEXPECTED)
}

#[no_mangle]
pub unsafe extern "system" fn DllRegisterServer() -> HRESULT {
    let Some(dll_path) = current_dll_path() else {
        return E_FAIL;
    };

    let clsid_key = format!(r"Software\Classes\CLSID\{CLSID_CBXSHELL_RS_STR}");
    let inproc_key = format!(r"{clsid_key}\InprocServer32");
    let approved_key = r"Software\Microsoft\Windows\CurrentVersion\Shell Extensions\Approved";

    let settings = Settings::load();

    // Register in HKCU (works even without elevation for per-user COM) and HKLM (if elevated).
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        write_reg_sz(root, &clsid_key, None, "cbxshell-rs Thumbnail Provider");
        write_reg_sz(root, &inproc_key, None, &dll_path);
        write_reg_sz(root, &inproc_key, Some("ThreadingModel"), "Free");
        write_reg_sz(
            root,
            approved_key,
            Some(CLSID_CBXSHELL_RS_STR),
            "cbxshell-rs Thumbnail Provider",
        );
    }

    // Configure execution mode (Safe Process Isolation vs In-Process) based on settings
    cbx_core::registry::apply_execution_mode(settings.video_sandbox_mode);

    let custom_texts = settings.parsed_custom_text_extensions();
    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        for fmt in cbx_core::media::SUPPORTED_MEDIA_FORMATS {
            if fmt.default_on || cbx_core::registry::is_associated(root, fmt.ext) {
                let enable_tip = settings.enable_infotip && fmt.category == cbx_core::media::MediaCategory::ImageArchive;
                associate_extension(root, fmt.ext, enable_tip);
            }
        }
        for ext in &custom_texts {
            if cbx_core::registry::is_associated(root, ext) {
                associate_extension(root, ext, false);
            }
        }
    }
    notify_shell_associations_changed();
    S_OK
}

#[no_mangle]
pub unsafe extern "system" fn DllUnregisterServer() -> HRESULT {
    for fmt in cbx_core::media::SUPPORTED_MEDIA_FORMATS {
        disassociate_extension(HKEY_CURRENT_USER, fmt.ext);
        disassociate_extension(HKEY_LOCAL_MACHINE, fmt.ext);
    }

    let appid_w = to_wide(&format!(r"Software\Classes\AppID\{CLSID_CBXSHELL_RS_STR}"));
    let clsid_w = to_wide(&format!(r"Software\Classes\CLSID\{CLSID_CBXSHELL_RS_STR}"));
    let approved_w = to_wide(r"Software\Microsoft\Windows\CurrentVersion\Shell Extensions\Approved");
    let guid_w = to_wide(CLSID_CBXSHELL_RS_STR);

    for root in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        // SAFETY: null-terminated wide strings.
        unsafe {
            let _ = RegDeleteTreeW(root, PCWSTR(clsid_w.as_ptr()));
            let _ = RegDeleteTreeW(root, PCWSTR(appid_w.as_ptr()));
            let _ = RegDeleteKeyValueW(root, PCWSTR(approved_w.as_ptr()), PCWSTR(guid_w.as_ptr()));
        }
    }

    notify_shell_associations_changed();
    S_OK
}
