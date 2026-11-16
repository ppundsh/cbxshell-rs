use std::io::{Read, Seek, SeekFrom};

/// Attempts to extract embedded album cover art (JPEG, PNG, WebP) from an audio or media stream.
/// Supports:
/// 1. MP3 (ID3v2 APIC frame)
/// 2. MP4 / M4A / AAC (`moov.udta.meta.ilst.covr.data` atom)
/// 3. FLAC (`METADATA_BLOCK_PICTURE` block 6)
/// 4. Ogg / Opus (Vorbis comment `METADATA_BLOCK_PICTURE`)
pub fn extract_embedded_cover<R: Read + Seek>(reader: &mut R) -> Option<Vec<u8>> {
    let _ = reader.seek(SeekFrom::Start(0));
    let mut header = [0u8; 16];
    if reader.read_exact(&mut header).is_err() {
        return None;
    }
    let _ = reader.seek(SeekFrom::Start(0));

    // 1. Check ID3v2 (starts with "ID3")
    if &header[0..3] == b"ID3" {
        if let Some(data) = extract_id3v2_apic(reader) {
            return Some(data);
        }
    }

    // 2. Check FLAC (starts with "fLaC")
    if &header[0..4] == b"fLaC" {
        if let Some(data) = extract_flac_picture(reader) {
            return Some(data);
        }
    }

    // 3. Check MP4 / M4A / QuickTime (atoms usually "ftyp", "moov", etc.)
    if &header[4..8] == b"ftyp" || &header[4..8] == b"moov" {
        if let Some(data) = extract_mp4_covr(reader) {
            return Some(data);
        }
    }

    // 4. Check OGG (starts with "OggS")
    if &header[0..4] == b"OggS" {
        if let Some(data) = extract_ogg_picture(reader) {
            return Some(data);
        }
    }

    None
}

/// Parses ID3v2 header and frames searching for the 'APIC' attached picture frame.
fn extract_id3v2_apic<R: Read + Seek>(reader: &mut R) -> Option<Vec<u8>> {
    let _ = reader.seek(SeekFrom::Start(0));
    let mut hdr = [0u8; 10];
    reader.read_exact(&mut hdr).ok()?;
    if &hdr[0..3] != b"ID3" {
        return None;
    }

    let version = hdr[3]; // e.g. 3 for ID3v2.3, 4 for ID3v2.4
    let tag_size = synchsafe_to_u32(&hdr[6..10]) as u64;
    let tag_end = 10 + tag_size;

    // Scan frames within tag_end. The frame count is capped: a 256 MiB tag of 11-byte frames
    // would otherwise cost ~25 million seek+read round-trips (each one a cross-process COM call
    // when reading from a Shell IStream) and keep running after the watchdog gives up.
    let mut cur_pos = 10u64;
    let mut frames_seen = 0usize;
    while cur_pos + 10 < tag_end && frames_seen < MAX_ID3_FRAMES {
        frames_seen += 1;
        if reader.seek(SeekFrom::Start(cur_pos)).is_err() {
            break;
        }
        let mut f_hdr = [0u8; 10];
        if reader.read_exact(&mut f_hdr).is_err() {
            break;
        }

        // Check for padding (null frame ID)
        if f_hdr[0] == 0 {
            break;
        }

        let frame_id = &f_hdr[0..4];
        let frame_size = if version == 4 {
            synchsafe_to_u32(&f_hdr[4..8]) as usize
        } else {
            u32::from_be_bytes([f_hdr[4], f_hdr[5], f_hdr[6], f_hdr[7]]) as usize
        };

        if frame_size == 0 || cur_pos + 10 + frame_size as u64 > tag_end + 1024 {
            break;
        }

        if frame_id == b"APIC" {
            // Read frame payload (limit to 32 MB)
            if frame_size > 32 * 1024 * 1024 {
                return None;
            }
            let mut payload = vec![0u8; frame_size];
            if reader.read_exact(&mut payload).is_err() {
                return None;
            }
            return parse_apic_payload(&payload);
        }

        cur_pos += 10 + frame_size as u64;
    }

    None
}

fn synchsafe_to_u32(bytes: &[u8]) -> u32 {
    let b0 = (bytes[0] & 0x7F) as u32;
    let b1 = (bytes[1] & 0x7F) as u32;
    let b2 = (bytes[2] & 0x7F) as u32;
    let b3 = (bytes[3] & 0x7F) as u32;
    (b0 << 21) | (b1 << 14) | (b2 << 7) | b3
}

fn parse_apic_payload(payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() < 5 {
        return None;
    }
    let encoding = payload[0];
    let mut idx = 1;

    // MIME type is null-terminated ASCII / ISO-8859-1
    while idx < payload.len() && payload[idx] != 0 {
        idx += 1;
    }
    if idx >= payload.len() {
        return None;
    }
    idx += 1; // skip null terminator

    if idx >= payload.len() {
        return None;
    }
    let _picture_type = payload[idx];
    idx += 1;

    // Skip description string according to encoding
    if encoding == 1 || encoding == 2 {
        // UTF-16: two null bytes
        while idx + 1 < payload.len() {
            if payload[idx] == 0 && payload[idx + 1] == 0 {
                idx += 2;
                break;
            }
            idx += 2;
        }
    } else {
        // ISO-8859-1 or UTF-8: single null byte
        while idx < payload.len() && payload[idx] != 0 {
            idx += 1;
        }
        if idx < payload.len() {
            idx += 1;
        }
    }

    if idx < payload.len() {
        Some(payload[idx..].to_vec())
    } else {
        None
    }
}

/// Parses FLAC metadata blocks searching for block type 6 (`METADATA_BLOCK_PICTURE`).
fn extract_flac_picture<R: Read + Seek>(reader: &mut R) -> Option<Vec<u8>> {
    let _ = reader.seek(SeekFrom::Start(4)); // skip "fLaC"
    // FLAC allows at most a handful of metadata blocks in practice; cap the walk so a stream of
    // empty 4-byte headers cannot turn into millions of iterations.
    for _ in 0..MAX_FLAC_BLOCKS {
        let mut blk_hdr = [0u8; 4];
        if reader.read_exact(&mut blk_hdr).is_err() {
            break;
        }
        let is_last = (blk_hdr[0] & 0x80) != 0;
        let blk_type = blk_hdr[0] & 0x7F;
        let blk_len = ((blk_hdr[1] as usize) << 16) | ((blk_hdr[2] as usize) << 8) | (blk_hdr[3] as usize);

        if blk_type == 6 {
            // Picture block
            if blk_len < 32 || blk_len > 32 * 1024 * 1024 {
                return None;
            }
            let mut data = vec![0u8; blk_len];
            reader.read_exact(&mut data).ok()?;

            // Picture block layout:
            // 4 bytes: picture type
            // 4 bytes: MIME type length (M)
            // M bytes: MIME type
            // 4 bytes: description length (D)
            // D bytes: description
            // 4 bytes: width, 4 bytes: height, 4 bytes: depth, 4 bytes: colors
            // 4 bytes: picture data length (L)
            // L bytes: picture data
            if data.len() < 8 {
                return None;
            }
            // All offsets use checked arithmetic: the length fields are attacker-controlled u32s.
            let rd = |o: usize| -> Option<usize> {
                let b = data.get(o..o.checked_add(4)?)?;
                Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
            };
            let mime_len = rd(4)?;
            let mut off = 8usize.checked_add(mime_len)?;
            let desc_len = rd(off)?;
            off = off.checked_add(4)?.checked_add(desc_len)?.checked_add(16)?; // desc + w/h/depth/colors
            let pic_len = rd(off)?;
            off = off.checked_add(4)?;
            let end = off.checked_add(pic_len)?;
            return data.get(off..end).map(|s| s.to_vec());
        }

        if reader.seek(SeekFrom::Current(blk_len as i64)).is_err() || is_last {
            break;
        }
    }
    None
}

const MAX_ID3_FRAMES: usize = 4096;
const MAX_FLAC_BLOCKS: usize = 1024;
const MAX_COVR_CHILD_ATOMS: usize = 64;

/// Recursively traverses MP4 atoms looking for 'covr' data.
fn extract_mp4_covr<R: Read + Seek>(reader: &mut R) -> Option<Vec<u8>> {
    let file_len = reader.seek(SeekFrom::End(0)).ok()?;
    let _ = reader.seek(SeekFrom::Start(0));
    find_atom_and_extract_covr(reader, 0, file_len, 0)
}

fn find_atom_and_extract_covr<R: Read + Seek>(
    reader: &mut R,
    start: u64,
    end: u64,
    depth: usize,
) -> Option<Vec<u8>> {
    if depth > 5 {
        return None;
    }
    let mut pos = start;
    let mut atoms_checked = 0usize;
    while pos + 8 <= end && atoms_checked < 64 {
        atoms_checked += 1;
        if reader.seek(SeekFrom::Start(pos)).is_err() {
            break;
        }
        let mut hdr = [0u8; 8];
        if reader.read_exact(&mut hdr).is_err() {
            break;
        }
        let size32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let atom_name = &hdr[4..8];

        let atom_size = if size32 == 1 {
            let mut size64_bytes = [0u8; 8];
            if reader.read_exact(&mut size64_bytes).is_err() {
                break;
            }
            u64::from_be_bytes(size64_bytes)
        } else if size32 == 0 {
            end - pos
        } else {
            size32
        };

        if atom_size < 8 {
            break;
        }

        let payload_start = if size32 == 1 { pos.saturating_add(16) } else { pos + 8 };
        let payload_end = pos.saturating_add(atom_size).min(end);
        if payload_start > payload_end {
            break;
        }

        // Container atoms: moov, udta, meta, ilst
        if atom_name == b"moov" || atom_name == b"udta" || atom_name == b"ilst" {
            if let Some(pic) = find_atom_and_extract_covr(reader, payload_start, payload_end, depth + 1) {
                return Some(pic);
            }
        } else if atom_name == b"meta" {
            // 'meta' in MP4 usually has a 4-byte version/flags before child atoms
            let meta_child_start = (payload_start + 4).min(payload_end);
            if let Some(pic) = find_atom_and_extract_covr(reader, meta_child_start, payload_end, depth + 1) {
                return Some(pic);
            }
        } else if atom_name == b"covr" {
            // 'covr' contains a 'data' atom
            return extract_covr_data(reader, payload_start, payload_end);
        }

        let next_pos = pos.saturating_add(atom_size);
        if next_pos <= pos {
            break;
        }
        pos = next_pos;
    }
    None
}

fn extract_covr_data<R: Read + Seek>(reader: &mut R, start: u64, end: u64) -> Option<Vec<u8>> {
    let mut pos = start;
    let mut atoms_seen = 0usize;
    while pos + 8 <= end && atoms_seen < MAX_COVR_CHILD_ATOMS {
        atoms_seen += 1;
        if reader.seek(SeekFrom::Start(pos)).is_err() {
            break;
        }
        let mut hdr = [0u8; 8];
        if reader.read_exact(&mut hdr).is_err() {
            break;
        }
        let size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let name = &hdr[4..8];
        // A zero or sub-header atom size would never advance `pos` (infinite loop on crafted files).
        if size < 8 {
            break;
        }
        if name == b"data" && size >= 16 {
            // data atom has 8 bytes header + 4 bytes type/flags + 4 bytes locale = 16 bytes
            let data_len = (size - 16) as usize;
            if data_len > 32 * 1024 * 1024 {
                return None;
            }
            let _ = reader.seek(SeekFrom::Start(pos + 16));
            let mut buf = vec![0u8; data_len];
            if reader.read_exact(&mut buf).is_ok() {
                return Some(buf);
            }
        }
        pos = match pos.checked_add(size) {
            Some(next) if next > pos => next,
            _ => break,
        };
    }
    None
}

/// Quick scanner for Ogg Vorbis `METADATA_BLOCK_PICTURE` in first 2 MB
fn extract_ogg_picture<R: Read + Seek>(reader: &mut R) -> Option<Vec<u8>> {
    let _ = reader.seek(SeekFrom::Start(0));
    let mut buf = vec![0u8; 1024 * 1024]; // 1MB scan
    let n = reader.read(&mut buf).ok()?;
    let slice = &buf[..n];

    let pattern = b"METADATA_BLOCK_PICTURE=";
    if let Some(pos) = slice.windows(pattern.len()).position(|w| w == pattern) {
        let start = pos + pattern.len();
        let mut end = start;
        while end < slice.len() && (slice[end].is_ascii_alphanumeric() || slice[end] == b'+' || slice[end] == b'/' || slice[end] == b'=') {
            end += 1;
        }
        let b64_str = std::str::from_utf8(&slice[start..end]).ok()?;
        // Decode base64 and parse FLAC picture block
        if let Some(bin) = simple_base64_decode(b64_str) {
            let mut cur = std::io::Cursor::new(bin);
            return extract_flac_picture_from_bytes(&mut cur);
        }
    }
    None
}

fn extract_flac_picture_from_bytes<R: Read>(reader: &mut R) -> Option<Vec<u8>> {
    // Length fields are attacker-controlled u32 values: never allocate from them directly.
    // Skip via a bounded `take` so a forged 4 GiB length cannot trigger an allocation abort.
    const MAX_MIME_LEN: u64 = 256;
    const MAX_DESC_LEN: u64 = 64 * 1024;
    fn skip_exact<R: Read>(reader: &mut R, n: u64) -> Option<()> {
        let copied = std::io::copy(&mut reader.take(n), &mut std::io::sink()).ok()?;
        (copied == n).then_some(())
    }

    let mut hdr = [0u8; 8];
    reader.read_exact(&mut hdr).ok()?;
    let mime_len = u32::from_be_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as u64;
    if mime_len > MAX_MIME_LEN {
        return None;
    }
    skip_exact(reader, mime_len)?;

    let mut desc_hdr = [0u8; 4];
    reader.read_exact(&mut desc_hdr).ok()?;
    let desc_len = u32::from_be_bytes(desc_hdr) as u64;
    if desc_len > MAX_DESC_LEN {
        return None;
    }
    skip_exact(reader, desc_len + 16)?; // desc + width/height/depth/colors

    let mut pic_len_bytes = [0u8; 4];
    reader.read_exact(&mut pic_len_bytes).ok()?;
    let pic_len = u32::from_be_bytes(pic_len_bytes) as usize;
    if pic_len > 32 * 1024 * 1024 {
        return None;
    }
    let mut pic = vec![0u8; pic_len];
    reader.read_exact(&mut pic).ok()?;
    Some(pic)
}

fn simple_base64_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity((input.len() * 3) / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for &b in input.as_bytes() {
        let val = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => continue,
            _ => continue,
        };
        buf = (buf << 6) | (val as u32);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Some(out)
}
