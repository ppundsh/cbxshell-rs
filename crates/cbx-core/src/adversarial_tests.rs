//! Adversarial test harness aimed at *unknown* attack shapes.
//!
//! Known-exploit regression tests only prove that yesterday's bug stays fixed. This module adds
//! oracles that flag whole *classes* of new bugs, regardless of the concrete input that triggers
//! them:
//!
//! | Oracle | Catches |
//! | --- | --- |
//! | no panic (`catch_unwind`) | out-of-bounds slicing, arithmetic overflow, `unwrap` on hostile data |
//! | per-case wall-clock budget | infinite loops, algorithmic blow-ups, runaway work after the watchdog |
//! | I/O amplification budget (instrumented reader) | length-field loops that re-seek/re-read millions of times |
//! | output-dimension bound | decoder trusting header dimensions |
//! | sanitizer invariants (property tests) | path traversal / ADS / device-name / BiDi bypasses |
//! | repository privacy scan | build-machine paths, user names, e-mail addresses in sources |
//!
//! Inputs come from *structure-aware* mutators applied to a seed corpus covering every parser
//! (ZIP/CBZ, EPUB, OOXML, TAR, 7z, RAR, PSD, ID3, FLAC, MP4, Ogg, MOBI, torrent, text):
//! "lying length" (boundary integers written over length fields), chunk amplification
//! (repeat a slice many times), splicing between formats (content-type confusion), truncation
//! and plain byte flips. The PRNG is deterministic so every failure is reproducible from the
//! printed iteration number; set `CBX_FUZZ_ITERS` to run longer campaigns locally.

use crate::archive::{sanitize_archive_path, verify_7z_signature_header, CoverOptions};
use crate::settings::Settings;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Deterministic PRNG

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

// ---------------------------------------------------------------------------
// Instrumented reader: counts I/O operations to detect amplification

struct CountingReader<R> {
    inner: R,
    ops: u64,
    bytes: u64,
}

impl<R> CountingReader<R> {
    fn new(inner: R) -> Self {
        Self { inner, ops: 0, bytes: 0 }
    }
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.ops += 1;
        let n = self.inner.read(buf)?;
        self.bytes += n as u64;
        Ok(n)
    }
}

impl<R: Seek> Seek for CountingReader<R> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.ops += 1;
        self.inner.seek(pos)
    }
}

/// A *virtual* stream of `len` bytes: `prefix` followed by `pattern` repeated forever.
/// Lets tests model multi-hundred-MiB hostile files without allocating them.
struct VirtualStream {
    prefix: Vec<u8>,
    pattern: Vec<u8>,
    len: u64,
    pos: u64,
}

impl Read for VirtualStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0;
        while n < buf.len() && self.pos < self.len {
            let p = self.pos as usize;
            buf[n] = if p < self.prefix.len() {
                self.prefix[p]
            } else {
                self.pattern[(p - self.prefix.len()) % self.pattern.len()]
            };
            n += 1;
            self.pos += 1;
        }
        Ok(n)
    }
}

impl Seek for VirtualStream {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let new = match pos {
            SeekFrom::Start(o) => o as i128,
            SeekFrom::Current(o) => self.pos as i128 + o as i128,
            SeekFrom::End(o) => self.len as i128 + o as i128,
        };
        if new < 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "negative seek"));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}

// ---------------------------------------------------------------------------
// Seed corpus

fn png_bytes(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([200, 30, 90, 255]));
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("png encode");
    out
}

fn zip_of(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cur = Cursor::new(Vec::new());
    {
        let mut zw = zip::ZipWriter::new(&mut cur);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in entries {
            let _ = zw.start_file(*name, opts);
            let _ = zw.write_all(data);
        }
        let _ = zw.finish();
    }
    cur.into_inner()
}

fn tar_of(name: &str, data: &[u8]) -> Vec<u8> {
    let mut cur = Cursor::new(Vec::new());
    {
        let mut tb = tar::Builder::new(&mut cur);
        let mut hdr = tar::Header::new_gnu();
        hdr.set_size(data.len() as u64);
        let _ = hdr.set_path(name);
        hdr.set_cksum();
        let _ = tb.append(&hdr, Cursor::new(data));
        let _ = tb.finish();
    }
    cur.into_inner()
}

fn seven_z_seed() -> Vec<u8> {
    // Signature header pointing at an 8-byte "next header" block directly after it.
    let mut v = b"7z\xBC\xAF\x27\x1C\x00\x04".to_vec();
    v.extend_from_slice(&[0u8; 4]); // start header CRC (wrong on purpose)
    v.extend_from_slice(&0u64.to_le_bytes()); // next header offset
    v.extend_from_slice(&8u64.to_le_bytes()); // next header size
    v.extend_from_slice(&[0u8; 4]); // next header CRC
    v.extend_from_slice(&[0x01, 0x04, 0x06, 0x00, 0x00, 0x00, 0x00, 0x00]);
    v
}

fn mobi_seed(cover: &[u8]) -> Vec<u8> {
    let mut v = vec![0u8; 78];
    v[60..68].copy_from_slice(b"BOOKMOBI");
    v[76..78].copy_from_slice(&2u16.to_be_bytes());
    let rec0_off = 78 + 2 * 8 + 2;
    let rec0_len = 256;
    let rec1_off = rec0_off + rec0_len;
    v.extend_from_slice(&(rec0_off as u32).to_be_bytes());
    v.extend_from_slice(&[0u8; 4]);
    v.extend_from_slice(&(rec1_off as u32).to_be_bytes());
    v.extend_from_slice(&[0u8; 4]);
    v.extend_from_slice(&[0u8; 2]);
    let mut rec0 = vec![0u8; rec0_len];
    rec0[16..20].copy_from_slice(b"MOBI");
    rec0[20..24].copy_from_slice(&232u32.to_be_bytes());
    rec0[108..112].copy_from_slice(&1u32.to_be_bytes()); // first image record
    v.extend_from_slice(&rec0);
    v.extend_from_slice(cover);
    v
}

fn id3_seed(pic: &[u8]) -> Vec<u8> {
    let mut frame = Vec::new();
    frame.push(0u8); // encoding
    frame.extend_from_slice(b"image/png\0");
    frame.push(3); // front cover
    frame.push(0); // empty description
    frame.extend_from_slice(pic);
    let mut tag = Vec::new();
    tag.extend_from_slice(b"APIC");
    tag.extend_from_slice(&(frame.len() as u32).to_be_bytes());
    tag.extend_from_slice(&[0, 0]);
    tag.extend_from_slice(&frame);
    let size = tag.len() as u32;
    let synch = [
        ((size >> 21) & 0x7F) as u8,
        ((size >> 14) & 0x7F) as u8,
        ((size >> 7) & 0x7F) as u8,
        (size & 0x7F) as u8,
    ];
    let mut v = b"ID3\x03\x00\x00".to_vec();
    v.extend_from_slice(&synch);
    v.extend_from_slice(&tag);
    v
}

fn flac_seed(pic: &[u8]) -> Vec<u8> {
    let mut block = Vec::new();
    block.extend_from_slice(&3u32.to_be_bytes());
    block.extend_from_slice(&9u32.to_be_bytes());
    block.extend_from_slice(b"image/png");
    block.extend_from_slice(&0u32.to_be_bytes());
    block.extend_from_slice(&[0u8; 16]);
    block.extend_from_slice(&(pic.len() as u32).to_be_bytes());
    block.extend_from_slice(pic);
    let mut v = b"fLaC".to_vec();
    v.push(0x86);
    let l = block.len() as u32;
    v.extend_from_slice(&[(l >> 16) as u8, (l >> 8) as u8, l as u8]);
    v.extend_from_slice(&block);
    v
}

fn mp4_seed(pic: &[u8]) -> Vec<u8> {
    fn atom(name: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(name);
        v.extend_from_slice(body);
        v
    }
    let mut data_body = vec![0u8; 8];
    data_body.extend_from_slice(pic);
    let covr = atom(b"covr", &atom(b"data", &data_body));
    let ilst = atom(b"ilst", &covr);
    let mut meta_body = vec![0u8; 4];
    meta_body.extend_from_slice(&ilst);
    let meta = atom(b"meta", &meta_body);
    let udta = atom(b"udta", &meta);
    let moov = atom(b"moov", &udta);
    let mut v = atom(b"ftyp", b"M4A \x00\x00\x00\x00isom");
    v.extend_from_slice(&moov);
    v
}

fn torrent_seed() -> Vec<u8> {
    b"d8:announce20:https://tracker.test/4:infod5:filesld6:lengthi10e4:pathl3:dir9:cover.jpgeed6:lengthi5e4:pathl8:read.txteee4:name4:Pack12:piece lengthi16384e6:pieces20:01234567890123456789ee".to_vec()
}

fn psd_seed() -> Vec<u8> {
    let mut v = b"8BPS".to_vec();
    v.extend_from_slice(&1u16.to_be_bytes());
    v.extend_from_slice(&[0u8; 6]);
    v.extend_from_slice(&3u16.to_be_bytes()); // channels
    v.extend_from_slice(&4u32.to_be_bytes()); // height
    v.extend_from_slice(&4u32.to_be_bytes()); // width
    v.extend_from_slice(&8u16.to_be_bytes()); // depth
    v.extend_from_slice(&3u16.to_be_bytes()); // RGB
    v.extend_from_slice(&0u32.to_be_bytes()); // color mode
    v.extend_from_slice(&0u32.to_be_bytes()); // resources
    v.extend_from_slice(&0u32.to_be_bytes()); // layer/mask
    v.extend_from_slice(&0u16.to_be_bytes()); // raw
    v.extend_from_slice(&[0x80u8; 48]);
    v
}

fn corpus() -> Vec<(&'static str, Vec<u8>)> {
    let png = png_bytes(6, 9);
    let container = br#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#;
    let opf = br#"<package><metadata><meta name="cover" content="c"/></metadata><manifest><item id="c" href="../../img/cover.png" media-type="image/png"/></manifest></package>"#;
    let docx_xml = br#"<w:document><w:body><w:p><w:r><w:t>Title</w:t></w:r></w:p><w:p><w:r><w:t>password = hunter2</w:t></w:r></w:p></w:body></w:document>"#;
    let xlsx_ss = br#"<sst><si><t>name</t></si><si><t>token</t></si></sst>"#;
    let xlsx_ws = br#"<worksheet><sheetData><row><c t="s"><v>0</v></c><c t="s"><v>1</v></c></row><row><c><v>1</v></c><c t="inlineStr"><is><t>x</t></is></c></row></sheetData></worksheet>"#;
    vec![
        (".cbz", zip_of(&[("001.png", &png), ("002.png", &png)])),
        (
            ".epub",
            zip_of(&[
                ("mimetype", b"application/epub+zip"),
                ("META-INF/container.xml", container),
                ("OEBPS/content.opf", opf),
                ("img/cover.png", &png),
            ]),
        ),
        (".docx", zip_of(&[("word/document.xml", docx_xml)])),
        (
            ".xlsx",
            zip_of(&[("xl/sharedStrings.xml", xlsx_ss), ("xl/worksheets/sheet1.xml", xlsx_ws)]),
        ),
        (".cbt", tar_of("001.png", &png)),
        (".cb7", seven_z_seed()),
        (".cbr", b"Rar!\x1A\x07\x01\x00\x33\x92\xB5\xE5\x0A\x01\x05\x06\x00\x05\x01\x01\x80\x80\x00".to_vec()),
        (".psd", psd_seed()),
        (".mp3", id3_seed(&png)),
        (".flac", flac_seed(&png)),
        (".m4a", mp4_seed(&png)),
        (".mobi", mobi_seed(&png)),
        (".torrent", torrent_seed()),
        (".txt", b"user = alice\napi_key = 0123456789abcdefXYZ\nhello \xE2\x80\xAE world\n".to_vec()),
        (".pdf", b"%PDF-1.4\n1 0 obj<<>>endobj\ntrailer<<>>\n%%EOF\n".to_vec()),
    ]
}

// ---------------------------------------------------------------------------
// Structure-aware mutators

const BOUNDARY_U32: &[u32] = &[0, 1, 7, 8, 0x7F, 0x80, 0xFF, 0xFFFF, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF_FFFE, 0xFFFF_FFFF];

fn mutate(rng: &mut Rng, base: &[u8], other: &[u8]) -> Vec<u8> {
    let mut buf = base.to_vec();
    let rounds = 1 + rng.below(4);
    for _ in 0..rounds {
        if buf.is_empty() {
            buf.push(0);
        }
        match rng.below(7) {
            // byte flip
            0 => {
                let i = rng.below(buf.len());
                buf[i] ^= (rng.next() & 0xFF) as u8 | 1;
            }
            // "lying length": overwrite 2/4/8 bytes with a boundary integer, BE or LE
            1 | 2 => {
                let width = [2usize, 4, 8][rng.below(3)];
                if buf.len() > width {
                    let i = rng.below(buf.len() - width);
                    let v = BOUNDARY_U32[rng.below(BOUNDARY_U32.len())] as u64;
                    let v = if width == 8 && rng.below(2) == 0 { v << 32 | v } else { v };
                    let bytes = if rng.below(2) == 0 { v.to_be_bytes() } else { v.to_le_bytes() };
                    let src = if rng.below(2) == 0 { &bytes[8 - width..] } else { &bytes[..width] };
                    buf[i..i + width].copy_from_slice(src);
                }
            }
            // chunk amplification: repeat a slice (loop / allocation amplification)
            3 => {
                let start = rng.below(buf.len());
                let len = 1 + rng.below((buf.len() - start).min(64));
                let chunk = buf[start..start + len].to_vec();
                let reps = 2 + rng.below(256);
                let mut out = Vec::with_capacity(buf.len() + chunk.len() * reps);
                out.extend_from_slice(&buf[..start]);
                for _ in 0..reps {
                    out.extend_from_slice(&chunk);
                    if out.len() > 2 * 1024 * 1024 {
                        break;
                    }
                }
                out.extend_from_slice(&buf[start..]);
                buf = out;
            }
            // truncation
            4 => {
                let keep = rng.below(buf.len()).max(1);
                buf.truncate(keep);
            }
            // splice: content-type confusion (header of one format, body of another)
            5 => {
                if !other.is_empty() {
                    let cut = rng.below(buf.len());
                    let from = rng.below(other.len());
                    buf.truncate(cut);
                    buf.extend_from_slice(&other[from..]);
                }
            }
            // interesting byte
            _ => {
                let i = rng.below(buf.len());
                buf[i] = [0x00, 0xFF, 0x7F, 0x80, b'.', b'/', b':', b'\\'][rng.below(8)];
            }
        }
    }
    buf
}

// ---------------------------------------------------------------------------
// Oracles

/// Generous budget for unoptimised test builds; a real hang/blow-up exceeds it by orders of magnitude.
const CASE_TIME_BUDGET: Duration = Duration::from_secs(20);
/// Max reader operations for one parse. Real files need hundreds to a few thousand.
const IO_OPS_BUDGET: u64 = 2_000_000;

fn fuzz_settings() -> Settings {
    let mut s = Settings::default();
    s.max_scan_seconds = 2;
    s.max_image_mb = 16;
    s.sanitize();
    s
}

fn run_case(ext: &str, data: &[u8], settings: &Settings) -> Result<(), String> {
    let start = Instant::now();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
        // Full dispatcher (archives, ebooks, audio, documents, text fallback)
        let mut r = CountingReader::new(Cursor::new(data));
        if let Ok((hbmp, _)) = crate::create_thumbnail_from_reader_unified(&mut r, 128, settings, Some(ext)) {
            // SAFETY: we own the bitmap returned by the pipeline.
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(hbmp);
            }
        }
        if r.ops > IO_OPS_BUDGET {
            return Err(format!("I/O amplification: {} reader ops", r.ops));
        }

        // Archive pipeline returning raw pixels: bound output dimensions.
        let mut r2 = CountingReader::new(Cursor::new(data));
        if let Ok((bgra, rep)) = crate::generate_thumbnail_bytes(&mut r2, 128, settings) {
            let (w, h) = (rep.decode.out_width as usize, rep.decode.out_height as usize);
            if w > 4096 || h > 4096 || bgra.len() != w * h * 4 {
                return Err(format!("unbounded/inconsistent output {w}x{h} ({} bytes)", bgra.len()));
            }
        }
        if r2.ops > IO_OPS_BUDGET {
            return Err(format!("I/O amplification (archive): {} reader ops", r2.ops));
        }

        // Audio cover parsers on any input (content-type confusion).
        let mut r3 = CountingReader::new(Cursor::new(data));
        let _ = crate::media::extract_embedded_cover(&mut r3);
        if r3.ops > IO_OPS_BUDGET {
            return Err(format!("I/O amplification (audio): {} reader ops", r3.ops));
        }

        // Raw image decoder.
        let mut opt = crate::image_dec::DecodeOptions::default();
        opt.max_side = 96;
        if let Ok(t) = crate::image_dec::decode_to_thumbnail(data, opt) {
            if t.info.out_width > 4096 || t.info.out_height > 4096 {
                return Err("decoder produced oversized output".into());
            }
        }
        Ok(())
    }));
    let elapsed = start.elapsed();
    match outcome {
        Err(_) => Err("panic".into()),
        Ok(Err(e)) => Err(e),
        Ok(Ok(())) if elapsed > CASE_TIME_BUDGET => Err(format!("too slow: {elapsed:?}")),
        Ok(Ok(())) => Ok(()),
    }
}

#[test]
fn structure_aware_differential_fuzz_all_parsers() {
    let iters: usize = std::env::var("CBX_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(900);
    let seeds = corpus();
    let settings = fuzz_settings();
    let mut rng = Rng(0xC0FF_EE00_D15E_A5E5);

    // Every unmodified seed must be handled without tripping an oracle.
    for (ext, data) in &seeds {
        run_case(ext, data, &settings).unwrap_or_else(|e| panic!("seed {ext}: {e}"));
    }

    for iter in 0..iters {
        let (ext, base) = &seeds[iter % seeds.len()];
        let (_, other) = &seeds[rng.below(seeds.len())];
        let input = mutate(&mut rng, base, other);
        // Also try the input under a *different* extension (dispatcher confusion).
        let alt_ext = seeds[rng.below(seeds.len())].0;
        for e in [*ext, alt_ext] {
            if let Err(why) = run_case(e, &input, &settings) {
                panic!(
                    "oracle violation at iter {iter} (ext {e}, {} bytes): {why}",
                    input.len()
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Targeted resource-exhaustion regressions (each models a whole bug class)

#[test]
fn bencode_node_flood_is_bounded() {
    // `d4:infod5:filesl` + millions of empty lists: tiny values, huge node count.
    let mut t = b"d4:infod5:filesl".to_vec();
    for _ in 0..3_000_000 {
        t.extend_from_slice(b"le");
    }
    t.extend_from_slice(b"eee");
    let start = Instant::now();
    let rows = crate::other_doc::parse_torrent_rows(&t);
    assert!(start.elapsed() < Duration::from_secs(10), "bencode flood too slow");
    assert!(rows.len() <= 21);

    // Deep nesting must stop at the depth limit, not overflow the stack.
    let deep = vec![b'l'; 1_000_000];
    let _ = crate::other_doc::parse_torrent_rows(&deep);
}

#[test]
fn torrent_labels_cannot_spoof_with_bidi_or_controls() {
    let name = "photo\u{202E}gpj.exe\u{200B}\r\nx";
    let t = format!("d4:infod6:lengthi1e4:name{}:{}ee", name.len(), name);
    let rows = crate::other_doc::parse_torrent_rows(t.as_bytes());
    assert!(!rows.is_empty());
    for r in rows {
        assert!(
            !r.label.chars().any(crate::redact::is_spoofing_or_control_char),
            "spoofing char survived in {:?}",
            r.label
        );
        assert!(!r.label.contains('\n') && !r.label.contains('\r'));
    }
}

#[test]
fn seven_z_header_allocation_bomb_rejected_before_parsing() {
    let mut v = b"7z\xBC\xAF\x27\x1C\x00\x04".to_vec();
    v.extend_from_slice(&[0u8; 4]);
    v.extend_from_slice(&0u64.to_le_bytes());
    v.extend_from_slice(&0x0000_FFFF_FFFF_FFFFu64.to_le_bytes()); // forged next-header size
    v.extend_from_slice(&[0u8; 4]);
    v.extend_from_slice(&[0u8; 64]);
    assert!(verify_7z_signature_header(&mut Cursor::new(&v)).is_err());
    let opt = CoverOptions::default();
    assert!(crate::archive::find_cover(&mut Cursor::new(&v), &opt, &[]).is_err());

    // Offset pointing past EOF.
    let mut w = b"7z\xBC\xAF\x27\x1C\x00\x04".to_vec();
    w.extend_from_slice(&[0u8; 4]);
    w.extend_from_slice(&u64::MAX.to_le_bytes());
    w.extend_from_slice(&16u64.to_le_bytes());
    w.extend_from_slice(&[0u8; 4]);
    assert!(verify_7z_signature_header(&mut Cursor::new(&w)).is_err());
}

#[test]
fn id3_frame_flood_does_not_amplify_io() {
    // 256 MiB virtual ID3 tag made of 11-byte "XXXX" frames (1 payload byte each).
    let mut prefix = b"ID3\x03\x00\x00".to_vec();
    prefix.extend_from_slice(&[0x7F, 0x7F, 0x7F, 0x7F]);
    let mut frame = b"TXXX".to_vec();
    frame.extend_from_slice(&1u32.to_be_bytes());
    frame.extend_from_slice(&[0, 0, b'a']);
    let vs = VirtualStream { prefix, pattern: frame, len: 300 * 1024 * 1024, pos: 0 };
    let mut r = CountingReader::new(vs);
    let start = Instant::now();
    assert!(crate::media::extract_embedded_cover(&mut r).is_none());
    assert!(r.ops < 50_000, "ID3 walk performed {} I/O ops", r.ops);
    assert!(start.elapsed() < Duration::from_secs(10));
}

#[test]
fn flac_block_flood_does_not_amplify_io() {
    // `fLaC` followed by endless empty, non-final metadata blocks.
    let vs = VirtualStream {
        prefix: b"fLaC".to_vec(),
        pattern: vec![0x01, 0x00, 0x00, 0x00],
        len: 400 * 1024 * 1024,
        pos: 0,
    };
    let mut r = CountingReader::new(vs);
    assert!(crate::media::extract_embedded_cover(&mut r).is_none());
    assert!(r.ops < 20_000, "FLAC walk performed {} I/O ops", r.ops);
}

#[test]
fn mp4_covr_child_flood_is_bounded() {
    // `covr` atom whose body is an endless run of 8-byte non-`data` atoms.
    let mut prefix = Vec::new();
    prefix.extend_from_slice(&16u32.to_be_bytes());
    prefix.extend_from_slice(b"ftypisom\x00\x00\x00\x00");
    prefix.extend_from_slice(&0u32.to_be_bytes()); // size 0 => extends to EOF
    prefix.extend_from_slice(b"covr");
    let mut atom = 8u32.to_be_bytes().to_vec();
    atom.extend_from_slice(b"free");
    let vs = VirtualStream { prefix, pattern: atom, len: 200 * 1024 * 1024, pos: 0 };
    let mut r = CountingReader::new(vs);
    assert!(crate::media::extract_embedded_cover(&mut r).is_none());
    assert!(r.ops < 20_000, "MP4 walk performed {} I/O ops", r.ops);
}

#[test]
fn ebook_cover_dimension_bomb_is_rejected_with_strict_limits() {
    // EPUB whose cover PNG header claims 60000 x 60000 pixels.
    let mut bomb = png_bytes(2, 2);
    bomb[16..20].copy_from_slice(&60_000u32.to_be_bytes());
    bomb[20..24].copy_from_slice(&60_000u32.to_be_bytes());
    let epub = zip_of(&[("mimetype", b"application/epub+zip"), ("cover.png", &bomb)]);
    let start = Instant::now();
    let res = crate::ebook::extract_ebook_cover_image(
        &mut Cursor::new(epub),
        ".epub",
        256,
        &CoverOptions::default(),
    );
    assert!(res.is_err());
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(crate::image_dec::decode_bytes_strict(&bomb).is_err());
}

// ---------------------------------------------------------------------------
// Property tests for trust-boundary helpers

#[test]
fn archive_path_sanitizer_invariants_hold_for_generated_names() {
    const ATOMS: &[&str] = &[
        "..", ".", "/", "\\", ":", "C:", "\\\\?\\", "//", "CON", "nul", "Com1", "LPT9", " ", "...",
        "\u{202E}", "\u{200B}", "\u{FEFF}", "\u{2066}", "a", "b.png", "%2e%2e", "\0", "\t", "~",
        "$DATA", "::", ".git", "x.jpg.", "dir", "\u{FF0E}\u{FF0E}", "CON .png", "com\u{00B9}", "CONOUT$",
    ];
    let mut rng = Rng(0x5EED_1234_ABCD_0001);
    for _ in 0..40_000 {
        let n = 1 + rng.below(8);
        let mut name = String::new();
        for _ in 0..n {
            name.push_str(ATOMS[rng.below(ATOMS.len())]);
        }
        let Some(out) = sanitize_archive_path(&name) else {
            continue;
        };
        assert!(!out.starts_with('/'), "absolute path survived: {name:?} -> {out:?}");
        assert!(!out.contains('\\') && !out.contains(':'), "separator/ADS survived: {name:?}");
        assert!(!out.chars().any(|c| c.is_control() && c != '\t'), "control survived: {name:?}");
        assert!(
            !out.chars().any(|c| matches!(
                c,
                '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
            )),
            "BiDi/zero-width survived: {name:?}"
        );
        for seg in out.split('/') {
            assert!(!seg.is_empty() && seg != "." && seg != "..", "bad segment in {out:?}");
            assert!(!seg.ends_with('.') && !seg.ends_with(' '), "trailing dot/space in {out:?}");
            let stem = seg.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
            assert!(
                !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                    && !(stem.len() == 4
                        && (stem.starts_with("COM") || stem.starts_with("LPT"))
                        && stem.as_bytes()[3].is_ascii_digit()),
                "device name survived: {out:?}"
            );
        }
    }
}

#[test]
fn trusted_local_path_gate_rejects_every_non_local_form() {
    use std::path::Path;
    for bad in [
        "clip.mp4",
        r"..\clip.mp4",
        r"sub\clip.mp4",
        r"\clip.mp4",
        r"\\server\share\clip.mp4",
        r"\\?\C:\clip.mp4",
        r"\\.\PhysicalDrive0",
        "//server/share/clip.mp4",
        r"C:clip.mp4",
        r"C:\dir\clip.mp4:hidden",
        r"C:\dir\..\..\clip.mp4",
        "C:\\dir\\cl\u{0007}ip.mp4",
        "",
    ] {
        assert!(
            !crate::media::is_trusted_local_abs_path(Path::new(bad)),
            "accepted untrusted path {bad:?}"
        );
    }
    assert!(crate::media::is_trusted_local_abs_path(Path::new(r"D:\media\clip.mp4")));
    // Name-only fallback resolution is disabled when the stream size is unknown.
    assert!(crate::media::resolve_file_from_name_with_source("clip.mp4", 0).is_none());
}

#[test]
fn ffmpeg_invocation_is_confined() {
    let args = crate::media::video_mf::FFMPEG_INPUT_HARDENING_ARGS;
    let pos = |flag: &str| args.iter().position(|a| *a == flag);
    let proto = pos("-protocol_whitelist").map(|i| args[i + 1]).expect("protocol whitelist");
    assert_eq!(proto, "file", "only the local file protocol may be allowed");
    let fmts = pos("-format_whitelist").map(|i| args[i + 1]).expect("format whitelist");
    for banned in ["hls", "dash", "concat", "image2", "tty", "lavfi", "data", "subfile", "http"] {
        assert!(
            !fmts.split(',').any(|f| f == banned),
            "meta-demuxer {banned} must not be whitelisted"
        );
    }
    let codecs = pos("-codec_whitelist").map(|i| args[i + 1]).expect("codec whitelist");
    for banned in ["magicyuv", "indeo3", "indeo4", "indeo5", "cinepak", "smackvid", "bink"] {
        assert!(!codecs.split(',').any(|c| c == banned), "legacy decoder {banned} must not be whitelisted");
    }
    let arg = crate::media::video_mf::safe_ffmpeg_input_arg(std::path::Path::new(r"D:\v\-i.mp4"));
    assert!(arg.starts_with("file:"), "input must use explicit file: protocol, got {arg}");
}

// ---------------------------------------------------------------------------
// Repository privacy scan (sources, configs, tests, examples, docs)

#[test]
fn repository_contains_no_build_machine_paths_or_personal_data() {
    use std::path::{Path, PathBuf};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf();

    // Needles are assembled at run time so this file does not match itself.
    let sep = '\\';
    let users_win = format!(":{sep}{}{sep}", "Users");
    let users_win_fwd = format!(":/{}/", "Users");
    let home_unix = format!("/{}/", "home");
    let users_mac = format!("/{}/", "Users");
    let onedrive = format!("{}{}", "One", "Drive -");
    let mut needles = vec![users_win, users_win_fwd, home_unix, users_mac, onedrive];
    // The developer's own environment must not appear either.
    for var in ["USERNAME", "USER"] {
        if let Ok(u) = std::env::var(var) {
            if u.len() >= 4 {
                needles.push(format!("{sep}{u}{sep}"));
                needles.push(format!("/{u}/"));
            }
        }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if profile.len() > 3 {
            needles.push(profile);
        }
    }
    let root_str = root.to_string_lossy().to_string();
    if root_str.len() > 3 {
        needles.push(root_str);
    }

    const TEXT_EXT: &[&str] = &[
        "rs", "toml", "md", "ps1", "yml", "yaml", "json", "def", "manifest", "txt", "gitignore",
    ];
    let allowed_email_domains = ["example.com", "example.org", "example.invalid", "users.noreply.github.com"];

    // Mirror what would be committed: skip root-anchored files (`/name`) and simple `*.ext`
    // patterns from the workspace `.gitignore` (local maintenance helpers, logs, ...).
    let gitignore = std::fs::read_to_string(root.join(".gitignore")).unwrap_or_default();
    let ignored_root_files: Vec<String> = gitignore
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('/') && !l.ends_with('/'))
        .map(|l| l.trim_start_matches('/').to_ascii_lowercase())
        .collect();
    let ignored_exts: Vec<String> = gitignore
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("*."))
        .map(str::to_ascii_lowercase)
        .collect();

    let mut stack: Vec<PathBuf> = vec![root.clone()];
    let mut findings = Vec::new();
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if !matches!(name.as_str(), "target" | ".git" | "dist" | "images" | "node_modules") {
                    stack.push(p);
                }
                continue;
            }
            let ext = p
                .extension()
                .map(|x| x.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_else(|| name.trim_start_matches('.').to_ascii_lowercase());
            if !TEXT_EXT.contains(&ext.as_str()) || ignored_exts.contains(&ext) {
                continue;
            }
            if p.parent() == Some(root.as_path()) && ignored_root_files.contains(&name.to_ascii_lowercase()) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&p) else { continue };
            let rel = p.strip_prefix(&root).unwrap_or(&p).display().to_string();
            for n in &needles {
                if text.to_ascii_lowercase().contains(&n.to_ascii_lowercase()) {
                    findings.push(format!("{rel}: build-machine / user path pattern"));
                }
            }
            // E-mail addresses (very small hand-rolled matcher).
            for (i, _) in text.match_indices('@') {
                let before = text[..i]
                    .chars()
                    .rev()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
                    .count();
                let after: String = text[i + 1..]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
                    .collect();
                let after = after.trim_end_matches('.');
                if before >= 1
                    && after.contains('.')
                    && after.rsplit('.').next().map_or(false, |tld| tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()))
                    && !allowed_email_domains.iter().any(|d| after.eq_ignore_ascii_case(d) || after.ends_with(&format!(".{d}")))
                {
                    findings.push(format!("{rel}: e-mail address ({after})"));
                }
            }
        }
    }
    findings.sort();
    findings.dedup();
    assert!(findings.is_empty(), "privacy scan findings:\n{}", findings.join("\n"));
}
