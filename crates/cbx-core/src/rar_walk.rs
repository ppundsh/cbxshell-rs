//! Seek-based RAR4 / RAR5 block-header walker.
//!
//! The cover extractor used to read a 16–96 MiB prefix of every `.cbr` into memory before
//! parsing it, which over SMB / cloud placeholders means transferring tens of MiB per thumbnail.
//! RAR stores a self-describing header in front of every entry's packed data, so the headers can
//! be walked with seeks instead: each step reads only the header (one small window) and then
//! jumps over the packed data. Walking stops as soon as the cover is known (unsorted mode) or
//! after the last header (sorted mode), and only the bytes needed to decode the chosen entry are
//! read afterwards:
//!
//! * non-solid entry → archive signature + main header + that entry's header and packed data;
//! * solid entry → signature + main header + every block from the start of its solid group up to
//!   the end of its packed data (solid streams cannot be decoded from the middle).
//!
//! Those slices are concatenated into a small synthetic archive and handed to `rar-codec`, which
//! re-validates every header CRC and performs the actual decompression.
//!
//! Every length read from the file is bounds-checked against the stream length and fixed caps;
//! nothing here allocates more than one header (≤ 2 MiB) at a time.

#![forbid(unsafe_code)]

use std::io::{Read, Seek, SeekFrom};
use std::time::Instant;

/// Read granularity for header windows. One window usually covers a whole header (RAR5 headers
/// are typically < 200 bytes), so each entry costs a single small read.
const WINDOW: usize = 4096;
/// SFX stubs are searched for the RAR signature within this many bytes.
const SFX_SEARCH_BYTES: u64 = 1024 * 1024;
/// RAR5 specification limit for a single header.
const MAX_RAR5_HEADER: u64 = 2 * 1024 * 1024;

const RAR5_SIG: &[u8; 8] = b"Rar!\x1a\x07\x01\x00";
const RAR4_SIG: &[u8; 7] = b"Rar!\x1a\x07\x00";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RarVersion {
    V4,
    V5,
}

/// One file entry discovered by the walker (absolute stream offsets).
#[derive(Clone, Debug)]
pub(crate) struct WalkEntry {
    pub name: String,
    pub header_offset: u64,
    pub data_offset: u64,
    pub packed_size: u64,
    pub unpacked_size: u64,
    pub dictionary_size: u64,
    pub is_dir: bool,
    pub encrypted: bool,
    /// Entry continues the solid stream of the previous entry.
    pub solid: bool,
    pub split_before: bool,
    pub split_after: bool,
}

impl WalkEntry {
    pub fn data_end(&self) -> Option<u64> {
        self.data_offset.checked_add(self.packed_size)
    }
}

#[derive(Debug)]
#[allow(dead_code)] // version / stopped_early / bytes_read are diagnostics used by tests
pub(crate) struct WalkedArchive {
    pub version: RarVersion,
    pub sig_offset: u64,
    /// Absolute end of the main archive header block.
    pub main_end: u64,
    pub entries: Vec<WalkEntry>,
    /// True when the end-of-archive header (or a clean EOF) was reached without hitting a cap.
    pub complete: bool,
    /// True when the walk ended because `stop` returned true.
    pub stopped_early: bool,
    pub stream_len: u64,
    /// Bytes actually read from the stream while walking (diagnostics / tests).
    pub bytes_read: u64,
}

// ---------------------------------------------------------------------------------------------
// CRC32 (IEEE) — headers are verified so a corrupt / hostile header cannot send the walker to
// arbitrary offsets.

const fn make_crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

static CRC_TABLE: [u32; 256] = make_crc_table();

pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in bytes {
        c = CRC_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

// ---------------------------------------------------------------------------------------------
// Windowed reader

struct Window<'a, R: Read + Seek> {
    r: &'a mut R,
    len: u64,
    base: u64,
    buf: Vec<u8>,
    bytes_read: u64,
}

impl<'a, R: Read + Seek> Window<'a, R> {
    fn new(r: &'a mut R) -> Result<Self, String> {
        let len = r.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
        Ok(Self { r, len, base: 0, buf: Vec::new(), bytes_read: 0 })
    }

    /// Returns a copy of `n` bytes at absolute offset `off`.
    fn get(&mut self, off: u64, n: usize) -> Result<Vec<u8>, String> {
        let end = off
            .checked_add(n as u64)
            .ok_or_else(|| "RAR offset overflow".to_string())?;
        if end > self.len {
            return Err("RAR header truncated".into());
        }
        let have_end = self.base + self.buf.len() as u64;
        if off < self.base || end > have_end {
            let want = (n.max(WINDOW) as u64).min(self.len - off) as usize;
            self.r.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
            self.buf.clear();
            self.buf.resize(want, 0);
            self.r.read_exact(&mut self.buf).map_err(|e| e.to_string())?;
            self.base = off;
            self.bytes_read = self.bytes_read.saturating_add(want as u64);
        }
        let s = (off - self.base) as usize;
        Ok(self.buf[s..s + n].to_vec())
    }
}

// ---------------------------------------------------------------------------------------------
// Byte parser helpers

struct P<'b> {
    b: &'b [u8],
    i: usize,
}

impl<'b> P<'b> {
    fn vint(&mut self) -> Result<u64, String> {
        let (v, n) = read_vint(&self.b[self.i.min(self.b.len())..])?;
        self.i += n;
        Ok(v)
    }
    fn take(&mut self, n: usize) -> Result<&'b [u8], String> {
        let end = self.i.checked_add(n).ok_or_else(|| "RAR field overflow".to_string())?;
        if end > self.b.len() {
            return Err("RAR field truncated".into());
        }
        let s = &self.b[self.i..end];
        self.i = end;
        Ok(s)
    }
}

/// RAR5 variable-length integer: 7 bits per byte, high bit = continuation, at most 10 bytes.
pub(crate) fn read_vint(b: &[u8]) -> Result<(u64, usize), String> {
    let mut v: u64 = 0;
    for (i, &byte) in b.iter().enumerate().take(10) {
        let shift = 7 * i as u32;
        let part = (byte & 0x7F) as u64;
        if shift >= 64 || (shift > 57 && (part >> (64 - shift)) != 0) {
            return Err("RAR vint overflow".into());
        }
        v |= part << shift;
        if byte & 0x80 == 0 {
            return Ok((v, i + 1));
        }
    }
    Err("RAR vint truncated".into())
}

fn le_u16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

fn le_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// Decodes the RAR 2.x–4.x "unicode" file-name encoding (ASCII name, NUL, compressed UTF-16).
pub(crate) fn decode_rar4_unicode(name: &[u8], enc: &[u8]) -> Option<String> {
    let mut out: Vec<u16> = Vec::with_capacity(name.len());
    let mut pos = 0usize;
    let high = *enc.get(pos)? as u16;
    pos += 1;
    let mut flags = 0u8;
    let mut flag_bits = 0u32;
    while pos < enc.len() && out.len() < 4096 {
        if flag_bits == 0 {
            flags = enc[pos];
            pos += 1;
            flag_bits = 8;
        }
        flag_bits -= 2;
        match (flags >> flag_bits) & 3 {
            0 => {
                out.push(*enc.get(pos)? as u16);
                pos += 1;
            }
            1 => {
                out.push(*enc.get(pos)? as u16 | (high << 8));
                pos += 1;
            }
            2 => {
                let lo = *enc.get(pos)? as u16;
                let hi = *enc.get(pos + 1)? as u16;
                out.push(lo | (hi << 8));
                pos += 2;
            }
            _ => {
                let len = *enc.get(pos)?;
                pos += 1;
                if len & 0x80 != 0 {
                    let correction = *enc.get(pos)?;
                    pos += 1;
                    for _ in 0..((len & 0x7F) as usize + 2) {
                        let c = *name.get(out.len())?;
                        out.push((c.wrapping_add(correction) as u16) | (high << 8));
                    }
                } else {
                    for _ in 0..(len as usize + 2) {
                        out.push(*name.get(out.len())? as u16);
                    }
                }
            }
        }
    }
    Some(String::from_utf16_lossy(&out))
}

// ---------------------------------------------------------------------------------------------
// Signature

fn locate_signature<R: Read + Seek>(w: &mut Window<'_, R>) -> Result<(RarVersion, u64), String> {
    let head_len = w.len.min(WINDOW as u64) as usize;
    if head_len < 7 {
        return Err("Not a RAR archive".into());
    }
    let head = w.get(0, head_len)?;
    let check = |b: &[u8], at: usize| -> Option<RarVersion> {
        if b.get(at..at + 8) == Some(&RAR5_SIG[..]) {
            Some(RarVersion::V5)
        } else if b.get(at..at + 7) == Some(&RAR4_SIG[..]) {
            Some(RarVersion::V4)
        } else {
            None
        }
    };
    if let Some(v) = check(&head, 0) {
        return Ok((v, 0));
    }
    // Self-extracting archive: PE stub followed by the archive.
    if !head.starts_with(b"MZ") {
        return Err("Not a RAR archive".into());
    }
    let limit = w.len.min(SFX_SEARCH_BYTES);
    let chunk = 64 * 1024u64;
    let mut off = 0u64;
    while off < limit {
        let n = (chunk + 8).min(w.len - off) as usize;
        let buf = w.get(off, n)?;
        for (i, &b) in buf.iter().enumerate().take(buf.len().saturating_sub(6)) {
            if b == b'R' {
                if let Some(v) = check(&buf, i) {
                    return Ok((v, off + i as u64));
                }
            }
        }
        off += chunk;
    }
    Err("RAR signature not found".into())
}

// ---------------------------------------------------------------------------------------------
// Walker

/// Walks the block headers of a RAR4/RAR5 stream.
///
/// `stop` is called after each file entry is recorded; returning `true` ends the walk early.
pub(crate) fn walk<R: Read + Seek>(
    reader: &mut R,
    deadline: Instant,
    max_entries: usize,
    mut stop: impl FnMut(&[WalkEntry]) -> bool,
) -> Result<WalkedArchive, String> {
    let mut w = Window::new(reader)?;
    let (version, sig_offset) = locate_signature(&mut w)?;
    let mut out = WalkedArchive {
        version,
        sig_offset,
        main_end: 0,
        entries: Vec::new(),
        complete: false,
        stopped_early: false,
        stream_len: w.len,
        bytes_read: 0,
    };
    let res = match version {
        RarVersion::V5 => walk_v5(&mut w, &mut out, deadline, max_entries, &mut stop),
        RarVersion::V4 => walk_v4(&mut w, &mut out, deadline, max_entries, &mut stop),
    };
    out.bytes_read = w.bytes_read;
    res?;
    if out.main_end == 0 {
        return Err("RAR main archive header missing".into());
    }
    Ok(out)
}

fn walk_v5<R: Read + Seek>(
    w: &mut Window<'_, R>,
    out: &mut WalkedArchive,
    deadline: Instant,
    max_entries: usize,
    stop: &mut impl FnMut(&[WalkEntry]) -> bool,
) -> Result<(), String> {
    let len = w.len;
    let mut pos = out.sig_offset + RAR5_SIG.len() as u64;
    let mut first = true;
    loop {
        if pos >= len {
            out.complete = true; // clean EOF without an end header
            return Ok(());
        }
        if Instant::now() > deadline || out.entries.len() >= max_entries {
            return Ok(());
        }
        let probe = w.get(pos, (len - pos).min(14) as usize)?;
        if probe.len() < 5 {
            return Err("RAR5 header truncated".into());
        }
        let crc = le_u32(&probe, 0).unwrap_or(0);
        let (hsize, vlen) = read_vint(&probe[4..])?;
        if hsize == 0 || hsize > MAX_RAR5_HEADER {
            return Err("RAR5 header size out of range".into());
        }
        let total_hdr = 4 + vlen + hsize as usize;
        let hdr = w.get(pos, total_hdr)?;
        if crc32(&hdr[4..]) != crc {
            return Err("RAR5 header CRC mismatch".into());
        }
        let mut p = P { b: &hdr, i: 4 + vlen };
        let htype = p.vint()?;
        let hflags = p.vint()?;
        let extra_size = if hflags & 0x0001 != 0 { p.vint()? } else { 0 };
        let data_size = if hflags & 0x0002 != 0 { p.vint()? } else { 0 };
        if extra_size > (total_hdr - p.i) as u64 {
            return Err("RAR5 extra area out of range".into());
        }
        let data_off = pos + total_hdr as u64;
        let next = data_off
            .checked_add(data_size)
            .ok_or_else(|| "RAR5 data size overflow".to_string())?;

        if first {
            match htype {
                1 => {}
                4 => return Err("RAR archive has encrypted headers".into()),
                _ => return Err("RAR5 main header missing".into()),
            }
            first = false;
        }
        match htype {
            1 => out.main_end = next.min(len),
            4 => return Err("RAR archive has encrypted headers".into()),
            5 => {
                out.complete = true;
                return Ok(());
            }
            2 => {
                if next > len {
                    return Ok(()); // truncated archive: entry data incomplete
                }
                let fflags = p.vint()?;
                let unpacked = p.vint()?;
                let _attr = p.vint()?;
                if fflags & 0x0002 != 0 {
                    p.take(4)?;
                }
                if fflags & 0x0004 != 0 {
                    p.take(4)?;
                }
                let comp = p.vint()?;
                let _host = p.vint()?;
                let name_len = p.vint()?;
                if name_len > 8192 {
                    return Err("RAR5 name too long".into());
                }
                let name = String::from_utf8_lossy(p.take(name_len as usize)?).into_owned();

                // Extra area sits at the end of the header; record type 1 = file encryption.
                let mut encrypted = false;
                let extra = &hdr[total_hdr - extra_size as usize..];
                let mut e = P { b: extra, i: 0 };
                while e.i < extra.len() {
                    let rec_size = e.vint()?;
                    let rec_start = e.i;
                    let rec_type = e.vint()?;
                    if rec_type == 1 {
                        encrypted = true;
                    }
                    let rec_end = (rec_start as u64)
                        .checked_add(rec_size)
                        .filter(|v| *v <= extra.len() as u64 && *v > rec_start as u64)
                        .ok_or_else(|| "RAR5 extra record out of range".to_string())?;
                    e.i = rec_end as usize;
                }

                let dict_shift = ((comp >> 10) & 0x1F) as u32;
                out.entries.push(WalkEntry {
                    name,
                    header_offset: pos,
                    data_offset: data_off,
                    packed_size: data_size,
                    unpacked_size: if fflags & 0x0008 != 0 { u64::MAX } else { unpacked },
                    dictionary_size: (128u64 * 1024) << dict_shift,
                    is_dir: fflags & 0x0001 != 0,
                    encrypted,
                    solid: comp & 0x0040 != 0,
                    split_before: hflags & 0x0008 != 0,
                    split_after: hflags & 0x0010 != 0,
                });
                if stop(&out.entries) {
                    out.stopped_early = true;
                    return Ok(());
                }
            }
            _ => {} // service headers (CMT, QO, ACL, STM, …) are skipped with their data
        }
        if next > len {
            return Ok(());
        }
        pos = next;
    }
}

fn walk_v4<R: Read + Seek>(
    w: &mut Window<'_, R>,
    out: &mut WalkedArchive,
    deadline: Instant,
    max_entries: usize,
    stop: &mut impl FnMut(&[WalkEntry]) -> bool,
) -> Result<(), String> {
    let len = w.len;
    let mut pos = out.sig_offset + RAR4_SIG.len() as u64;
    let mut first = true;
    loop {
        if pos >= len {
            out.complete = true;
            return Ok(());
        }
        if Instant::now() > deadline || out.entries.len() >= max_entries {
            return Ok(());
        }
        if len - pos < 7 {
            // Trailing garbage shorter than a block header.
            out.complete = true;
            return Ok(());
        }
        let head = w.get(pos, 7)?;
        let crc16 = le_u16(&head, 0).unwrap_or(0);
        let htype = head[2];
        let hflags = le_u16(&head, 3).unwrap_or(0);
        let hsize = le_u16(&head, 5).unwrap_or(0) as usize;
        if hsize < 7 {
            return Err("RAR4 header size out of range".into());
        }
        let hdr = w.get(pos, hsize)?;
        if (crc32(&hdr[2..]) & 0xFFFF) as u16 != crc16 {
            return Err("RAR4 header CRC mismatch".into());
        }
        let mut add: u64 = 0;
        if hflags & 0x8000 != 0 {
            add = le_u32(&hdr, 7).ok_or_else(|| "RAR4 header truncated".to_string())? as u64;
        }
        let large = hflags & 0x0100 != 0 && (htype == 0x74 || htype == 0x7A);
        if large {
            let high = le_u32(&hdr, 32).ok_or_else(|| "RAR4 header truncated".to_string())? as u64;
            add |= high << 32;
        }
        let data_off = pos + hsize as u64;
        let next = data_off
            .checked_add(add)
            .ok_or_else(|| "RAR4 data size overflow".to_string())?;

        if first {
            if htype != 0x73 {
                return Err("RAR4 main header missing".into());
            }
            first = false;
        }
        match htype {
            0x73 => {
                if hflags & 0x0080 != 0 {
                    return Err("RAR archive has encrypted headers".into());
                }
                out.main_end = next.min(len);
            }
            0x7B => {
                out.complete = true;
                return Ok(());
            }
            0x74 => {
                if next > len {
                    return Ok(());
                }
                if hdr.len() < 32 {
                    return Err("RAR4 file header truncated".into());
                }
                let mut unpacked = le_u32(&hdr, 11).unwrap_or(0) as u64;
                let name_size = le_u16(&hdr, 26).unwrap_or(0) as usize;
                let name_at = if large {
                    let high = le_u32(&hdr, 36).ok_or_else(|| "RAR4 header truncated".to_string())? as u64;
                    unpacked |= high << 32;
                    40
                } else {
                    32
                };
                let raw_name = hdr
                    .get(name_at..name_at + name_size)
                    .ok_or_else(|| "RAR4 name truncated".to_string())?;
                let name = if hflags & 0x0200 != 0 {
                    match raw_name.iter().position(|&b| b == 0) {
                        Some(z) => decode_rar4_unicode(&raw_name[..z], &raw_name[z + 1..])
                            .unwrap_or_else(|| String::from_utf8_lossy(&raw_name[..z]).into_owned()),
                        None => String::from_utf8_lossy(raw_name).into_owned(),
                    }
                } else {
                    String::from_utf8_lossy(raw_name).into_owned()
                };
                let dict_bits = (hflags >> 5) & 7;
                out.entries.push(WalkEntry {
                    name,
                    header_offset: pos,
                    data_offset: data_off,
                    packed_size: add,
                    unpacked_size: unpacked,
                    dictionary_size: if dict_bits == 7 { 0 } else { (64u64 * 1024) << dict_bits },
                    is_dir: dict_bits == 7,
                    encrypted: hflags & 0x0004 != 0,
                    solid: hflags & 0x0010 != 0,
                    split_before: hflags & 0x0001 != 0,
                    split_after: hflags & 0x0002 != 0,
                });
                if stop(&out.entries) {
                    out.stopped_early = true;
                    return Ok(());
                }
            }
            _ => {}
        }
        if next > len {
            return Ok(());
        }
        pos = next;
    }
}

/// Index of the first entry of the solid group that `idx` belongs to.
pub(crate) fn solid_group_start(entries: &[WalkEntry], idx: usize) -> usize {
    let mut start = idx;
    while start > 0 && entries[start].solid {
        start -= 1;
    }
    start
}

/// Reads the minimal byte set needed to decode `entries[idx]` and assembles it into a small
/// self-contained archive: signature + main header + blocks `[group start .. entry data end)`.
pub(crate) fn build_minimal_archive<R: Read + Seek>(
    reader: &mut R,
    walked: &WalkedArchive,
    idx: usize,
    max_bytes: u64,
) -> Result<Vec<u8>, String> {
    let entry = walked.entries.get(idx).ok_or_else(|| "RAR entry index out of range".to_string())?;
    let start = solid_group_start(&walked.entries, idx);
    let range_start = walked.entries[start].header_offset;
    let range_end = entry.data_end().ok_or_else(|| "RAR entry range overflow".to_string())?;
    if range_end > walked.stream_len || range_start < walked.main_end || range_end < range_start {
        return Err("RAR entry range out of bounds".into());
    }
    let head_len = walked.main_end - walked.sig_offset;
    let total = head_len
        .checked_add(range_end - range_start)
        .ok_or_else(|| "RAR range overflow".to_string())?;
    if total > max_bytes {
        return Err("RAR cover range exceeds the read budget".into());
    }
    let mut buf = vec![0u8; total as usize];
    reader
        .seek(SeekFrom::Start(walked.sig_offset))
        .map_err(|e| e.to_string())?;
    reader
        .read_exact(&mut buf[..head_len as usize])
        .map_err(|e| e.to_string())?;
    reader.seek(SeekFrom::Start(range_start)).map_err(|e| e.to_string())?;
    reader
        .read_exact(&mut buf[head_len as usize..])
        .map_err(|e| e.to_string())?;
    // Close the archive properly so the decoder sees a complete, well-formed stream.
    buf.extend_from_slice(&end_of_archive_block(walked.version));
    Ok(buf)
}

/// A minimal end-of-archive block for the given format.
pub(crate) fn end_of_archive_block(version: RarVersion) -> Vec<u8> {
    match version {
        RarVersion::V5 => {
            // size=3 | type=5 (end) | header flags=0 | end flags=0
            let body = [0x03u8, 0x05, 0x00, 0x00];
            let mut out = crc32(&body).to_le_bytes().to_vec();
            out.extend_from_slice(&body);
            out
        }
        RarVersion::V4 => {
            // type=0x7B | flags=0x4000 (skip if unknown) | size=7
            let tail = [0x7Bu8, 0x00, 0x40, 0x07, 0x00];
            let mut out = ((crc32(&tail) & 0xFFFF) as u16).to_le_bytes().to_vec();
            out.extend_from_slice(&tail);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::CoverOptions;
    use std::io::Cursor;
    use std::time::Duration;

    /// Read+Seek wrapper that counts how many bytes were actually read.
    struct Counting {
        inner: Cursor<Vec<u8>>,
        read: u64,
    }

    impl Read for Counting {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.read += n as u64;
            Ok(n)
        }
    }

    impl Seek for Counting {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    fn vint(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let b = (v & 0x7F) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return out;
            }
            out.push(b | 0x80);
        }
    }

    fn rar5_block(body: &[u8]) -> Vec<u8> {
        let mut sized = vint(body.len() as u64);
        sized.extend_from_slice(body);
        let mut out = crc32(&sized).to_le_bytes().to_vec();
        out.extend_from_slice(&sized);
        out
    }

    /// RAR5 archive whose files are all stored (method 0), non-solid.
    fn rar5_stored(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut out = RAR5_SIG.to_vec();
        out.extend(rar5_block(&[1, 0, 0])); // main header: type 1, flags 0, archive flags 0
        for (name, data) in files {
            let mut body = vec![2u8, 0x02]; // file header, has data area
            body.extend(vint(data.len() as u64));
            body.push(0x04); // file flags: CRC32 present
            body.extend(vint(data.len() as u64));
            body.extend(vint(0x20)); // attributes
            body.extend_from_slice(&crc32(data).to_le_bytes());
            body.extend(vint(0)); // compression: version 0, store, 128 KiB dict
            body.extend(vint(0)); // host OS: Windows
            body.extend(vint(name.len() as u64));
            body.extend_from_slice(name.as_bytes());
            out.extend(rar5_block(&body));
            out.extend_from_slice(data);
        }
        out.extend(rar5_block(&[5, 0, 0])); // end of archive
        out
    }

    fn rar4_block(htype: u8, flags: u16, rest: &[u8]) -> Vec<u8> {
        let size = (7 + rest.len()) as u16;
        let mut tail = vec![htype];
        tail.extend_from_slice(&flags.to_le_bytes());
        tail.extend_from_slice(&size.to_le_bytes());
        tail.extend_from_slice(rest);
        let mut out = ((crc32(&tail) & 0xFFFF) as u16).to_le_bytes().to_vec();
        out.extend(tail);
        out
    }

    fn rar4_stored(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut out = RAR4_SIG.to_vec();
        out.extend(rar4_block(0x73, 0, &[0u8; 6]));
        for (name, data) in files {
            let mut rest = Vec::new();
            rest.extend_from_slice(&(data.len() as u32).to_le_bytes()); // PACK_SIZE
            rest.extend_from_slice(&(data.len() as u32).to_le_bytes()); // UNP_SIZE
            rest.push(2); // HOST_OS: Win32
            rest.extend_from_slice(&crc32(data).to_le_bytes());
            rest.extend_from_slice(&0x5A21_0000u32.to_le_bytes()); // FTIME
            rest.push(20); // UNP_VER 2.0
            rest.push(0x30); // METHOD: store
            rest.extend_from_slice(&(name.len() as u16).to_le_bytes());
            rest.extend_from_slice(&0x20u32.to_le_bytes()); // ATTR
            rest.extend_from_slice(name.as_bytes());
            out.extend(rar4_block(0x74, 0x8000, &rest));
            out.extend_from_slice(data);
        }
        out.extend(rar4_block(0x7B, 0x4000, &[]));
        out
    }

    fn fixture_files() -> Vec<(&'static str, Vec<u8>)> {
        let big = vec![0xA5u8; 2 * 1024 * 1024];
        vec![
            ("pad1.bin", big.clone()),
            ("999.jpg", vec![0x99u8; 300 * 1024]),
            ("pad2.bin", big.clone()),
            ("001.jpg", b"\xFF\xD8\xFF\xE0 cover bytes \xFF\xD9".to_vec()),
            ("pad3.bin", big),
        ]
    }

    fn opts(sort: bool) -> CoverOptions {
        CoverOptions {
            sort,
            prefer_continuous_sequence: false,
            scan_timeout: Duration::from_secs(30),
            ..CoverOptions::default()
        }
    }

    fn far_deadline() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    #[test]
    fn vint_round_trips_and_rejects_overflow() {
        for v in [0u64, 1, 127, 128, 300, 1 << 35, u64::MAX] {
            let enc = vint(v);
            assert_eq!(read_vint(&enc).unwrap(), (v, enc.len()));
        }
        assert!(read_vint(&[0x80; 10]).is_err());
        assert!(read_vint(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F]).is_err());
    }

    #[test]
    fn rar5_walk_reads_only_headers() {
        let bytes = rar5_stored(&fixture_files());
        let total = bytes.len() as u64;
        let mut r = Counting { inner: Cursor::new(bytes), read: 0 };
        let walked = walk(&mut r, far_deadline(), 10_000, |_| false).unwrap();
        assert_eq!(walked.version, RarVersion::V5);
        assert!(walked.complete);
        let names: Vec<_> = walked.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["pad1.bin", "999.jpg", "pad2.bin", "001.jpg", "pad3.bin"]);
        assert_eq!(walked.entries[3].unpacked_size, 19);
        assert!(r.read < 64 * 1024, "walk read {} of {} bytes", r.read, total);
    }

    #[test]
    fn rar4_walk_reads_only_headers() {
        let bytes = rar4_stored(&fixture_files());
        let mut r = Counting { inner: Cursor::new(bytes), read: 0 };
        let walked = walk(&mut r, far_deadline(), 10_000, |_| false).unwrap();
        assert_eq!(walked.version, RarVersion::V4);
        assert!(walked.complete);
        assert_eq!(walked.entries.len(), 5);
        assert_eq!(walked.entries[1].name, "999.jpg");
        assert_eq!(walked.entries[1].packed_size, 300 * 1024);
        assert!(r.read < 64 * 1024);
    }

    #[test]
    fn walk_stops_early_when_asked() {
        let bytes = rar5_stored(&fixture_files());
        let mut r = Counting { inner: Cursor::new(bytes), read: 0 };
        let walked = walk(&mut r, far_deadline(), 10_000, |e| e.len() == 2).unwrap();
        assert!(walked.stopped_early);
        assert_eq!(walked.entries.len(), 2);
    }

    #[test]
    fn corrupt_header_crc_is_rejected() {
        let mut bytes = rar5_stored(&fixture_files());
        let at = RAR5_SIG.len() + 8 + 6; // inside the first file header
        bytes[at] ^= 0xFF;
        let mut r = Cursor::new(bytes);
        assert!(walk(&mut r, far_deadline(), 10_000, |_| false).is_err());
    }

    #[test]
    fn minimal_archive_contains_only_the_chosen_entry() {
        let bytes = rar5_stored(&fixture_files());
        let mut r = Cursor::new(bytes);
        let walked = walk(&mut r, far_deadline(), 10_000, |_| false).unwrap();
        let mini = build_minimal_archive(&mut r, &walked, 3, 16 * 1024 * 1024).unwrap();
        assert!(mini.len() < 512, "minimal archive is {} bytes", mini.len());
        assert!(mini.starts_with(RAR5_SIG));
        assert!(mini.ends_with(&end_of_archive_block(RarVersion::V5)));
    }

    #[test]
    fn find_cover_rar5_sorted_reads_headers_plus_cover() {
        let bytes = rar5_stored(&fixture_files());
        let mut r = Counting { inner: Cursor::new(bytes), read: 0 };
        let c = crate::rar_pure::find_cover_rar_fast_only(&mut r, &opts(true), &[]).unwrap_or_else(|e| panic!("header-walk path failed: {e}"));
        assert_eq!(c.name, "001.jpg");
        assert_eq!(c.data, b"\xFF\xD8\xFF\xE0 cover bytes \xFF\xD9");
        assert!(r.read < 128 * 1024, "read {} bytes", r.read);
    }

    #[test]
    fn find_cover_rar5_unsorted_stops_at_first_image() {
        let bytes = rar5_stored(&fixture_files());
        let mut r = Counting { inner: Cursor::new(bytes), read: 0 };
        let c = crate::rar_pure::find_cover_rar_fast_only(&mut r, &opts(false), &[]).unwrap_or_else(|e| panic!("header-walk path failed: {e}"));
        assert_eq!(c.name, "999.jpg");
        assert_eq!(c.data.len(), 300 * 1024);
        // headers before it + the 300 KiB entry; never the 2 MiB pads after it
        assert!(r.read < 400 * 1024, "read {} bytes", r.read);
    }

    #[test]
    fn find_cover_rar4_sorted_reads_headers_plus_cover() {
        let bytes = rar4_stored(&fixture_files());
        let mut r = Counting { inner: Cursor::new(bytes), read: 0 };
        let c = crate::rar_pure::find_cover_rar_fast_only(&mut r, &opts(true), &[]).unwrap_or_else(|e| panic!("header-walk path failed: {e}"));
        assert_eq!(c.name, "001.jpg");
        assert_eq!(c.data, b"\xFF\xD8\xFF\xE0 cover bytes \xFF\xD9");
        assert!(r.read < 128 * 1024, "read {} bytes", r.read);
    }

    #[test]
    fn count_entries_rar_walks_headers_only() {
        let bytes = rar5_stored(&fixture_files());
        let mut r = Counting { inner: Cursor::new(bytes), read: 0 };
        let stats = crate::rar_pure::count_entries_rar(&mut r, Duration::from_secs(30)).unwrap();
        assert_eq!((stats.files, stats.images), (5, 2));
        assert!(stats.complete);
        assert!(r.read < 64 * 1024);
    }

    #[test]
    fn rar4_unicode_names_decode() {
        // "ab" ASCII part, then encoding: high byte 0x00, flags 0b00_00_00_00 → two literal bytes
        let s = decode_rar4_unicode(b"ab", &[0x00, 0x00, b'a', b'b']).unwrap();
        assert_eq!(s, "ab");
        // flag 2 → full 16-bit char 0x4E2D (中)
        let s = decode_rar4_unicode(b"?", &[0x00, 0x80, 0x2D, 0x4E]).unwrap();
        assert_eq!(s, "中");
    }
}
