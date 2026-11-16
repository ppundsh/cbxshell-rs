use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use windows::core::PCWSTR;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, FILE_MAP_ALL_ACCESS, PAGE_READWRITE,
};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

pub const MONITOR_MAGIC: u32 = 0x4D584243; // "CBXM"
pub const MONITOR_VERSION: u32 = 9;
pub const MAX_HISTORY: usize = 64;
const MAX_ACTIVE_SLOTS: usize = 16;
const ACTIVE_TASK_MAX_AGE_MS: u64 = 20_000;
const SHARED_MEM_NAME: &str = "Local\\CBXShell_LiveMonitor_Map_v9\0";

static CACHED_VIEW: AtomicUsize = AtomicUsize::new(0);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MonitorActiveSlot {
    pub task_id: u64,
    pub pid: u32,
    pub _pad: u32,
    pub start_epoch_ms: u64,
    pub file_name: [u16; 384],
    pub stage: [u16; 64],
}

impl MonitorActiveSlot {
    const fn empty() -> Self {
        Self {
            task_id: 0,
            pid: 0,
            _pad: 0,
            start_epoch_ms: 0,
            file_name: [0; 384],
            stage: [0; 64],
        }
    }
}

#[repr(C)]
pub struct MonitorSharedHeader {
    pub magic: u32,
    pub version: u32,
    pub lock: AtomicU32,
    pub active_tasks: u32,
    pub total_processed: u64,
    pub total_duration_ms: u64,
    pub current_start_time_epoch_ms: u64,
    pub current_file: [u16; 384],
    pub current_stage: [u16; 64],
    pub next_task_id: u64,
    pub active_slots: [MonitorActiveSlot; MAX_ACTIVE_SLOTS],
    pub history_head: u32,
    pub history_count: u32,
    pub history: [MonitorHistoryItem; MAX_HISTORY],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MonitorHistoryItem {
    pub timestamp_epoch_ms: u64,
    pub duration_ms: u32,
    pub success: u32, // 1: success, 0: failure (u32 prevents ABI alignment padding ambiguity)
    pub ext: [u16; 16],
    pub file_name: [u16; 384],
    pub pipeline: [u16; 24],
    pub search_trace: [u16; 128],
    pub details: [u16; 128],
}

#[derive(Debug, Clone)]
pub struct MonitorSnapshot {
    pub active_tasks: u32,
    pub total_processed: u64,
    pub avg_duration_ms: u32,
    pub current_file: String,
    pub current_stage: String,
    pub current_elapsed_ms: u64,
    pub history: Vec<SnapshotHistoryItem>,
}

#[derive(Debug, Clone)]
pub struct SnapshotHistoryItem {
    pub time_str: String,
    pub ext: String,
    pub file_name: String,
    pub pipeline: String,
    pub search_trace: String,
    pub duration_ms: u32,
    pub success: bool,
    pub details: String,
}

impl SnapshotHistoryItem {
    pub fn display_name(&self) -> &str {
        if let Some(pos) = self.file_name.rfind('\\').or_else(|| self.file_name.rfind('/')) {
            &self.file_name[pos + 1..]
        } else {
            &self.file_name
        }
    }
}

pub struct TaskGuard {
    pub task_id: u64,
    pub start_time: Instant,
    pub file_name: String,
    pub ext: String,
    pub finished: bool,
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.finished = true;
            let duration_ms = self.start_time.elapsed().as_millis() as u32;
            let now_epoch_ms = current_epoch_ms();

            with_shared_mem(|header| {
                acquire_lock(&header.lock);
                for slot in header.active_slots.iter_mut() {
                    if slot.task_id == self.task_id {
                        *slot = MonitorActiveSlot::empty();
                    }
                }
                sync_and_reap_active_slots(header, now_epoch_ms);
                header.total_processed = header.total_processed.saturating_add(1);
                header.total_duration_ms = header.total_duration_ms.saturating_add(duration_ms as u64);
                let idx = (header.history_head as usize) % MAX_HISTORY;
                header.history[idx] = MonitorHistoryItem {
                    timestamp_epoch_ms: now_epoch_ms,
                    duration_ms,
                    success: 0,
                    ext: str_to_u16_buf(&self.ext),
                    file_name: str_to_u16_buf(&self.file_name),
                    pipeline: str_to_u16_buf(&crate::i18n::t("pipeline.stream_rpc")),
                    search_trace: str_to_u16_buf(&crate::i18n::t("trace.worker_aborted")),
                    details: str_to_u16_buf(&crate::i18n::t("details.worker_aborted")),
                };
                header.history_head = ((header.history_head + 1) as usize % MAX_HISTORY) as u32;
                if header.history_count < MAX_HISTORY as u32 {
                    header.history_count += 1;
                }
                release_lock(&header.lock);
            });
        }
    }
}

fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if pid == std::process::id() {
        return true;
    }
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        if handle.is_invalid() {
            return false;
        }
        let mut exit_code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut exit_code).is_ok();
        let _ = CloseHandle(handle);
        ok && exit_code == 259 // STILL_ACTIVE
    }
}

fn sync_and_reap_active_slots(header: &mut MonitorSharedHeader, now_ms: u64) {
    let mut count: u32 = 0;
    let mut oldest_idx: Option<usize> = None;
    let mut oldest_ts: u64 = u64::MAX;

    for (i, slot) in header.active_slots.iter_mut().enumerate() {
        if slot.task_id == 0 {
            continue;
        }
        let age_ms = now_ms.saturating_sub(slot.start_epoch_ms);
        if age_ms > ACTIVE_TASK_MAX_AGE_MS || !is_pid_alive(slot.pid) {
            *slot = MonitorActiveSlot::empty();
            continue;
        }
        count += 1;
        if slot.start_epoch_ms < oldest_ts {
            oldest_ts = slot.start_epoch_ms;
            oldest_idx = Some(i);
        }
    }

    header.active_tasks = count;
    if let Some(idx) = oldest_idx {
        let slot = &header.active_slots[idx];
        header.current_start_time_epoch_ms = slot.start_epoch_ms;
        header.current_file = slot.file_name;
        header.current_stage = slot.stage;
    } else {
        header.current_start_time_epoch_ms = 0;
        header.current_file = [0u16; 384];
        header.current_stage = [0u16; 64];
    }
}

fn acquire_lock(lock: &AtomicU32) {
    let start = Instant::now();
    while lock.compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Relaxed).is_err() {
        std::hint::spin_loop();
        if start.elapsed().as_millis() > 50 {
            lock.store(0, Ordering::Release);
            break;
        }
    }
}

fn release_lock(lock: &AtomicU32) {
    lock.store(0, Ordering::Release);
}

fn str_to_u16_buf<const N: usize>(s: &str) -> [u16; N] {
    let mut buf = [0u16; N];
    let mut i = 0;
    for c in s.encode_utf16() {
        if i >= N - 1 {
            break;
        }
        buf[i] = c;
        i += 1;
    }
    buf[i] = 0;
    buf
}

fn u16_buf_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

fn get_view() -> Option<*mut MonitorSharedHeader> {
    let ptr = CACHED_VIEW.load(Ordering::Acquire);
    if ptr != 0 {
        return Some(ptr as *mut MonitorSharedHeader);
    }

    unsafe {
        // Privacy: the monitor history contains full paths of every file Explorer thumbnailed.
        // Back the section with the paging file (INVALID_HANDLE_VALUE) instead of a file in
        // %TEMP%, so browsing history is never persisted to disk and disappears once the last
        // process holding the view exits.
        let size = std::mem::size_of::<MonitorSharedHeader>() as u64;
        let h_file = windows::Win32::Foundation::INVALID_HANDLE_VALUE;

        let name_wide: Vec<u16> = SHARED_MEM_NAME.encode_utf16().collect();
        let h_map = CreateFileMappingW(
            h_file,
            None,
            PAGE_READWRITE,
            0,
            size as u32,
            PCWSTR(name_wide.as_ptr()),
        );

        let Ok(h_map) = h_map else { return None };
        if h_map.is_invalid() {
            return None;
        }

        let p_view = MapViewOfFile(h_map, FILE_MAP_ALL_ACCESS, 0, 0, size as usize);
        if p_view.Value.is_null() {
            let _ = CloseHandle(h_map);
            return None;
        }

        let header = &mut *(p_view.Value as *mut MonitorSharedHeader);
        if header.magic != MONITOR_MAGIC || header.version != MONITOR_VERSION {
            header.magic = MONITOR_MAGIC;
            header.version = MONITOR_VERSION;
            header.lock.store(0, Ordering::Release);
            header.active_tasks = 0;
            header.total_processed = 0;
            header.total_duration_ms = 0;
            header.current_start_time_epoch_ms = 0;
            header.current_file = [0u16; 384];
            header.current_stage = [0u16; 64];
            header.next_task_id = 1;
            header.active_slots = [MonitorActiveSlot::empty(); MAX_ACTIVE_SLOTS];
            header.history_head = 0;
            header.history_count = 0;
            for item in header.history.iter_mut() {
                *item = MonitorHistoryItem {
                    timestamp_epoch_ms: 0,
                    duration_ms: 0,
                    success: 0,
                    ext: [0; 16],
                    file_name: [0; 384],
                    pipeline: [0; 24],
                    search_trace: [0; 128],
                    details: [0; 128],
                };
            }
        }

        CACHED_VIEW.store(p_view.Value as usize, Ordering::Release);
        Some(p_view.Value as *mut MonitorSharedHeader)
    }
}

fn with_shared_mem<F: FnOnce(&mut MonitorSharedHeader)>(f: F) {
    if let Some(ptr) = get_view() {
        unsafe {
            f(&mut *ptr);
        }
    }
}

/// Begins recording an active thumbnail generation task.
pub fn clear_all_history() {
    with_shared_mem(|header| {
        acquire_lock(&header.lock);
        header.total_processed = 0;
        header.total_duration_ms = 0;
        header.active_tasks = 0;
        header.current_start_time_epoch_ms = 0;
        header.current_file = [0u16; 384];
        header.current_stage = [0u16; 64];
        header.active_slots = [MonitorActiveSlot::empty(); MAX_ACTIVE_SLOTS];
        header.history_head = 0;
        header.history_count = 0;
        for item in header.history.iter_mut() {
            *item = MonitorHistoryItem {
                timestamp_epoch_ms: 0,
                duration_ms: 0,
                success: 0,
                ext: [0; 16],
                file_name: [0; 384],
                pipeline: [0; 24],
                search_trace: [0; 128],
                details: [0; 128],
            };
        }
        release_lock(&header.lock);
    });
}

/// Retrieves the current operational stage text.
pub fn get_current_stage() -> String {
    let mut stage = String::new();
    with_shared_mem(|header| {
        acquire_lock(&header.lock);
        sync_and_reap_active_slots(header, current_epoch_ms());
        stage = u16_buf_to_string(&header.current_stage);
        release_lock(&header.lock);
    });
    stage
}

pub fn record_task_begin(file_name: &str, ext: &str) -> Option<TaskGuard> {
    let settings = crate::settings::Settings::load();
    if !settings.monitor_enabled {
        return None;
    }

    let mut assigned_task_id = 1u64;
    let now_ms = current_epoch_ms();
    let pid = std::process::id();

    with_shared_mem(|header| {
        acquire_lock(&header.lock);
        sync_and_reap_active_slots(header, now_ms);

        let task_id = header.next_task_id.wrapping_add(1).max(1);
        header.next_task_id = task_id;
        assigned_task_id = task_id;

        let mut target_idx = header.active_slots.iter().position(|s| s.task_id == 0);
        if target_idx.is_none() {
            // Overwrite oldest active slot if all MAX_ACTIVE_SLOTS are full
            target_idx = header
                .active_slots
                .iter()
                .enumerate()
                .min_by_key(|(_, s)| s.start_epoch_ms)
                .map(|(i, _)| i);
        }

        if let Some(idx) = target_idx {
            header.active_slots[idx] = MonitorActiveSlot {
                task_id,
                pid,
                _pad: 0,
                start_epoch_ms: now_ms,
                file_name: str_to_u16_buf(file_name),
                stage: str_to_u16_buf(&crate::i18n::t("stage.preparing_thumbnail")),
            };
        }
        sync_and_reap_active_slots(header, now_ms);
        release_lock(&header.lock);
    });

    Some(TaskGuard {
        task_id: assigned_task_id,
        start_time: Instant::now(),
        file_name: file_name.to_string(),
        ext: ext.to_string(),
        finished: false,
    })
}

/// Updates the current operational stage for active jobs (e.g. "Smart seek frame 12/50").
pub fn record_task_stage(stage: &str) {
    let pid = std::process::id();
    let now_ms = current_epoch_ms();
    with_shared_mem(|header| {
        acquire_lock(&header.lock);
        let stage_buf = str_to_u16_buf::<64>(stage);
        if let Some((idx, _)) = header
            .active_slots
            .iter()
            .enumerate()
            .filter(|(_, s)| s.task_id != 0 && s.pid == pid)
            .max_by_key(|(_, s)| s.start_epoch_ms)
        {
            header.active_slots[idx].stage = stage_buf;
            sync_and_reap_active_slots(header, now_ms);
        } else {
            sync_and_reap_active_slots(header, now_ms);
            if header.active_tasks > 0 {
                header.current_stage = stage_buf;
            }
        }
        release_lock(&header.lock);
    });
}

/// Completes an active thumbnail task and pushes its report into the rolling history buffer.
pub fn record_task_end(guard: TaskGuard, success: bool, pipeline: &str, details: &str) {
    record_task_end_with_trace(guard, success, pipeline, "", details);
}

/// Completes an active thumbnail task with explicit search/resolution trace text.
pub fn record_task_end_with_trace(
    mut guard: TaskGuard,
    success: bool,
    pipeline: &str,
    search_trace: &str,
    details: &str,
) {
    guard.finished = true;
    let duration_ms = guard.start_time.elapsed().as_millis() as u32;
    let now_epoch_ms = current_epoch_ms();

    with_shared_mem(|header| {
        acquire_lock(&header.lock);
        for slot in header.active_slots.iter_mut() {
            if slot.task_id == guard.task_id {
                *slot = MonitorActiveSlot::empty();
            }
        }
        sync_and_reap_active_slots(header, now_epoch_ms);
        header.total_processed = header.total_processed.saturating_add(1);
        header.total_duration_ms = header.total_duration_ms.saturating_add(duration_ms as u64);

        let idx = (header.history_head as usize) % MAX_HISTORY;
        header.history[idx] = MonitorHistoryItem {
            timestamp_epoch_ms: now_epoch_ms,
            duration_ms,
            success: if success { 1 } else { 0 },
            ext: str_to_u16_buf(&guard.ext),
            file_name: str_to_u16_buf(&guard.file_name),
            pipeline: str_to_u16_buf(pipeline),
            search_trace: str_to_u16_buf(search_trace),
            details: str_to_u16_buf(details),
        };
        header.history_head = ((header.history_head + 1) as usize % MAX_HISTORY) as u32;
        if header.history_count < MAX_HISTORY as u32 {
            header.history_count += 1;
        }

        release_lock(&header.lock);
    });
}

/// Fetches a point-in-time snapshot of the thumbnail processing engine for GUI display.
pub fn fetch_snapshot() -> Option<MonitorSnapshot> {
    let mut snap = None;
    with_shared_mem(|header| {
        acquire_lock(&header.lock);
        let now_ms = current_epoch_ms();
        sync_and_reap_active_slots(header, now_ms);

        let elapsed = if header.active_tasks > 0 && header.current_start_time_epoch_ms > 0 {
            now_ms.saturating_sub(header.current_start_time_epoch_ms)
        } else {
            0
        };

        let avg = if header.total_processed > 0 {
            (header.total_duration_ms / header.total_processed) as u32
        } else {
            0
        };

        let mut items = Vec::new();
        let count = (header.history_count as usize).min(MAX_HISTORY);
        let head = header.history_head as usize;

        // Traverse backwards from newest to oldest
        for i in 0..count {
            let idx = (head + MAX_HISTORY - 1 - i) % MAX_HISTORY;
            let item = header.history[idx];
            // Filter out empty or uninitialized records (prevents ghost 08:00:00 rows)
            if item.timestamp_epoch_ms == 0 || item.file_name[0] == 0 {
                continue;
            }
            let secs = item.timestamp_epoch_ms / 1000;
            let time_str = format_epoch_hms(secs);
            items.push(SnapshotHistoryItem {
                time_str,
                ext: u16_buf_to_string(&item.ext),
                file_name: u16_buf_to_string(&item.file_name),
                pipeline: u16_buf_to_string(&item.pipeline),
                search_trace: u16_buf_to_string(&item.search_trace),
                duration_ms: item.duration_ms,
                success: item.success != 0,
                details: u16_buf_to_string(&item.details),
            });
        }

        snap = Some(MonitorSnapshot {
            active_tasks: header.active_tasks,
            total_processed: header.total_processed,
            avg_duration_ms: avg,
            current_file: u16_buf_to_string(&header.current_file),
            current_stage: u16_buf_to_string(&header.current_stage),
            current_elapsed_ms: elapsed,
            history: items,
        });

        release_lock(&header.lock);
    });
    snap
}

/// Converts UTC Unix seconds to "local Unix seconds" using the machine's real time zone and DST
/// rules (`FileTimeToLocalFileTime`). Falls back to UTC if the conversion fails.
pub fn utc_epoch_secs_to_local(epoch_secs: u64) -> u64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
    const EPOCH_DIFF_100NS: u64 = 116_444_736_000_000_000; // 1601-01-01 -> 1970-01-01
    let ticks = epoch_secs
        .saturating_mul(10_000_000)
        .saturating_add(EPOCH_DIFF_100NS);
    let utc = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    let mut local = FILETIME::default();
    // SAFETY: both pointers reference valid stack FILETIME structures.
    if unsafe { FileTimeToLocalFileTime(&utc, &mut local) }.is_err() {
        return epoch_secs;
    }
    let local_ticks = ((local.dwHighDateTime as u64) << 32) | local.dwLowDateTime as u64;
    local_ticks.saturating_sub(EPOCH_DIFF_100NS) / 10_000_000
}

fn format_epoch_hms(epoch_secs: u64) -> String {
    let local_sec = utc_epoch_secs_to_local(epoch_secs) % 86400;
    let h = local_sec / 3600;
    let m = (local_sec % 3600) / 60;
    let s = local_sec % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}
