use super::smart_render::{
    aces_filmic_tone_mapping, compute_luminance_and_entropy_bgra,
    compute_luminance_and_entropy_y_plane, compute_luminance_and_entropy_yuy2,
    evaluate_metrics, FrameMetrics, SmartRenderConfig,
};
use image::{Rgba, RgbaImage};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use windows::core::{GUID, PCWSTR, PROPVARIANT};
use windows::Win32::Media::MediaFoundation::{
    MFCreateAttributes, MFCreateMediaType, MFCreateMFByteStreamOnStream, MFCreateSourceReaderFromByteStream,
    MFCreateSourceReaderFromURL, MFStartup, IMFAttributes, IMFByteStream,
    IMFSample, IMFSourceReader, MFMediaType_Video, MFVideoFormat_NV12,
    MFVideoFormat_RGB32, MFVideoFormat_YUY2,
    MFSTARTUP_NOSOCKET, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_PD_DURATION,
    MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_SOURCE_READER_ALL_STREAMS,
    MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
    MF_SOURCE_READER_MEDIASOURCE, MF_SOURCE_READERF_ENDOFSTREAM,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToInt64WithDefault;
use windows::Win32::System::Com::IStream;

const MF_VERSION: u32 = 0x00020070;
const MF_LOW_LATENCY: GUID = GUID::from_u128(0x9c27891a_ed89_4104_8eec_214050f1ef9e);
const MF_MT_DEFAULT_STRIDE: GUID = GUID::from_u128(0x64440491_4c20_4f6e_a913_cc5c120fb654);
const MF_MT_TRANSFER_FUNCTION: GUID = GUID::from_u128(0x5fb0fce9_be5c_4935_a811_ec838f8eed93);
const MF_MT_VIDEO_PRIMARIES: GUID = GUID::from_u128(0xdbfbe4d7_0740_4e08_addc_1e91859c00b0);
const MF_VIDEO_FORMAT_P010: GUID = GUID::from_u128(0x30313050_0000_0010_8000_00aa00389b71);
const MF_VIDEO_FORMAT_P016: GUID = GUID::from_u128(0x30313036_0000_0010_8000_00aa00389b71);
static MF_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Ensures Media Foundation is initialized once for the process.
pub fn ensure_mf_initialized() {
    if !MF_INITIALIZED.swap(true, Ordering::SeqCst) {
        unsafe {
            let _ = MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET);
        }
    }
}

pub struct VideoExtractReport {
    pub metrics: FrameMetrics,
    pub attempts_made: u32,
    pub duration_seconds: f64,
    pub selected_time_seconds: f64,
    pub original_width: u32,
    pub original_height: u32,
}

unsafe fn create_mf_attributes() -> Option<IMFAttributes> {
    let mut attr: Option<IMFAttributes> = None;
    if MFCreateAttributes(&mut attr, 3).is_ok() {
        if let Some(ref a) = attr {
            let _ = a.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1);
            let _ = a.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1);
            let _ = a.SetUINT32(&MF_LOW_LATENCY, 1);
        }
    }
    attr
}

enum VideoPixelFormat {
    Rgb32,
    Nv12,
    Yuy2,
}

/// Extracts a representative video thumbnail from an `IStream` using Media Foundation and Smart Rendering.
pub fn extract_video_thumbnail_from_stream(
    stream: &IStream,
    side: u32,
    config: &SmartRenderConfig,
) -> Result<(RgbaImage, VideoExtractReport), String> {
    ensure_mf_initialized();

    // Wrap in BufferedIStream to eliminate cross-process COM RPC seek/read round-trip delays
    let buffered = super::buffered_stream::BufferedIStream::new(stream.clone());

    unsafe {
        let byte_stream: IMFByteStream = MFCreateMFByteStreamOnStream(&buffered)
            .map_err(|e| format!("MFCreateMFByteStreamOnStream failed: {e}"))?;

        let attr = create_mf_attributes();
        let mf_res = match MFCreateSourceReaderFromByteStream(&byte_stream, attr.as_ref()) {
            Ok(reader) => process_source_reader(&reader, side, config, true),
            Err(e) => Err(format!("MFCreateSourceReaderFromByteStream failed: {e}")),
        };
        match mf_res {
            Ok(res) => Ok(res),
            Err(mf_err) => {
                // Media Foundation cannot demux this container / codec from a stream (FLV, many
                // MKV, Hi10P ...). Feed the leading part of the stream to FFmpeg through its stdin
                // instead: the shell host never opens the file (or a network path) itself.
                match extract_video_thumbnail_ffmpeg_stream(stream, side, config) {
                    Ok(res) => Ok(res),
                    Err(ff_err) => Err(format!("{mf_err}; FFmpeg (stream) fallback: {ff_err}")),
                }
            }
        }
    }
}

/// Stream bytes fed to FFmpeg for header / duration probing.
const FFMPEG_STREAM_HEAD_BYTES: usize = 2 * 1024 * 1024;
/// Stream bytes fed per candidate frame (around the estimated byte position of its timestamp).
const FFMPEG_STREAM_WINDOW_BYTES: usize = 8 * 1024 * 1024;
/// Bytes read before the estimated position so a keyframe precedes the target time.
const FFMPEG_STREAM_PREROLL_BYTES: u64 = 2 * 1024 * 1024;

fn stream_size(stream: &IStream) -> Result<u64, String> {
    use std::io::{Seek, SeekFrom};
    let mut reader = crate::ComStreamReader::new(stream.clone());
    reader.seek(SeekFrom::End(0)).map_err(|e| format!("stream size query failed: {e}"))
}

/// Reads at most `max` bytes of `stream` starting at `offset`, in 1 MiB chunks, giving up once
/// `deadline` passes: on a slow network share a large read must never outlive the watchdog and
/// keep the shell host busy after Explorer has already been answered.
fn read_stream_range(stream: &IStream, offset: u64, max: usize, deadline: std::time::Instant) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek, SeekFrom};
    const CHUNK: usize = 1024 * 1024;
    let mut reader = crate::ComStreamReader::new(stream.clone());
    reader.seek(SeekFrom::Start(offset)).map_err(|e| format!("stream seek failed: {e}"))?;
    let mut data = Vec::with_capacity(max.min(16 * CHUNK));
    let mut buf = vec![0u8; CHUNK];
    while data.len() < max {
        if std::time::Instant::now() >= deadline {
            return Err("stream read exceeded the time budget".to_string());
        }
        let want = (max - data.len()).min(CHUNK);
        let n = reader.read(&mut buf[..want]).map_err(|e| format!("stream read failed: {e}"))?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }
    Ok(data)
}

/// FFmpeg frame extraction for stream-only files, driven by the same algorithm parameters as the
/// file pipeline (frame-selection mode, percentage / fixed offset / threshold, step interval,
/// probe limit, luminance and entropy scoring, watchdog budget, HDR tone-mapping).
///
/// FFmpeg reads only from stdin (`-protocol_whitelist pipe`), so it cannot seek. Instead:
/// 1. the stream head is fed once to read the duration and HDR hints;
/// 2. for each candidate time the byte position is estimated from duration and stream size,
///    and the container head plus a window around that position is fed; FFmpeg's demuxers
///    resynchronise on the jump and the first frame at or after the target is taken;
/// 3. targets near the start are fed contiguously and selected with an output-side `-ss`.
fn extract_video_thumbnail_ffmpeg_stream(
    stream: &IStream,
    side: u32,
    config: &SmartRenderConfig,
) -> Result<(RgbaImage, VideoExtractReport), String> {
    let ffmpeg_bin = match find_ffmpeg_path() {
        Some(p) if is_trusted_ffmpeg_bin(&p, Path::new("")) => p,
        Some(_) => return Err("Rejected untrusted FFmpeg binary path".to_string()),
        None => return Err(crate::i18n::t("error.ffmpeg_missing")),
    };
    let size = stream_size(stream)?;
    if size == 0 {
        return Err("empty stream".to_string());
    }
    let budget_ms = ((config.watchdog_timeout_sec.clamp(3, 60) as u128) * 1000)
        .saturating_sub(500)
        .max(1500);
    let loop_start = std::time::Instant::now();
    let deadline = loop_start + std::time::Duration::from_millis(budget_ms as u64);
    let head = std::sync::Arc::new(read_stream_range(stream, 0, FFMPEG_STREAM_HEAD_BYTES, deadline)?);
    let remaining = || std::time::Duration::from_millis(budget_ms.saturating_sub(loop_start.elapsed().as_millis()) as u64);

    // 1. Duration / HDR probe from the head.
    let mut probe = std::process::Command::new(&ffmpeg_bin);
    prepare_ffmpeg_sandbox(&mut probe, &ffmpeg_bin);
    probe.args(ffmpeg_input_hardening_args("pipe"));
    probe.args(["-i", "pipe:0"]);
    let (duration_sec, is_hdr) = run_ffmpeg_bounded_with_input(
        &mut probe,
        remaining().min(std::time::Duration::from_secs(5)),
        64 * 1024,
        512 * 1024,
        Some(head.clone()),
    )
    .map(|(_, _, err)| parse_probe_stderr(&err))
    .unwrap_or((0.0, false));
    let tonemap_active = config.hdr_tonemap_enabled && is_hdr;

    // Without a duration (some streamed containers) positions cannot be estimated: start at the
    // beginning and step forward from there.
    let target_seek_sec = if duration_sec > 0.0 { ffmpeg_target_seek_sec(duration_sec, config) } else { 0.0 };
    let step_sec = (config.step_probe_sec as f64).clamp(1.0, 60.0);
    let max_attempts = config.consecutive_unacceptable_limit.clamp(1, 20);
    let vf_scale = format!("scale=min(iw\\,{side}):-1");

    let mut current_seek_sec = target_seek_sec;
    let mut best: Option<(RgbaImage, VideoExtractReport)> = None;
    let mut best_score = f32::MIN;
    let mut last_err = String::from("no frame decoded");

    for attempt in 1..=max_attempts + 1 {
        // The extra final round (only when nothing decoded) takes the first frame of the stream.
        let fallback_round = attempt > max_attempts;
        if fallback_round && best.is_some() {
            break;
        }
        let left = remaining();
        if left < std::time::Duration::from_millis(400) {
            break;
        }
        let t = if fallback_round { 0.0 } else { current_seek_sec };

        // Estimated byte position of `t`; near the start (or without a duration) feed contiguously.
        let est = if duration_sec > 0.0 { ((size as f64) * (t / duration_sec)).clamp(0.0, size as f64) as u64 } else { 0 };
        let contiguous = duration_sec <= 0.0 || est <= (FFMPEG_STREAM_HEAD_BYTES + FFMPEG_STREAM_WINDOW_BYTES) as u64;
        let (input, seek_args, vf_arg) = if contiguous {
            let len = (est as usize).saturating_add(FFMPEG_STREAM_WINDOW_BYTES);
            let data = if len <= head.len() {
                (*head).clone()
            } else {
                match read_stream_range(stream, 0, len, deadline) {
                    Ok(d) => d,
                    Err(e) => {
                        last_err = e;
                        break;
                    }
                }
            };
            (data, vec!["-ss".to_string(), format!("{t:.2}")], vf_scale.clone())
        } else {
            let from = est.saturating_sub(FFMPEG_STREAM_PREROLL_BYTES).max(head.len() as u64);
            let mut data = (*head).clone();
            match read_stream_range(stream, from, FFMPEG_STREAM_WINDOW_BYTES, deadline) {
                Ok(d) => data.extend_from_slice(&d),
                Err(e) => {
                    last_err = e;
                    break;
                }
            }
            // Frames from the head are early; take the first one at or after a margin below `t`.
            let lo = (t * 0.5).max(t - 30.0).max(0.0);
            (data, Vec::new(), format!("select=gte(t\\,{lo:.2}),{vf_scale}"))
        };

        let mut cmd = std::process::Command::new(&ffmpeg_bin);
        prepare_ffmpeg_sandbox(&mut cmd, &ffmpeg_bin);
        cmd.args(ffmpeg_input_hardening_args("pipe"));
        cmd.args(["-fflags", "+discardcorrupt", "-i", "pipe:0"]);
        cmd.args(&seek_args);
        cmd.args(["-an", "-sn", "-dn", "-vf", vf_arg.as_str(), "-frames:v", "1", "-f", "image2pipe", "-vcodec", "mjpeg", "-q:v", "2", "-"]);

        let step_timeout = left.min(std::time::Duration::from_secs(6));
        match run_ffmpeg_bounded_with_input(&mut cmd, step_timeout, 16 * 1024 * 1024, 256 * 1024, Some(std::sync::Arc::new(input))) {
            Some((status, stdout, _stderr)) if status.success() && !stdout.is_empty() => {
                let mut reader = image::ImageReader::new(std::io::Cursor::new(&stdout));
                if let Ok(guessed) = reader.with_guessed_format() {
                    reader = guessed;
                    let mut lim = image::Limits::default();
                    lim.max_image_width = Some(crate::image_dec::MAX_IMAGE_DIMENSION);
                    lim.max_image_height = Some(crate::image_dec::MAX_IMAGE_DIMENSION);
                    lim.max_alloc = Some(crate::image_dec::MAX_DECODER_ALLOC_BYTES);
                    reader.limits(lim);
                    if let Ok(img) = reader.decode() {
                        let (orig_w, orig_h) = (img.width(), img.height());
                        let mut rgba = img.to_rgba8();
                        if tonemap_active {
                            for px in rgba.chunks_exact_mut(4) {
                                let (r, g, b) = aces_filmic_tone_mapping(px[0] as f32 / 255.0, px[1] as f32 / 255.0, px[2] as f32 / 255.0);
                                px[0] = r;
                                px[1] = g;
                                px[2] = b;
                            }
                        }
                        let metrics = super::smart_render::evaluate_frame(&rgba, false, config);
                        let report = VideoExtractReport {
                            metrics,
                            attempts_made: attempt,
                            duration_seconds: duration_sec,
                            selected_time_seconds: t,
                            original_width: orig_w,
                            original_height: orig_h,
                        };
                        if metrics.is_accepted {
                            return Ok((rgba, report));
                        }
                        if metrics.score > best_score {
                            best_score = metrics.score;
                            best = Some((rgba, report));
                        }
                    }
                }
            }
            Some((_, _, stderr)) => {
                let line = last_stderr_line(&stderr);
                if !line.is_empty() {
                    last_err = line;
                }
            }
            None => last_err = "FFmpeg could not be started or timed out".to_string(),
        }
        current_seek_sec = ffmpeg_next_seek_sec(current_seek_sec, step_sec, duration_sec);
    }

    best.ok_or_else(|| format!("FFmpeg (stream) failed to extract a frame ({last_err})"))
}

/// Extracts a representative video thumbnail from a file path using Media Foundation and Smart Rendering.
pub fn extract_video_thumbnail_from_path(
    path: &Path,
    side: u32,
    config: &SmartRenderConfig,
) -> Result<(RgbaImage, VideoExtractReport), String> {
    ensure_mf_initialized();

    let path_str = path.to_string_lossy();
    let wide: Vec<u16> = path_str.encode_utf16().chain(std::iter::once(0)).collect();

    let mf_res = unsafe {
        let attr = create_mf_attributes();
        match MFCreateSourceReaderFromURL(PCWSTR(wide.as_ptr()), attr.as_ref()) {
            Ok(reader) => process_source_reader(&reader, side, config, false),
            Err(e) => Err(format!("MFCreateSourceReaderFromURL failed for '{}': {e}", path.display())),
        }
    };

    match mf_res {
        Ok(res) => Ok(res),
        Err(mf_err) => {
            // Universal Fallback to FFmpeg (for 10-bit H.264 Hi10P, rare formats, etc.)
            match extract_video_thumbnail_ffmpeg(path, side, config) {
                Ok(res) => Ok(res),
                Err(ff_err) => Err(format!("{mf_err}; FFmpeg fallback: {ff_err}")),
            }
        }
    }
}

/// Pure automatic detection of FFmpeg executable matching installer specification,
/// including application directories, registry InstallLocation, WinGet links, or system PATH.
pub fn find_ffmpeg_path_auto() -> Option<PathBuf> {
    // 1. Check current executable directory and its bin subfolder
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(parent) = current_exe.parent() {
            let candidate1 = parent.join("ffmpeg.exe");
            if candidate1.is_file() {
                return Some(candidate1);
            }
            let candidate2 = parent.join("bin").join("ffmpeg.exe");
            if candidate2.is_file() {
                return Some(candidate2);
            }
        }
    }

    // 2. Check registered InstallLocation written by cbx-installer (HKCU / HKLM)
    const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\CBXShell";
    for root in [windows::Win32::System::Registry::HKEY_CURRENT_USER, windows::Win32::System::Registry::HKEY_LOCAL_MACHINE] {
        if let Some(loc) = crate::registry::read_reg_sz(root, UNINSTALL_KEY, Some("InstallLocation")) {
            let p = PathBuf::from(loc.trim());
            let c1 = p.join("bin").join("ffmpeg.exe");
            if c1.is_file() {
                return Some(c1);
            }
            let c2 = p.join("ffmpeg.exe");
            if c2.is_file() {
                return Some(c2);
            }
        }
    }

    // 3. Check default per-user install directory (%LOCALAPPDATA%\Programs\CBXShell)
    if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
        let user_cbx = PathBuf::from(&local_app_data).join("Programs").join("CBXShell");
        let c1 = user_cbx.join("bin").join("ffmpeg.exe");
        if c1.is_file() {
            return Some(c1);
        }
        let c2 = user_cbx.join("ffmpeg.exe");
        if c2.is_file() {
            return Some(c2);
        }

        // 4. Check WinGet Links
        let winget_path = PathBuf::from(&local_app_data).join("Microsoft").join("WinGet").join("Links").join("ffmpeg.exe");
        if winget_path.is_file() {
            return Some(winget_path);
        }
    }

    // 5. Check machine install directory (%ProgramFiles%\CBXShell)
    if let Ok(prog_files) = std::env::var("ProgramFiles") {
        let machine_cbx = PathBuf::from(&prog_files).join("CBXShell");
        let c1 = machine_cbx.join("bin").join("ffmpeg.exe");
        if c1.is_file() {
            return Some(c1);
        }
        let c2 = machine_cbx.join("ffmpeg.exe");
        if c2.is_file() {
            return Some(c2);
        }
    }
    if let Ok(prog_files_x86) = std::env::var("ProgramFiles(x86)") {
        let machine_cbx = PathBuf::from(&prog_files_x86).join("CBXShell");
        let c1 = machine_cbx.join("bin").join("ffmpeg.exe");
        if c1.is_file() {
            return Some(c1);
        }
        let c2 = machine_cbx.join("ffmpeg.exe");
        if c2.is_file() {
            return Some(c2);
        }
    }

    // 6. Check system PATH (only trusted absolute non-Temp/non-Downloads directories)
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            if !dir.is_absolute() {
                continue;
            }
            let dir_lower = dir.to_string_lossy().to_ascii_lowercase();
            if dir_lower.starts_with("\\\\")
                || dir_lower.contains("\\appdata\\local\\temp")
                || dir_lower.ends_with("\\downloads")
                || dir_lower.contains("\\downloads\\")
            {
                continue;
            }
            let candidate = dir.join("ffmpeg.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Resolves FFmpeg status. Returns `(Option<PathBuf>, bool)` where the boolean indicates whether manual override was used.
pub fn resolve_ffmpeg_status(custom_path: &str) -> (Option<PathBuf>, bool) {
    let trimmed = custom_path.trim();
    if !trimmed.is_empty() {
        let p = PathBuf::from(trimmed);
        if p.is_file() {
            (Some(p), true)
        } else {
            (None, true)
        }
    } else {
        (find_ffmpeg_path_auto(), false)
    }
}

/// Searches for FFmpeg executable: checks custom path setting first, falling back to automatic system detection.
pub fn find_ffmpeg_path() -> Option<PathBuf> {
    let settings = crate::settings::Settings::load();
    let trimmed = settings.custom_ffmpeg_path.trim();
    if !trimmed.is_empty() {
        let p = PathBuf::from(trimmed);
        if p.is_file() {
            return Some(p);
        }
    }
    find_ffmpeg_path_auto()
}

fn is_trusted_ffmpeg_bin(ffmpeg_bin: &Path, media_path: &Path) -> bool {
    let s = ffmpeg_bin.to_string_lossy();
    if !ffmpeg_bin.is_absolute() || s.starts_with("\\\\") || s.starts_with("//") {
        return false;
    }
    if s.char_indices().any(|(idx, ch)| ch == ':' && idx != 1) {
        return false;
    }
    if !ffmpeg_bin.extension().map_or(false, |e| e.eq_ignore_ascii_case("exe")) || !ffmpeg_bin.is_file() {
        return false;
    }
    // Never execute an ffmpeg.exe placed inside the untrusted media file's own directory
    if let (Some(ff_dir), Some(media_dir)) = (ffmpeg_bin.parent(), media_path.parent()) {
        if ff_dir.to_string_lossy().eq_ignore_ascii_case(&media_dir.to_string_lossy()) {
            return false;
        }
    }
    true
}

fn prepare_ffmpeg_sandbox(cmd: &mut std::process::Command, ffmpeg_bin: &Path) {
    let sys_root = crate::registry::system_root_dir();
    let sys32 = sys_root.join("System32");
    let work_dir = ffmpeg_bin.parent().unwrap_or(&sys32);
    let safe_path = format!("{};{}", work_dir.display(), sys32.display());

    // Clear inherited environment and restrict CWD/PATH to prevent DLL planting
    cmd.current_dir(work_dir)
        .env_clear()
        .env("SystemRoot", &sys_root)
        .env("WINDIR", &sys_root)
        .env("PATH", safe_path);
}

#[cfg(windows)]
fn apply_least_privilege_job_and_token(child: &std::process::Child) -> Option<windows::Win32::Foundation::HANDLE> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{AdjustTokenPrivileges, TOKEN_ADJUST_PRIVILEGES};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicUIRestrictions,
        JobObjectExtendedLimitInformation, SetInformationJobObject, JOBOBJECT_BASIC_UI_RESTRICTIONS,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
        JOB_OBJECT_UILIMIT_DESKTOP, JOB_OBJECT_UILIMIT_DISPLAYSETTINGS,
        JOB_OBJECT_UILIMIT_EXITWINDOWS, JOB_OBJECT_UILIMIT_GLOBALATOMS,
        JOB_OBJECT_UILIMIT_HANDLES, JOB_OBJECT_UILIMIT_READCLIPBOARD,
        JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS, JOB_OBJECT_UILIMIT_WRITECLIPBOARD,
    };
    use windows::Win32::System::Threading::OpenProcessToken;

    let proc_handle = HANDLE(child.as_raw_handle() as *mut _);

    // 1. Strip all optional token privileges from the child process
    unsafe {
        let mut tok = HANDLE::default();
        if OpenProcessToken(proc_handle, TOKEN_ADJUST_PRIVILEGES, &mut tok).is_ok() {
            let _ = AdjustTokenPrivileges(tok, true, None, 0, None, None);
            let _ = CloseHandle(tok);
        }
    }

    // 2. Bind child to a strict Job Object (no child processes, 512 MiB memory cap, UI isolation, kill on close)
    let Ok(job) = (unsafe { CreateJobObjectW(None, windows::core::PCWSTR::null()) }) else {
        return None;
    };
    let mut ext_limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    ext_limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_PROCESS_MEMORY;
    ext_limits.BasicLimitInformation.ActiveProcessLimit = 1;
    ext_limits.ProcessMemoryLimit = 512 * 1024 * 1024;
    let _ = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&ext_limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };

    let ui_limits = JOBOBJECT_BASIC_UI_RESTRICTIONS {
        UIRestrictionsClass: JOB_OBJECT_UILIMIT_HANDLES
            | JOB_OBJECT_UILIMIT_READCLIPBOARD
            | JOB_OBJECT_UILIMIT_WRITECLIPBOARD
            | JOB_OBJECT_UILIMIT_DISPLAYSETTINGS
            | JOB_OBJECT_UILIMIT_EXITWINDOWS
            | JOB_OBJECT_UILIMIT_GLOBALATOMS
            | JOB_OBJECT_UILIMIT_DESKTOP
            | JOB_OBJECT_UILIMIT_SYSTEMPARAMETERS,
    };
    let _ = unsafe {
        SetInformationJobObject(
            job,
            JobObjectBasicUIRestrictions,
            (&ui_limits as *const JOBOBJECT_BASIC_UI_RESTRICTIONS).cast(),
            std::mem::size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
        )
    };

    if unsafe { AssignProcessToJobObject(job, proc_handle) }.is_err() {
        let _ = unsafe { CloseHandle(job) };
        return None;
    }
    Some(job)
}

/// Resumes every thread of a process that was created with `CREATE_SUSPENDED`.
/// Returns `true` if at least one thread was resumed.
#[cfg(windows)]
fn resume_suspended_process(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    let mut resumed = 0u32;
    // SAFETY: the snapshot handle and THREADENTRY32 out-structure are owned locally and the
    // thread handles are closed right after use.
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) else {
            return false;
        };
        let mut te = THREADENTRY32 {
            dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };
        let mut more = Thread32First(snap, &mut te).is_ok();
        while more {
            if te.th32OwnerProcessID == pid {
                if let Ok(th) = OpenThread(THREAD_SUSPEND_RESUME, false, te.th32ThreadID) {
                    if ResumeThread(th) != u32::MAX {
                        resumed += 1;
                    }
                    let _ = CloseHandle(th);
                }
            }
            more = Thread32Next(snap, &mut te).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    resumed > 0
}

/// Input-side FFmpeg hardening shared by every invocation.
///
/// * `-protocol_whitelist file` - only the local file protocol; no network, no `pipe:`/`fd:`,
///   no `concat:`/`subfile:` URL tricks.
/// * `-format_whitelist ...` - only ordinary video container demuxers may be selected by
///   probing. This blocks *content-type confusion* attacks in which a file named `.mp4`/`.avi`
///   actually contains an HLS/DASH playlist, an `ffconcat` script, an `image2` sequence pattern
///   or another meta-demuxer that makes FFmpeg open *other* files (classic local-file-disclosure
///   chains that render the bytes of e.g. a private key into the thumbnail).
/// * `-codec_whitelist ...` - only mainstream video decoders may be instantiated. Rare/legacy
///   decoders are where most memory-safety bugs live (e.g. CVE-2026-8461, a heap out-of-bounds
///   write in the MagicYUV decoder); a file cannot force FFmpeg to load a decoder outside this list.
/// Input-side hardening shared by every FFmpeg invocation.
///
/// Only generic `AVFormatContext` options belong here. Demuxer-private options (e.g. the HLS
/// demuxer's `-max_reload`) are rejected by the ffmpeg CLI with "Option … not found" whenever the
/// probed demuxer is a different one, which made every FLV / MKV / AVI fallback fail.
pub(crate) const FFMPEG_INPUT_HARDENING_ARGS: &[&str] = &[
    "-nostdin",
    "-hide_banner",
    "-protocol_whitelist",
    "file",
    "-format_whitelist",
    "mov,matroska,webm,avi,asf,flv,mpegts,mpeg,mpegvideo,m4v,ogg,rm,h264,hevc,dv,nut,ivf,obu",
    "-codec_whitelist",
    "h264,hevc,av1,libdav1d,libaom-av1,vp8,vp9,libvpx,libvpx-vp9,mpeg4,mpeg1video,mpeg2video,msmpeg4,msmpeg4v1,msmpeg4v2,wmv1,wmv2,wmv3,vc1,flv,h263,h263p,theora,vp6,vp6f,vp6a,rv10,rv20,rv30,rv40,mjpeg,prores,dvvideo",
    "-probesize",
    "1048576",
    "-analyzeduration",
    "1000000",
];

/// [`FFMPEG_INPUT_HARDENING_ARGS`] with `-protocol_whitelist` set to `protocol` (`file` for paths,
/// `pipe` when the input bytes are fed through stdin). Everything else stays identical.
pub(crate) fn ffmpeg_input_hardening_args(protocol: &'static str) -> Vec<&'static str> {
    let mut args = FFMPEG_INPUT_HARDENING_ARGS.to_vec();
    if let Some(i) = args.iter().position(|a| *a == "-protocol_whitelist") {
        if i + 1 < args.len() {
            args[i + 1] = protocol;
        }
    }
    args
}

fn read_pipe_bounded<R: std::io::Read>(mut r: R, max_bytes: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let remaining = max_bytes.saturating_sub(out.len());
                if remaining > 0 {
                    out.extend_from_slice(&buf[..n.min(remaining)]);
                }
                // Continue draining if needed so child doesn't block on pipe write,
                // or break once cap is hit if caller will kill on timeout.
                if out.len() >= max_bytes {
                    break;
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    out
}

fn run_ffmpeg_bounded(
    cmd: &mut std::process::Command,
    timeout: std::time::Duration,
    max_stdout: usize,
    max_stderr: usize,
) -> Option<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    run_ffmpeg_bounded_with_input(cmd, timeout, max_stdout, max_stderr, None)
}

/// [`run_ffmpeg_bounded`] that optionally writes `input` to the child's stdin (from a separate
/// thread, so a child that stops reading early can never dead-lock the caller).
fn run_ffmpeg_bounded_with_input(
    cmd: &mut std::process::Command,
    timeout: std::time::Duration,
    max_stdout: usize,
    max_stderr: usize,
    input: Option<std::sync::Arc<Vec<u8>>>,
) -> Option<(std::process::ExitStatus, Vec<u8>, Vec<u8>)> {
    use std::process::Stdio;
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Start the child suspended so it is confined to the restrictive Job Object *before* it
    // executes a single instruction of the (untrusted-input-parsing) decoder.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_SUSPENDED: u32 = 0x0000_0004;
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }
    let mut child = cmd.spawn().ok()?;
    #[cfg(windows)]
    let _job_guard = {
        struct JobGuard(Option<windows::Win32::Foundation::HANDLE>);
        impl Drop for JobGuard {
            fn drop(&mut self) {
                if let Some(h) = self.0 {
                    unsafe {
                        let _ = windows::Win32::Foundation::CloseHandle(h);
                    }
                }
            }
        }
        let job = apply_least_privilege_job_and_token(&child);
        if job.is_none() {
            // Fail closed: never let FFmpeg run outside the sandbox.
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        let guard = JobGuard(job);
        if !resume_suspended_process(child.id()) {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        guard
    };
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
        std::thread::spawn(move || {
            use std::io::Write;
            // Errors (e.g. broken pipe once FFmpeg has its frame) simply end the feed.
            let _ = stdin.write_all(&data);
            drop(stdin);
        });
    }

    let out_handle = std::thread::spawn(move || {
        stdout_pipe
            .map(|s| read_pipe_bounded(s, max_stdout))
            .unwrap_or_default()
    });
    let err_handle = std::thread::spawn(move || {
        stderr_pipe
            .map(|s| read_pipe_bounded(s, max_stderr))
            .unwrap_or_default()
    });

    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };

    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();
    status.map(|st| (st, stdout, stderr))
}

/// Builds the `-i` argument for FFmpeg.
///
/// The absolute path is always prefixed with the explicit `file:` protocol so that FFmpeg never
/// interprets a crafted file name as an option (`-...`) or as another protocol/URL scheme
/// (`concat:`, `subfile:`, `http:` ...).
pub(crate) fn safe_ffmpeg_input_arg(path: &Path) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    format!("file:{}", abs.to_string_lossy())
}

fn probe_ffmpeg_info(ffmpeg_bin: &Path, path_str: &str, timeout: std::time::Duration) -> (f64, bool) {
    use std::process::Command;
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    #[cfg(windows)]
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let mut duration = 0.0;
    let mut is_hdr = false;

    let mut cmd = Command::new(ffmpeg_bin);
    prepare_ffmpeg_sandbox(&mut cmd, ffmpeg_bin);
    cmd.args(FFMPEG_INPUT_HARDENING_ARGS);
    cmd.args(&["-i", path_str]);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    if let Some((_status, _stdout, stderr)) = run_ffmpeg_bounded(&mut cmd, timeout, 64 * 1024, 512 * 1024) {
        (duration, is_hdr) = parse_probe_stderr(&stderr);
    }
    (duration, is_hdr)
}

/// Seek target for the FFmpeg pipelines, from the user's frame-selection parameters
/// (dynamic ratio / percentage / fixed offset, threshold, fixed seconds).
fn ffmpeg_target_seek_sec(duration_sec: f64, config: &SmartRenderConfig) -> f64 {
    if duration_sec > 5.0 {
        match config.seek_mode {
            1 => {
                let fixed = config.seek_fixed_sec as f64;
                if duration_sec > fixed + 5.0 {
                    fixed
                } else {
                    let pct = (config.position_seek_percent as f64 / 100.0).clamp(0.05, 0.90);
                    (duration_sec * pct).clamp(1.0, duration_sec - 1.0)
                }
            }
            2 => {
                let threshold_sec = config.seek_threshold_min as f64 * 60.0;
                if duration_sec < threshold_sec {
                    let pct = (config.position_seek_percent as f64 / 100.0).clamp(0.05, 0.90);
                    (duration_sec * pct).clamp(1.0, duration_sec - 1.0)
                } else {
                    let fixed = config.seek_fixed_sec as f64;
                    if duration_sec > fixed + 5.0 {
                        fixed
                    } else {
                        let pct = (config.position_seek_percent as f64 / 100.0).clamp(0.05, 0.90);
                        (duration_sec * pct).clamp(1.0, duration_sec - 1.0)
                    }
                }
            }
            _ => {
                let pct = (config.position_seek_percent as f64 / 100.0).clamp(0.05, 0.90);
                (duration_sec * pct).clamp(1.0, duration_sec - 1.0)
            }
        }
    } else if config.seek_fixed_sec > 0 {
        config.seek_fixed_sec as f64
    } else {
        60.0
    }
}

/// Next probe position after an unacceptable frame: step forward, wrapping near the end.
fn ffmpeg_next_seek_sec(current: f64, step_sec: f64, duration_sec: f64) -> f64 {
    let mut next = current + step_sec;
    if duration_sec > 0.0 && next >= duration_sec - 2.0 {
        next = (duration_sec * 0.15)
            .max(5.0_f64.min((duration_sec * 0.5).max(0.5)))
            .min((duration_sec - 2.0).max(0.5));
    }
    next
}

/// Duration and HDR hints from FFmpeg's banner on stderr.
fn parse_probe_stderr(stderr: &[u8]) -> (f64, bool) {
    let mut duration = 0.0;
    let err_text = String::from_utf8_lossy(stderr);
    if let Some(pos) = err_text.find("Duration: ") {
        let sub = &err_text[pos + 10..];
        let dur_str = sub.split(',').next().unwrap_or("").trim();
        let parts: Vec<&str> = dur_str.split(':').collect();
        if parts.len() == 3 {
            let h: f64 = parts[0].parse().unwrap_or(0.0);
            let m: f64 = parts[1].parse().unwrap_or(0.0);
            let s: f64 = parts[2].parse().unwrap_or(0.0);
            duration = h * 3600.0 + m * 60.0 + s;
        }
    }
    let lower = err_text.to_lowercase();
    let is_hdr = ["bt2020", "smpte2084", "arib-std-b67", "mastering display", "content light", "yuv420p10", "yuv422p10", "yuv444p10"]
        .iter()
        .any(|k| lower.contains(k));
    (duration, is_hdr)
}

fn extract_video_thumbnail_ffmpeg(
    path: &Path,
    side: u32,
    config: &SmartRenderConfig,
) -> Result<(RgbaImage, VideoExtractReport), String> {
    use std::process::Command;
    #[cfg(windows)]
    use std::os::windows::process::CommandExt;
    #[cfg(windows)]
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let ffmpeg_bin = match find_ffmpeg_path() {
        Some(p) if is_trusted_ffmpeg_bin(&p, path) => p,
        Some(_) => {
            return Err("Rejected untrusted FFmpeg binary path".to_string());
        }
        None => {
            return Err(crate::i18n::t("error.ffmpeg_missing"));
        }
    };

    let path_str = safe_ffmpeg_input_arg(path);
    let total_timeout = std::time::Duration::from_secs(config.watchdog_timeout_sec.clamp(3, 60) as u64);
    let probe_timeout = total_timeout.min(std::time::Duration::from_secs(5));
    let (duration_sec, is_hdr) = probe_ffmpeg_info(&ffmpeg_bin, &path_str, probe_timeout);
    let tonemap_active = config.hdr_tonemap_enabled && is_hdr;

    let target_seek_sec = ffmpeg_target_seek_sec(duration_sec, config);

    let step_sec = (config.step_probe_sec as f64).clamp(1.0, 60.0);
    let max_attempts = config.consecutive_unacceptable_limit.clamp(1, 20);
    let budget_ms = ((config.watchdog_timeout_sec.clamp(3, 60) as u128) * 1000)
        .saturating_sub(500)
        .max(1500);
    let loop_start = std::time::Instant::now();
    let mut current_seek_sec = target_seek_sec;
    let mut best_candidate: Option<(RgbaImage, VideoExtractReport)> = None;
    let mut best_score = f32::MIN;
    let mut last_ffmpeg_error = String::new();

    // Use FFmpeg filter to scale directly to thumbnail dimensions using SIMD swscale,
    // and encode as high-quality MJPEG to avoid slow PNG compression overhead.
    let vf_arg = format!("scale=min(iw\\,{side}):-1");

    for attempt in 1..=max_attempts {
        if attempt > 1 && loop_start.elapsed().as_millis() >= budget_ms {
            break;
        }
        let mut cmd = Command::new(&ffmpeg_bin);
        prepare_ffmpeg_sandbox(&mut cmd, &ffmpeg_bin);
        cmd.args(FFMPEG_INPUT_HARDENING_ARGS);
        cmd.args(&[
            "-ss", &format!("{:.2}", current_seek_sec),
            "-i", &path_str,
            // Only the video stream is needed: never instantiate audio/subtitle/data decoders.
            "-an", "-sn", "-dn",
            "-vf", &vf_arg,
            "-vframes", "1",
            "-f", "image2pipe",
            "-vcodec", "mjpeg",
            "-q:v", "2",
            "-",
        ]);
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        let remaining_ms = budget_ms.saturating_sub(loop_start.elapsed().as_millis()).max(500);
        let step_timeout = std::time::Duration::from_millis(remaining_ms.min(6000) as u64);
        if let Some((status, stdout, stderr)) = run_ffmpeg_bounded(&mut cmd, step_timeout, 16 * 1024 * 1024, 256 * 1024) {
            if !status.success() || stdout.is_empty() {
                last_ffmpeg_error = last_stderr_line(&stderr);
            }
            if status.success() && !stdout.is_empty() {
                let mut reader = image::ImageReader::new(std::io::Cursor::new(&stdout));
                if let Ok(guessed) = reader.with_guessed_format() {
                    let mut lim = image::Limits::default();
                    lim.max_image_width = Some(crate::image_dec::MAX_IMAGE_DIMENSION);
                    lim.max_image_height = Some(crate::image_dec::MAX_IMAGE_DIMENSION);
                    lim.max_alloc = Some(crate::image_dec::MAX_DECODER_ALLOC_BYTES);
                    reader = guessed;
                    reader.limits(lim);
                    if let Ok(dyn_img) = reader.decode() {
                    let orig_w = dyn_img.width();
                    let orig_h = dyn_img.height();
                    let mut rgba = dyn_img.to_rgba8();

                    if tonemap_active {
                        for pixel in rgba.chunks_exact_mut(4) {
                            let (r, g, b) = aces_filmic_tone_mapping(
                                pixel[0] as f32 / 255.0,
                                pixel[1] as f32 / 255.0,
                                pixel[2] as f32 / 255.0,
                            );
                            pixel[0] = r;
                            pixel[1] = g;
                            pixel[2] = b;
                        }
                    }

                    let metrics = super::smart_render::evaluate_frame(&rgba, false, config);

                    let report = VideoExtractReport {
                        metrics,
                        attempts_made: attempt,
                        duration_seconds: duration_sec,
                        selected_time_seconds: current_seek_sec,
                        original_width: orig_w,
                        original_height: orig_h,
                    };

                    if metrics.is_accepted {
                        return Ok((rgba, report));
                    }

                    if metrics.score > best_score {
                        best_score = metrics.score;
                        best_candidate = Some((rgba, report));
                    }
                    }
                }
            }
        }

        current_seek_sec = ffmpeg_next_seek_sec(current_seek_sec, step_sec, duration_sec);
    }

    if let Some(best) = best_candidate {
        return Ok(best);
    }

    if last_ffmpeg_error.is_empty() {
        Err("FFmpeg failed to extract any valid candidate frame".to_string())
    } else {
        Err(format!("FFmpeg failed to extract any valid candidate frame ({last_ffmpeg_error})"))
    }
}

/// Last meaningful line of FFmpeg's stderr, bounded and stripped of control characters, so a
/// failure explains itself (e.g. "Option xyz not found.") without dumping the whole log.
fn last_stderr_line(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("Conversion failed"))
        .last()
        .unwrap_or("");
    line.chars().filter(|c| !c.is_control()).take(160).collect()
}

unsafe fn step_reader_position(
    reader: &IMFSourceReader,
    current_probe_sec: &mut f64,
    hop: f64,
    duration_sec: f64,
) -> bool {
    let mut next_seek = *current_probe_sec + hop;
    if duration_sec > 0.0 && next_seek >= duration_sec - 2.0 {
        next_seek = (duration_sec * 0.15)
            .max(5.0_f64.min((duration_sec * 0.5).max(0.5)))
            .min((duration_sec - 2.0).max(0.5));
    }
    *current_probe_sec = next_seek;
    let seek_hns = (*current_probe_sec * 10_000_000.0) as i64;
    let pv_seek = PROPVARIANT::from(seek_hns);
    reader.SetCurrentPosition(&GUID::zeroed(), &pv_seek).is_ok()
}

unsafe fn process_source_reader(
    reader: &IMFSourceReader,
    side: u32,
    config: &SmartRenderConfig,
    _is_stream: bool,
) -> Result<(RgbaImage, VideoExtractReport), String> {
    // 1. Deselect all streams, select only first video stream
    let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
    let _ = reader.SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, true);

    // 2. Query native video dimensions and detect HDR metadata
    let mut is_hdr_video = false;
    let (native_w, native_h) = if let Ok(native_type) =
        reader.GetNativeMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, 0)
    {
        // Check Transfer Function (7 = SMPTE ST 2084 / PQ for HDR10, 8 = HLG for BT.2100)
        if let Ok(tf) = native_type.GetUINT32(&MF_MT_TRANSFER_FUNCTION) {
            if tf == 7 || tf == 8 {
                is_hdr_video = true;
            }
        }
        // Check Primaries (3 = BT.2020 Wide Color Gamut)
        if !is_hdr_video {
            if let Ok(prim) = native_type.GetUINT32(&MF_MT_VIDEO_PRIMARIES) {
                if prim == 3 {
                    is_hdr_video = true;
                }
            }
        }
        // Check Subtype (P010 / P016 for 10-bit/16-bit High Dynamic Range formats)
        if !is_hdr_video {
            if let Ok(sub) = native_type.GetGUID(&MF_MT_SUBTYPE) {
                if sub == MF_VIDEO_FORMAT_P010 || sub == MF_VIDEO_FORMAT_P016 {
                    is_hdr_video = true;
                }
            }
        }

        if let Ok(size_u64) = native_type.GetUINT64(&MF_MT_FRAME_SIZE) {
            ((size_u64 >> 32) as u32, (size_u64 & 0xFFFFFFFF) as u32)
        } else {
            (0, 0)
        }
    } else {
        (0, 0)
    };

    let tonemap_active = config.hdr_tonemap_enabled && is_hdr_video;

    // 3. Negotiate output format: Native NV12 (Direct3D 11/DXVA hardware decoder) -> YUY2 -> RGB32
    let mut selected_fmt = VideoPixelFormat::Nv12;
    let mut negotiated = false;

    // Try native NV12 directly (default on Windows 11 hardware pipeline, zero conversion delay)
    if let Ok(mt_nv12) = MFCreateMediaType() {
        let _ = mt_nv12.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video);
        let _ = mt_nv12.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12);
        if reader.SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &mt_nv12).is_ok() {
            selected_fmt = VideoPixelFormat::Nv12;
            negotiated = true;
        }
    }

    // Fallback to YUY2
    if !negotiated {
        if let Ok(mt_yuy2) = MFCreateMediaType() {
            let _ = mt_yuy2.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video);
            let _ = mt_yuy2.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_YUY2);
            if reader.SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &mt_yuy2).is_ok() {
                selected_fmt = VideoPixelFormat::Yuy2;
                negotiated = true;
            }
        }
    }

    // Fallback to RGB32
    if !negotiated {
        if let Ok(mt_rgb) = MFCreateMediaType() {
            let _ = mt_rgb.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video);
            let _ = mt_rgb.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32);
            if reader.SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &mt_rgb).is_ok() {
                selected_fmt = VideoPixelFormat::Rgb32;
                negotiated = true;
            }
        }
    }

    if !negotiated {
        return Err("SetCurrentMediaType failed: Unable to negotiate NV12, YUY2 or RGB32".to_string());
    }

    // 4. Determine probe seek position:
    // Determine accurate stream/video duration
    let mut duration_hns: i64 = 0;
    if let Ok(prop_duration) =
        reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
    {
        duration_hns = PropVariantToInt64WithDefault(&prop_duration, 0);
    }
    if duration_hns <= 0 {
        if let Ok(prop_duration) =
            reader.GetPresentationAttribute(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, &MF_PD_DURATION)
        {
            duration_hns = PropVariantToInt64WithDefault(&prop_duration, 0);
        }
    }
    let duration_sec = if duration_hns > 0 {
        duration_hns as f64 / 10_000_000.0
    } else {
        0.0
    };

    let target_seek_sec = if duration_sec > 5.0 {
        match config.seek_mode {
            1 => {
                // Fixed seconds offset (e.g. skip first N seconds)
                let fixed = config.seek_fixed_sec as f64;
                if duration_sec > fixed + 5.0 {
                    fixed
                } else {
                    let pct = (config.position_seek_percent as f64 / 100.0).clamp(0.05, 0.90);
                    (duration_sec * pct).clamp(1.0, duration_sec - 1.0)
                }
            }
            2 => {
                // Smart Duration:
                // For TV episodes / standard videos (< threshold_min, default 60m), use percentage (e.g. 33%) to avoid static OP
                // For long movies (>= threshold_min), skip fixed intro seconds (e.g. 120s) to jump past studio logos
                let threshold_sec = config.seek_threshold_min as f64 * 60.0;
                if duration_sec < threshold_sec {
                    let pct = (config.position_seek_percent as f64 / 100.0).clamp(0.05, 0.90);
                    (duration_sec * pct).clamp(1.0, duration_sec - 1.0)
                } else {
                    let fixed = config.seek_fixed_sec as f64;
                    if duration_sec > fixed + 5.0 {
                        fixed
                    } else {
                        let pct = (config.position_seek_percent as f64 / 100.0).clamp(0.05, 0.90);
                        (duration_sec * pct).clamp(1.0, duration_sec - 1.0)
                    }
                }
            }
            _ => {
                // Default Mode 0: Percentage seek (default 33%)
                let pct = (config.position_seek_percent as f64 / 100.0).clamp(0.05, 0.90);
                (duration_sec * pct).clamp(1.0, duration_sec - 1.0)
            }
        }
    } else {
        // Unknown or stream duration: probe forward with fixed seek offset (e.g. 120s or 10s) to prevent 0.0s collapse
        if config.seek_fixed_sec > 0 {
            config.seek_fixed_sec as f64
        } else {
            120.0
        }
    };

    let mut actual_seek_sec = target_seek_sec;
    if target_seek_sec > 0.0 {
        let sec_s = format!("{:.1}", target_seek_sec);
        let msg = crate::i18n::t_fmt("stage.seeking_frame", &[("sec", &sec_s)]);
        crate::monitor::record_task_stage(&msg);
        let seek_hns = (target_seek_sec * 10_000_000.0) as i64;
        let pv_seek = PROPVARIANT::from(seek_hns);
        if reader.SetCurrentPosition(&GUID::zeroed(), &pv_seek).is_err() {
            // If probe at 120s fails on stream, try 10s
            let pv_ten = PROPVARIANT::from(100_000_000i64);
            if reader.SetCurrentPosition(&GUID::zeroed(), &pv_ten).is_ok() {
                actual_seek_sec = 10.0;
            } else {
                let pv_zero = PROPVARIANT::from(0i64);
                let _ = reader.SetCurrentPosition(&GUID::zeroed(), &pv_zero);
                actual_seek_sec = 0.0;
            }
        }
    } else {
        crate::monitor::record_task_stage(&crate::i18n::t("stage.decoding_frame"));
    }

    let loop_start = std::time::Instant::now();
    let mut candidate_raw: Option<(Vec<u8>, u32, u32, u32, FrameMetrics, i64)> = None;
    let mut accepted_image: Option<(RgbaImage, FrameMetrics, i64, u32)> = None;
    let mut attempts = 0u32;
    let mut consecutive_unacceptable = 0u32;
    let mut current_probe_sec = actual_seek_sec;
    let mut last_sample_ts: Option<i64> = None;
    let mut duplicate_keyframe_hops = 0u32;
    let mut rewound_on_eof = false;
    let step_sec = (config.step_probe_sec as f64).clamp(1.0, 60.0);
    let max_attempts = config.consecutive_unacceptable_limit.clamp(1, 20);
    let max_gop_hops = config.max_gop_hops.min(20);
    let budget_ms = ((config.watchdog_timeout_sec.clamp(3, 60) as u128) * 1000)
        .saturating_sub(500)
        .max(1500);

    // Keyframe & Scene Step Probe:
    // Seek straight across distinct GOP keyframes whenever a candidate fails luminance/entropy filters,
    // guarding against MF SetCurrentPosition snap-back onto an earlier keyframe.
    loop {
        if attempts >= max_attempts {
            break;
        }
        if attempts >= 1 && loop_start.elapsed().as_millis() >= budget_ms {
            break;
        }

        let mut actual_stream_flags = 0u32;
        let mut sample_timestamp = 0i64;
        let mut sample_opt: Option<IMFSample> = None;

        let hr = reader.ReadSample(
            MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
            0,
            None,
            Some(&mut actual_stream_flags),
            Some(&mut sample_timestamp),
            Some(&mut sample_opt),
        );

        let is_eof = (actual_stream_flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32) != 0;
        if hr.is_err() || is_eof {
            // If seek was too far and hit EOF early, rewind to 0s once
            if !rewound_on_eof && attempts <= 2 && actual_seek_sec > 0.0 {
                rewound_on_eof = true;
                let pv_zero = PROPVARIANT::from(0i64);
                let _ = reader.SetCurrentPosition(&GUID::zeroed(), &pv_zero);
                actual_seek_sec = 0.0;
                current_probe_sec = 0.0;
                last_sample_ts = None;
                duplicate_keyframe_hops = 0;
                continue;
            }
            break;
        }

        let Some(sample) = sample_opt else {
            // Seek flush/preroll or stream tick: wait for next decoded sample
            continue;
        };

        // Detect Media Foundation keyframe snap-back so a long GOP does not stall retries on the same I-frame.
        if let Some(prev_ts) = last_sample_ts {
            if (sample_timestamp - prev_ts).abs() <= 5_000_000 && duplicate_keyframe_hops < max_gop_hops {
                duplicate_keyframe_hops += 1;
                if step_reader_position(reader, &mut current_probe_sec, step_sec, duration_sec) {
                    continue;
                }
            }
        }
        last_sample_ts = Some(sample_timestamp);
        duplicate_keyframe_hops = 0;
        let sample_sec = (sample_timestamp as f64 / 10_000_000.0).max(0.0);
        current_probe_sec = current_probe_sec.max(sample_sec);

        attempts += 1;

        // Extract frame width, height, and stride from current media type
        let (width, height, stride_hint) = if let Ok(current_type) =
            reader.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
        {
            let (w, h) = if let Ok(size_u64) = current_type.GetUINT64(&MF_MT_FRAME_SIZE) {
                ((size_u64 >> 32) as u32, (size_u64 & 0xFFFFFFFF) as u32)
            } else {
                (native_w, native_h)
            };
            let s = if let Ok(stride_val) = current_type.GetUINT32(&MF_MT_DEFAULT_STRIDE) {
                (stride_val as i32).unsigned_abs()
            } else {
                0
            };
            (w, h, s)
        } else {
            (native_w, native_h, 0)
        };

        if width == 0 || height == 0 || width > 16384 || height > 16384 {
            continue;
        }

        let Ok(buffer) = sample.ConvertToContiguousBuffer() else {
            continue;
        };

        let mut p_data: *mut u8 = std::ptr::null_mut();
        let mut cur_len = 0u32;
        let mut max_len = 0u32;
        if buffer.Lock(&mut p_data, Some(&mut max_len), Some(&mut cur_len)).is_err() {
            continue;
        }

        if p_data.is_null() {
            let _ = buffer.Unlock();
            continue;
        }

        let raw_slice = std::slice::from_raw_parts(p_data, cur_len as usize);

        // Fast zero-allocation evaluation
        let (lum, ent) = match selected_fmt {
            VideoPixelFormat::Nv12 => {
                let y_len = (width as usize) * (height as usize);
                if raw_slice.len() < y_len {
                    let _ = buffer.Unlock();
                    continue;
                }
                compute_luminance_and_entropy_y_plane(&raw_slice[..y_len])
            }
            VideoPixelFormat::Rgb32 => {
                let expected = (width as usize) * (height as usize) * 4;
                if raw_slice.len() < expected {
                    let _ = buffer.Unlock();
                    continue;
                }
                compute_luminance_and_entropy_bgra(&raw_slice[..expected])
            }
            VideoPixelFormat::Yuy2 => {
                let expected = (width as usize) * (height as usize) * 2;
                if raw_slice.len() < expected {
                    let _ = buffer.Unlock();
                    continue;
                }
                compute_luminance_and_entropy_yuy2(&raw_slice[..expected])
            }
        };

        let metrics = evaluate_metrics(lum, ent, config);

        if metrics.is_accepted {
            // ACCEPTED! Direct zero-copy single-pass downsample directly to target thumbnail
            let rgba = match selected_fmt {
                VideoPixelFormat::Nv12 => nv12_to_thumbnail_direct(raw_slice, width, height, stride_hint, side, tonemap_active),
                VideoPixelFormat::Rgb32 => bgra_to_thumbnail_direct(raw_slice, width, height, stride_hint, side, tonemap_active),
                VideoPixelFormat::Yuy2 => yuy2_to_thumbnail_direct(raw_slice, width, height, stride_hint, side, tonemap_active),
            };
            let _ = buffer.Unlock();
            accepted_image = Some((rgba, metrics, sample_timestamp, attempts));
            break;
        } else {
            consecutive_unacceptable += 1;
            // Check if better candidate
            let is_better = match &candidate_raw {
                None => true,
                Some((_, _, _, _, prev_m, _)) => metrics.score > prev_m.score,
            };
            if is_better {
                candidate_raw = Some((raw_slice.to_vec(), width, height, stride_hint, metrics, sample_timestamp));
            }
            let _ = buffer.Unlock();

            if consecutive_unacceptable >= max_attempts && candidate_raw.is_some() {
                break;
            }

            // Step forward immediately on every rejected frame so we inspect a distinct keyframe next.
            let _ = step_reader_position(reader, &mut current_probe_sec, step_sec, duration_sec);
        }
    }

    let (final_rgba, final_metrics, final_ts, final_attempts) = if let Some(accepted) = accepted_image {
        accepted
    } else if let Some((raw, w, h, s, m, ts)) = candidate_raw {
        let rgba = match selected_fmt {
            VideoPixelFormat::Nv12 => nv12_to_thumbnail_direct(&raw, w, h, s, side, tonemap_active),
            VideoPixelFormat::Rgb32 => bgra_to_thumbnail_direct(&raw, w, h, s, side, tonemap_active),
            VideoPixelFormat::Yuy2 => yuy2_to_thumbnail_direct(&raw, w, h, s, side, tonemap_active),
        };
        (rgba, m, ts, attempts)
    } else {
        return Err("Failed to extract any valid candidate frame from video stream".to_string());
    };

    let report = VideoExtractReport {
        metrics: final_metrics,
        attempts_made: final_attempts,
        duration_seconds: duration_sec,
        selected_time_seconds: final_ts as f64 / 10_000_000.0,
        original_width: native_w.max(final_rgba.width()),
        original_height: native_h.max(final_rgba.height()),
    };

    Ok((final_rgba, report))
}

fn clean_green_border_artifacts(img: &mut RgbaImage) {
    let w = img.width();
    let h = img.height();
    if w <= 24 || h <= 24 {
        return;
    }

    // YUV (0,0,0) converts to RGB roughly around (0, 135, 0) or elevated green tint.
    // NV12 chroma subsampling padding also creates cyan/blue-green artifacts (e.g. R=79, G=181, B=221)
    // or pitch black padding lines (R=0, G=0, B=0).
    let is_edge_artifact = |p: &image::Rgba<u8>| -> bool {
        let r = p[0] as u32;
        let g = p[1] as u32;
        let b = p[2] as u32;
        // 1. Green artifact
        let is_green = (g >= 40 && g > r + 12 && g > b + 12) || (g >= 70 && g * 2 > r * 3 + b * 3);
        // 2. Cyan / blue-green Chroma padding artifact
        let is_cyan = g >= 45 && b >= 45 && (g + b) > r * 2 + 25 && g > r + 12;
        // 3. Pure black padding line
        let is_black = r <= 6 && g <= 6 && b <= 6;
        is_green || is_cyan || is_black
    };

    let scan_depth = 12.min(h / 4) as usize;
    let ref_y = h - 1 - scan_depth as u32;

    // Clean bottom rows
    for y_from_bottom in 0..scan_depth {
        let y = h - 1 - y_from_bottom as u32;
        let mut artifact_count = 0;
        let step = (w / 32).max(1);
        let mut checked = 0;
        for x in (0..w).step_by(step as usize) {
            checked += 1;
            if is_edge_artifact(img.get_pixel(x, y)) {
                artifact_count += 1;
            }
        }
        if artifact_count * 10 >= checked * 3 || is_edge_artifact(img.get_pixel(0, y)) && is_edge_artifact(img.get_pixel(w - 1, y)) {
            for x in 0..w {
                let safe_px = *img.get_pixel(x, ref_y);
                img.put_pixel(x, y, safe_px);
            }
        }
    }

    // Clean rightmost columns
    let scan_w = 10.min(w / 4) as usize;
    let ref_x = w - 1 - scan_w as u32;
    for x_from_right in 0..scan_w {
        let x = w - 1 - x_from_right as u32;
        let mut artifact_count = 0;
        let step = (h / 32).max(1);
        let mut checked = 0;
        for y in (0..h).step_by(step as usize) {
            checked += 1;
            if is_edge_artifact(img.get_pixel(x, y)) {
                artifact_count += 1;
            }
        }
        if artifact_count * 10 >= checked * 3 || is_edge_artifact(img.get_pixel(x, 0)) && is_edge_artifact(img.get_pixel(x, h - 1)) {
            for y in 0..h {
                let safe_px = *img.get_pixel(ref_x, y);
                img.put_pixel(x, y, safe_px);
            }
        }
    }
}

fn nv12_to_thumbnail_direct(data: &[u8], src_w: u32, src_h: u32, stride_hint: u32, target_side: u32, tonemap: bool) -> RgbaImage {
    let aspect = src_w as f32 / src_h as f32;
    let (dst_w, dst_h) = if aspect >= 1.0 {
        let nw = target_side.min(src_w);
        let nh = ((nw as f32 / aspect).round() as u32).max(1);
        (nw, nh)
    } else {
        let nh = target_side.min(src_h);
        let nw = ((nh as f32 * aspect).round() as u32).max(1);
        (nw, nh)
    };

    let sw = src_w as usize;
    let sh = src_h as usize;
    let dw = dst_w as usize;
    let dh = dst_h as usize;

    let stride = if stride_hint >= src_w {
        stride_hint as usize
    } else if data.len() >= (sw * sh * 3 / 2) {
        ((data.len() * 2) / (sh * 3)).max(sw)
    } else {
        sw
    };

    let y_plane_len = stride * sh;
    let y_plane = if data.len() >= y_plane_len {
        &data[..y_plane_len]
    } else {
        &data[..data.len().min(sw * sh)]
    };
    let uv_plane = if data.len() > y_plane_len {
        &data[y_plane_len..]
    } else {
        &data[data.len().min(sw * sh)..]
    };

    let mut img = RgbaImage::new(dst_w, dst_h);

    let x_scale = (src_w as u64 * 65536) / (dst_w as u64);
    let y_scale = (src_h as u64 * 65536) / (dst_h as u64);

    let max_sx = sw.saturating_sub(2).max(1);
    let max_sy = sh.saturating_sub(2).max(1);

    for dy in 0..dh {
        let sy = (((dy as u64 * y_scale) >> 16) as usize).min(max_sy);
        let y_row = sy * stride;
        let uv_row = (sy / 2) * stride;

        for dx in 0..dw {
            let sx = (((dx as u64 * x_scale) >> 16) as usize).min(max_sx);
            let y_idx = y_row + sx;
            let y_val = y_plane.get(y_idx).copied().unwrap_or(0) as f32;

            let uv_idx = uv_row + (sx & !1);
            let u_val = uv_plane.get(uv_idx).copied().unwrap_or(128) as f32 - 128.0;
            let v_val = uv_plane.get(uv_idx + 1).copied().unwrap_or(128) as f32 - 128.0;

            let r = (y_val + 1.402 * v_val).clamp(0.0, 255.0);
            let g = (y_val - 0.344136 * u_val - 0.714136 * v_val).clamp(0.0, 255.0);
            let b = (y_val + 1.772 * u_val).clamp(0.0, 255.0);

            let (final_r, final_g, final_b) = if tonemap {
                aces_filmic_tone_mapping(r / 255.0, g / 255.0, b / 255.0)
            } else {
                (r as u8, g as u8, b as u8)
            };

            img.put_pixel(dx as u32, dy as u32, Rgba([final_r, final_g, final_b, 255]));
        }
    }

    clean_green_border_artifacts(&mut img);
    img
}

fn bgra_to_thumbnail_direct(data: &[u8], src_w: u32, src_h: u32, stride_hint: u32, target_side: u32, tonemap: bool) -> RgbaImage {
    let aspect = src_w as f32 / src_h as f32;
    let (dst_w, dst_h) = if aspect >= 1.0 {
        let nw = target_side.min(src_w);
        let nh = ((nw as f32 / aspect).round() as u32).max(1);
        (nw, nh)
    } else {
        let nh = target_side.min(src_h);
        let nw = ((nh as f32 * aspect).round() as u32).max(1);
        (nw, nh)
    };

    let sw = src_w as usize;
    let sh = src_h as usize;
    let dw = dst_w as usize;
    let dh = dst_h as usize;

    let stride = if stride_hint >= src_w * 4 {
        stride_hint as usize
    } else if data.len() >= (sw * sh * 4) {
        (data.len() / sh).max(sw * 4)
    } else {
        sw * 4
    };

    let mut img = RgbaImage::new(dst_w, dst_h);
    let x_scale = (src_w as u64 * 65536) / (dst_w as u64);
    let y_scale = (src_h as u64 * 65536) / (dst_h as u64);

    let max_sx = sw.saturating_sub(2).max(1);
    let max_sy = sh.saturating_sub(2).max(1);

    for dy in 0..dh {
        let sy = (((dy as u64 * y_scale) >> 16) as usize).min(max_sy);
        let row_start = sy * stride;

        for dx in 0..dw {
            let sx = (((dx as u64 * x_scale) >> 16) as usize).min(max_sx);
            let idx = row_start + sx * 4;
            if idx + 3 < data.len() {
                let b = data[idx];
                let g = data[idx + 1];
                let r = data[idx + 2];
                let a = data[idx + 3];

                let (final_r, final_g, final_b) = if tonemap {
                    aces_filmic_tone_mapping(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0)
                } else {
                    (r, g, b)
                };

                img.put_pixel(dx as u32, dy as u32, Rgba([final_r, final_g, final_b, if a == 0 { 255 } else { a }]));
            }
        }
    }

    clean_green_border_artifacts(&mut img);
    img
}

fn yuy2_to_thumbnail_direct(data: &[u8], src_w: u32, src_h: u32, stride_hint: u32, target_side: u32, tonemap: bool) -> RgbaImage {
    let aspect = src_w as f32 / src_h as f32;
    let (dst_w, dst_h) = if aspect >= 1.0 {
        let nw = target_side.min(src_w);
        let nh = ((nw as f32 / aspect).round() as u32).max(1);
        (nw, nh)
    } else {
        let nh = target_side.min(src_h);
        let nw = ((nh as f32 * aspect).round() as u32).max(1);
        (nw, nh)
    };

    let sw = src_w as usize;
    let sh = src_h as usize;
    let dw = dst_w as usize;
    let dh = dst_h as usize;

    let stride = if stride_hint >= src_w * 2 {
        stride_hint as usize
    } else if data.len() >= (sw * sh * 2) {
        (data.len() / sh).max(sw * 2)
    } else {
        sw * 2
    };

    let mut img = RgbaImage::new(dst_w, dst_h);
    let x_scale = (src_w as u64 * 65536) / (dst_w as u64);
    let y_scale = (src_h as u64 * 65536) / (dst_h as u64);

    let max_sx = sw.saturating_sub(2).max(1);
    let max_sy = sh.saturating_sub(2).max(1);

    for dy in 0..dh {
        let sy = (((dy as u64 * y_scale) >> 16) as usize).min(max_sy);
        let row_start = sy * stride;

        for dx in 0..dw {
            let sx = (((dx as u64 * x_scale) >> 16) as usize).min(max_sx);
            let pair_start = row_start + (sx & !1) * 2;
            if pair_start + 3 < data.len() {
                let y_val = if sx & 1 == 0 { data[pair_start] as f32 } else { data[pair_start + 2] as f32 };
                let u_val = data[pair_start + 1] as f32 - 128.0;
                let v_val = data[pair_start + 3] as f32 - 128.0;

                let r = (y_val + 1.402 * v_val).clamp(0.0, 255.0);
                let g = (y_val - 0.344136 * u_val - 0.714136 * v_val).clamp(0.0, 255.0);
                let b = (y_val + 1.772 * u_val).clamp(0.0, 255.0);

                let (final_r, final_g, final_b) = if tonemap {
                    aces_filmic_tone_mapping(r / 255.0, g / 255.0, b / 255.0)
                } else {
                    (r as u8, g as u8, b as u8)
                };

                img.put_pixel(dx as u32, dy as u32, Rgba([final_r, final_g, final_b, 255]));
            }
        }
    }

    clean_green_border_artifacts(&mut img);
    img
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::Shell::SHCreateStreamOnFileEx;
    use windows::Win32::System::Com::STGM_READ;
    use windows::core::PCWSTR;

    #[test]
    fn test_stream_primary_video() {
        let p = match std::env::var("CBX_TEST_VIDEO") {
            Ok(val) if !val.is_empty() => val,
            _ => return,
        };
        let wide: Vec<u16> = p.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let stream = SHCreateStreamOnFileEx(
                PCWSTR(wide.as_ptr()),
                STGM_READ.0,
                0,
                false,
                None,
            ).unwrap();
            let settings = crate::settings::Settings::load();
            let hbmp_res = crate::create_universal_thumbnail(&stream, 256, &settings, Some("sample.mp4"));
            println!("create_universal_thumbnail res: {:?}", hbmp_res.is_ok());
            if let Ok(hbmp) = hbmp_res {
                println!("Got valid HBITMAP: {:?}", !hbmp.is_invalid());
            } else if let Err(e) = hbmp_res {
                println!("Error was: {}", e);
            }
        }
    }

    #[test]
    fn test_stream_secondary_video() {
        let p = match std::env::var("CBX_TEST_VIDEO_2") {
            Ok(val) if !val.is_empty() => val,
            _ => return,
        };
        let wide: Vec<u16> = p.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            let stream = SHCreateStreamOnFileEx(
                PCWSTR(wide.as_ptr()),
                STGM_READ.0,
                0,
                false,
                None,
            ).unwrap();
            let config = SmartRenderConfig::default();
            let sw = std::time::Instant::now();
            let res = extract_video_thumbnail_from_stream(&stream, 256, &config);
            println!("stream extract took {:?}, success={}", sw.elapsed(), res.is_ok());
        }
    }
}

