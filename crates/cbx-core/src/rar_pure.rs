//! 100% Pure-Rust, memory-safe (`#![forbid(unsafe_code)]`) RAR4 & RAR5 reader and decompressor.
//!
//! Backed by `rar-codec` (Apache-2.0, `#![forbid(unsafe_code)]`) with additional stream-level
//! decompression-bomb defenses:
//! - Partial reads: block headers are walked with seeks (`rar_walk`) and only the chosen entry
//!   (or its solid group up to it) is read, so network shares transfer header bytes + one image
//!   instead of a large prefix.
//! - Bounded read cap (`MAX_RAR_PREFIX_BYTES` = 96 MiB) for both the minimal archive and the
//!   prefix fallback, so multi-gigabyte `.cbr` files never allocate unbounded heap memory.
//! - Dictionary window cap (`MAX_RAR_DICT_BYTES` = 64 MiB).
//! - Solid-group cumulative unpacked size cap (`MAX_SOLID_GROUP_BYTES` = 128 MiB).
//! - Compression ratio sanity check (`MAX_COMPRESSION_RATIO` = 200x for entries > 8 MiB).
//! - Entry count cap (`MAX_SCAN_ENTRIES` = 10,000) and wall-clock deadline enforcement.

#![forbid(unsafe_code)]

use crate::archive::{
    inspect_entry_security, inspect_entry_security_with_rules, is_image_name, is_junk_name,
    natural_cmp_less, sanitize_archive_path, ArchiveStats, ContainerFormat, CoverCandidate,
    CoverOptions, MAX_COMPRESSION_RATIO, MAX_SCAN_ENTRIES, MAX_SOLID_DRAIN_BYTES,
};
use crate::rar_walk::{self, solid_group_start, WalkEntry};
use rar_codec::{Archive, EntryKind};
use std::io::{Read, Seek, SeekFrom};
use std::time::{Duration, Instant};

/// Maximum prefix of a RAR archive read into memory for cover extraction (96 MiB).
const MAX_RAR_PREFIX_BYTES: u64 = 96 * 1024 * 1024;
/// Maximum dictionary size permitted for a RAR5/RAR4 entry in a shell thumbnailer (64 MiB).
const MAX_RAR_DICT_BYTES: u64 = 64 * 1024 * 1024;

fn read_prefix_bounded<R: Read + Seek>(reader: &mut R, max_bytes: u64) -> Result<Vec<u8>, String> {
    let total_len = reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let to_read = total_len.min(max_bytes) as usize;
    let mut buf = Vec::with_capacity(to_read.min(8 * 1024 * 1024));
    reader
        .take(to_read as u64)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    Ok(buf)
}

/// Discovers and extracts the cover image from a RAR4 or RAR5 stream in 100% safe Rust.
///
/// Fast path: walk the block headers with seeks (`rar_walk`), pick the cover from the header
/// metadata alone, then read only the bytes that entry needs (its own block, or its solid group
/// up to it). Falls back to the bounded-prefix parser when the walker cannot handle the stream
/// (unusual layouts) or `rar-codec` rejects the minimal archive.
pub fn find_cover_rar<R: Read + Seek>(
    reader: &mut R,
    opt: &CoverOptions,
    exclude: &[String],
) -> Result<CoverCandidate, String> {
    match find_cover_rar_walk(reader, opt, exclude) {
        Ok(c) => Ok(c),
        Err(WalkError::Final(e)) => Err(e),
        Err(WalkError::Fallback(reason)) => find_cover_rar_prefix(reader, opt, exclude)
            .map_err(|e| format!("{e} (header walk: {reason})")),
    }
}

/// Header-walk path only (no prefix fallback), so tests can see why it was not taken.
#[cfg(test)]
pub(crate) fn find_cover_rar_fast_only<R: Read + Seek>(
    reader: &mut R,
    opt: &CoverOptions,
    exclude: &[String],
) -> Result<CoverCandidate, String> {
    find_cover_rar_walk(reader, opt, exclude).map_err(|e| match e {
        WalkError::Final(m) => format!("final: {m}"),
        WalkError::Fallback(m) => format!("fallback: {m}"),
    })
}

enum WalkError {
    /// The archive was understood; the answer is definitive (e.g. no usable image).
    Final(String),
    /// The fast path could not handle this stream; retry with the prefix parser.
    Fallback(String),
}

/// Header-metadata filter shared by the walk-based path; returns the sanitized name when the
/// entry is a usable cover candidate.
fn walk_candidate_name(
    entries: &[WalkEntry],
    idx: usize,
    opt: &CoverOptions,
    exclude: &[String],
) -> Option<String> {
    let entry = &entries[idx];
    if entry.is_dir || entry.encrypted || entry.split_before || entry.split_after {
        return None;
    }
    let name = sanitize_archive_path(&entry.name)?;
    if name.is_empty() || name.ends_with('/') || is_junk_name(&name) || !is_image_name(&name) {
        return None;
    }
    if exclude.iter().any(|ex| ex == &name) {
        return None;
    }
    if entry.unpacked_size == 0 || entry.unpacked_size > opt.max_image_bytes {
        return None;
    }
    if entry.dictionary_size > MAX_RAR_DICT_BYTES {
        return None;
    }
    if entry.unpacked_size > 8 * 1024 * 1024
        && (entry.packed_size == 0 || (entry.unpacked_size / entry.packed_size) > MAX_COMPRESSION_RATIO)
    {
        return None;
    }
    if entry.solid {
        let start = solid_group_start(entries, idx);
        let solid_sum = entries[start..=idx]
            .iter()
            .fold(0u64, |acc, e| acc.saturating_add(e.unpacked_size));
        if solid_sum > MAX_SOLID_DRAIN_BYTES || (idx - start) > 256 {
            return None;
        }
    }
    Some(name)
}

fn find_cover_rar_walk<R: Read + Seek>(
    reader: &mut R,
    opt: &CoverOptions,
    exclude: &[String],
) -> Result<CoverCandidate, WalkError> {
    let deadline = Instant::now() + opt.scan_timeout;
    let unsorted = !opt.sort;
    let walked = rar_walk::walk(reader, deadline, MAX_SCAN_ENTRIES, |entries| {
        // Unsorted mode takes the first usable image, so the walk can stop right there.
        unsorted && walk_candidate_name(entries, entries.len() - 1, opt, exclude).is_some()
    })
    .map_err(WalkError::Fallback)?;

    let mut has_risk = false;
    let mut best_idx: Option<usize> = None;
    let mut best_name = String::new();
    let mut image_entries: Vec<(usize, String)> = Vec::new();
    for (idx, entry) in walked.entries.iter().enumerate() {
        if inspect_entry_security_with_rules(
            &entry.name,
            entry.unpacked_size,
            entry.packed_size,
            false,
            Some(&opt.risk_rules),
        )
        .is_some()
        {
            has_risk = true;
        }
        let Some(name) = walk_candidate_name(&walked.entries, idx, opt, exclude) else {
            continue;
        };
        let is_better = best_idx.is_none() || (opt.sort && natural_cmp_less(&name, &best_name));
        image_entries.push((idx, name.clone()));
        if is_better {
            best_idx = Some(idx);
            best_name = name;
            if !opt.sort {
                break;
            }
        }
    }
    if opt.sort && opt.prefer_continuous_sequence && image_entries.len() > 1 {
        if let Some(chosen_idx) = crate::archive::pick_best_cover_index(&image_entries, opt.sort, true) {
            if let Some((_, chosen_name)) = image_entries.iter().find(|(i, _)| *i == chosen_idx) {
                best_idx = Some(chosen_idx);
                best_name = chosen_name.clone();
            }
        }
    }
    let Some(idx) = best_idx else {
        return Err(WalkError::Final("No valid cover image candidate found in RAR archive".into()));
    };
    let chosen = walked.entries[idx].clone();

    let budget = opt
        .max_image_bytes
        .saturating_mul(2)
        .clamp(16 * 1024 * 1024, MAX_RAR_PREFIX_BYTES);
    let mini = rar_walk::build_minimal_archive(reader, &walked, idx, budget).map_err(WalkError::Fallback)?;
    let arc = Archive::open_lenient(&mini).map_err(|e| WalkError::Fallback(format!("Invalid RAR archive: {e:?}")))?;
    if arc.encrypted {
        return Err(WalkError::Final("RAR archive has encrypted headers".into()));
    }
    let entries = arc.entries();
    // The chosen entry is the last file block of the minimal archive.
    let target = entries
        .iter()
        .rev()
        .find(|e| e.kind == EntryKind::File && e.data_offset.is_some())
        .ok_or_else(|| WalkError::Fallback("RAR minimal archive has no file entry".into()))?;
    if target.unpacked_size != chosen.unpacked_size
        || target.packed_size != chosen.packed_size
        || target.encrypted
        || target.dictionary_size > MAX_RAR_DICT_BYTES
    {
        return Err(WalkError::Fallback("RAR minimal archive disagrees with walked headers".into()));
    }
    let data = arc
        .extract(target)
        .map_err(|e| WalkError::Fallback(format!("RAR decompression failed: {e:?}")))?;
    if data.is_empty() || (data.len() as u64) > opt.max_image_bytes {
        return Err(WalkError::Final("Extracted RAR cover violates size constraints".into()));
    }
    Ok(CoverCandidate {
        name: best_name,
        data,
        from_epub_metadata: false,
        is_epub: false,
        container: ContainerFormat::Rar,
        has_risk: opt.anomaly_detection && has_risk,
    })
}

/// Bounded-prefix fallback: reads up to 16–96 MiB and parses it in memory.
fn find_cover_rar_prefix<R: Read + Seek>(
    reader: &mut R,
    opt: &CoverOptions,
    exclude: &[String],
) -> Result<CoverCandidate, String> {
    let deadline = Instant::now() + opt.scan_timeout;
    let prefix_limit = opt
        .max_image_bytes
        .saturating_mul(2)
        .clamp(16 * 1024 * 1024, MAX_RAR_PREFIX_BYTES);
    let raw = read_prefix_bounded(reader, prefix_limit)?;

    let arc = Archive::open_lenient(&raw).map_err(|e| format!("Invalid RAR archive: {e:?}"))?;
    if arc.encrypted {
        return Err("RAR archive has encrypted headers".into());
    }

    let entries = arc.entries();
    let mut best_idx: Option<usize> = None;
    let mut image_entries: Vec<(usize, String)> = Vec::new();
    let mut best_name = String::new();
    let mut has_risk = false;

    for (idx, entry) in entries.iter().take(MAX_SCAN_ENTRIES).enumerate() {
        if Instant::now() > deadline {
            break;
        }
        if inspect_entry_security_with_rules(
            &entry.name,
            entry.unpacked_size,
            entry.packed_size,
            false,
            Some(&opt.risk_rules),
        )
        .is_some()
        {
            has_risk = true;
        }
        if entry.kind != EntryKind::File || entry.encrypted || entry.split_before || entry.split_after {
            continue;
        }
        let Some(name) = sanitize_archive_path(&entry.name) else {
            continue;
        };
        if name.is_empty() || name.ends_with('/') || is_junk_name(&name) || !is_image_name(&name) {
            continue;
        }
        if exclude.iter().any(|ex| ex == &name) {
            continue;
        }

        // Security checks against RAR decompression bombs
        if entry.unpacked_size == 0 || entry.unpacked_size > opt.max_image_bytes {
            continue;
        }
        if entry.dictionary_size > MAX_RAR_DICT_BYTES {
            continue;
        }
        if entry.unpacked_size > 8 * 1024 * 1024
            && (entry.packed_size == 0
                || (entry.unpacked_size / entry.packed_size) > MAX_COMPRESSION_RATIO)
        {
            continue;
        }

        // Verify the entry's packed data actually fits inside the bounded prefix buffer
        let Some(data_off) = entry.data_offset else {
            continue;
        };
        let Some(data_end) = data_off.checked_add(entry.packed_size as usize) else {
            continue;
        };
        if data_end > raw.len() {
            continue;
        }

        // If the entry is part of a solid group, verify cumulative solid group unpacked bytes
        if entry.solid {
            let mut start = idx;
            let mut solid_sum = 0u64;
            while start > 0 && (entries[start].solid || entries[start].data_offset.is_none()) {
                solid_sum = solid_sum.saturating_add(entries[start].unpacked_size);
                start -= 1;
            }
            solid_sum = solid_sum.saturating_add(entries[start].unpacked_size);
            if solid_sum > MAX_SOLID_DRAIN_BYTES || (idx - start) > 256 {
                continue;
            }
        }

        let is_better = best_idx.is_none() || (opt.sort && natural_cmp_less(&name, &best_name));
        image_entries.push((idx, name.clone()));
        if is_better {
            best_idx = Some(idx);
            best_name = name;
            if !opt.sort {
                break;
            }
        }
    }

    if opt.sort && opt.prefer_continuous_sequence && image_entries.len() > 1 {
        if let Some(chosen_idx) = crate::archive::pick_best_cover_index(&image_entries, opt.sort, true) {
            best_idx = Some(chosen_idx);
            if let Some((_, chosen_name)) = image_entries.iter().find(|(i, _)| *i == chosen_idx) {
                best_name = chosen_name.clone();
            }
        }
    }

    let idx = best_idx.ok_or_else(|| "No valid cover image candidate found in RAR archive".to_string())?;
    let data = arc
        .extract(&entries[idx])
        .map_err(|e| format!("RAR decompression failed: {e:?}"))?;
    if data.is_empty() || (data.len() as u64) > opt.max_image_bytes {
        return Err("Extracted RAR cover violates size constraints".into());
    }

    Ok(CoverCandidate {
        name: best_name,
        data,
        from_epub_metadata: false,
        is_epub: false,
        container: ContainerFormat::Rar,
        has_risk: opt.anomaly_detection && has_risk,
    })
}

/// Counts total files and image files inside a RAR4 or RAR5 archive.
///
/// Walks block headers with seeks (header bytes only); falls back to the prefix parser when the
/// walker cannot handle the stream.
pub fn count_entries_rar<R: Read + Seek>(
    reader: &mut R,
    timeout: Duration,
) -> Result<ArchiveStats, String> {
    let deadline = Instant::now() + timeout;
    let walked = match rar_walk::walk(reader, deadline, MAX_SCAN_ENTRIES, |_| false) {
        Ok(w) => w,
        Err(_) => return count_entries_rar_prefix(reader, timeout),
    };
    let mut stats = ArchiveStats {
        images: 0,
        files: 0,
        complete: walked.complete,
        has_risk: false,
        risk_reasons: Vec::new(),
    };
    for entry in &walked.entries {
        if inspect_entry_security(&entry.name, entry.unpacked_size, entry.packed_size, false).is_some() {
            stats.has_risk = true;
        }
        if entry.is_dir {
            continue;
        }
        let Some(name) = sanitize_archive_path(&entry.name) else {
            continue;
        };
        if name.is_empty() || name.ends_with('/') {
            continue;
        }
        stats.files += 1;
        if !is_junk_name(&name) && is_image_name(&name) {
            stats.images += 1;
        }
    }
    Ok(stats)
}

fn count_entries_rar_prefix<R: Read + Seek>(
    reader: &mut R,
    timeout: Duration,
) -> Result<ArchiveStats, String> {
    let deadline = Instant::now() + timeout;
    let total_len = reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    let prefix = read_prefix_bounded(reader, MAX_RAR_PREFIX_BYTES)?;
    let arc = Archive::open_lenient(&prefix).map_err(|e| format!("Invalid RAR archive: {e:?}"))?;

    let mut stats = ArchiveStats {
        images: 0,
        files: 0,
        complete: total_len <= MAX_RAR_PREFIX_BYTES,
        has_risk: false,
        risk_reasons: Vec::new(),
    };

    for (idx, entry) in arc.entries().iter().enumerate() {
        if idx >= MAX_SCAN_ENTRIES || Instant::now() > deadline {
            stats.complete = false;
            break;
        }
        if inspect_entry_security(&entry.name, entry.unpacked_size, entry.packed_size, false).is_some() {
            stats.has_risk = true;
        }
        if entry.kind != EntryKind::File {
            continue;
        }
        let Some(name) = sanitize_archive_path(&entry.name) else {
            continue;
        };
        if name.is_empty() || name.ends_with('/') {
            continue;
        }
        stats.files += 1;
        if !is_junk_name(&name) && is_image_name(&name) {
            stats.images += 1;
        }
    }

    Ok(stats)
}
