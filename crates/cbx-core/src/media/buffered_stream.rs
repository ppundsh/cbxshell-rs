use std::ffi::c_void;
use std::sync::Mutex;
use windows::core::{implement, Result, HRESULT};
use windows::Win32::Foundation::{E_NOTIMPL, E_POINTER, S_FALSE, S_OK};
use windows::Win32::System::Com::{
    ISequentialStream, ISequentialStream_Impl, IStream, IStream_Impl, LOCKTYPE, STATFLAG,
    STATSTG, STGC, STREAM_SEEK, STREAM_SEEK_CUR, STREAM_SEEK_END, STREAM_SEEK_SET,
};

const CHUNK_SIZE: usize = 512 * 1024; // 512 KB chunks

struct BufferState {
    pos: u64,
    size: u64,
    cache_offset: u64,
    cache_data: Vec<u8>,
}

#[implement(IStream, ISequentialStream)]
pub struct BufferedIStream {
    inner: IStream,
    state: Mutex<BufferState>,
}

impl BufferedIStream {
    pub fn new(inner: IStream) -> IStream {
        let mut size = 0u64;
        unsafe {
            let mut stat = STATSTG::default();
            if inner.Stat(&mut stat, STATFLAG(1)).is_ok() {
                size = stat.cbSize;
            }
        }
        let obj = Self {
            inner,
            state: Mutex::new(BufferState {
                pos: 0,
                size,
                cache_offset: u64::MAX,
                cache_data: Vec::new(),
            }),
        };
        obj.into()
    }
}

impl ISequentialStream_Impl for BufferedIStream_Impl {
    fn Read(&self, pv: *mut c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        if pv.is_null() {
            return E_POINTER;
        }
        if cb == 0 {
            if !pcbread.is_null() {
                unsafe { *pcbread = 0 };
            }
            return S_OK;
        }

        let Ok(mut state) = self.state.lock() else {
            return windows::Win32::Foundation::E_FAIL;
        };

        if state.size > 0 && state.pos >= state.size {
            if !pcbread.is_null() {
                unsafe { *pcbread = 0 };
            }
            return S_FALSE;
        }

        let mut total_read = 0usize;
        let dest = unsafe { std::slice::from_raw_parts_mut(pv as *mut u8, cb as usize) };

        while total_read < dest.len() {
            let cur_pos = state.pos;
            if state.size > 0 && cur_pos >= state.size {
                break;
            }

            // Check if current position is within cached chunk
            let in_cache = cur_pos >= state.cache_offset
                && cur_pos < state.cache_offset.saturating_add(state.cache_data.len() as u64);

            if in_cache {
                let offset_in_cache = (cur_pos - state.cache_offset) as usize;
                let available = state.cache_data.len() - offset_in_cache;
                let needed = dest.len() - total_read;
                let to_copy = available.min(needed);

                dest[total_read..total_read + to_copy].copy_from_slice(
                    &state.cache_data[offset_in_cache..offset_in_cache + to_copy],
                );
                total_read += to_copy;
                state.pos += to_copy as u64;
            } else {
                // Populate cache at cur_pos
                let mut new_buf = vec![0u8; CHUNK_SIZE];
                let mut bytes_read = 0u32;

                let seek_res = unsafe { self.inner.Seek(cur_pos as i64, STREAM_SEEK_SET, None) };
                if seek_res.is_err() {
                    break;
                }

                let read_res = unsafe {
                    self.inner.Read(
                        new_buf.as_mut_ptr() as *mut c_void,
                        new_buf.len() as u32,
                        Some(&mut bytes_read),
                    )
                };
                if read_res.is_err() || bytes_read == 0 {
                    break;
                }

                // Never trust the inner stream's count beyond what we asked for.
                new_buf.truncate((bytes_read as usize).min(CHUNK_SIZE));
                state.cache_offset = cur_pos;
                state.cache_data = new_buf;
            }
        }

        if !pcbread.is_null() {
            unsafe { *pcbread = total_read as u32 };
        }

        if total_read == 0 && cb > 0 {
            S_FALSE
        } else {
            S_OK
        }
    }

    fn Write(&self, _pv: *const c_void, _cb: u32, _pcbwritten: *mut u32) -> HRESULT {
        E_NOTIMPL
    }
}

impl IStream_Impl for BufferedIStream_Impl {
    fn Seek(
        &self,
        dlibmove: i64,
        dworigin: STREAM_SEEK,
        plibnewposition: *mut u64,
    ) -> Result<()> {
        let Ok(mut state) = self.state.lock() else {
            return Err(windows_core::Error::from(windows::Win32::Foundation::E_FAIL));
        };

        // Checked arithmetic: offsets come from the media pipeline (and ultimately from container
        // fields in the file). An overflow here would panic across the COM boundary (abort of the
        // host process) in debug builds or silently wrap in release builds.
        let base: Option<i64> = match dworigin {
            STREAM_SEEK_SET => Some(0),
            STREAM_SEEK_CUR => i64::try_from(state.pos).ok(),
            STREAM_SEEK_END => i64::try_from(state.size).ok(),
            _ => return Err(windows_core::Error::from(windows::Win32::Foundation::E_INVALIDARG)),
        };
        let Some(new_pos) = base.and_then(|b| b.checked_add(dlibmove)) else {
            return Err(windows_core::Error::from(windows::Win32::Foundation::E_INVALIDARG));
        };

        if new_pos < 0 {
            return Err(windows_core::Error::from(windows::Win32::Foundation::E_INVALIDARG));
        }

        state.pos = new_pos as u64;

        if !plibnewposition.is_null() {
            unsafe { *plibnewposition = state.pos };
        }

        Ok(())
    }

    fn SetSize(&self, _libnewsize: u64) -> Result<()> {
        Err(windows_core::Error::from(E_NOTIMPL))
    }

    fn CopyTo(
        &self,
        _pstm: Option<&IStream>,
        _cb: u64,
        _pcbread: *mut u64,
        _pcbwritten: *mut u64,
    ) -> Result<()> {
        Err(windows_core::Error::from(E_NOTIMPL))
    }

    fn Commit(&self, _grfcommitflags: &STGC) -> Result<()> {
        Ok(())
    }

    fn Revert(&self) -> Result<()> {
        Ok(())
    }

    fn LockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: &LOCKTYPE) -> Result<()> {
        Err(windows_core::Error::from(E_NOTIMPL))
    }

    fn UnlockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: u32) -> Result<()> {
        Err(windows_core::Error::from(E_NOTIMPL))
    }

    fn Stat(&self, pstatstg: *mut STATSTG, grfstatflag: &STATFLAG) -> Result<()> {
        unsafe { self.inner.Stat(pstatstg, *grfstatflag) }
    }

    fn Clone(&self) -> windows::core::Result<IStream> {
        let inner_clone = unsafe { self.inner.Clone()? };
        Ok(BufferedIStream::new(inner_clone))
    }
}
