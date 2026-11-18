# Archive & Image Container Support

CBX Shell provides streaming decompression and cover extraction across common comic and image archive containers. (For e-book formats such as EPUB, MOBI, IBA, AZW, and PDF, see [E-Book Support](./ebooks.md).)

---

## 1. Supported Containers

- **ZIP / CBZ**: Standard ZIP (including ZIP64), Deflate, Deflate64 and LZMA. Encrypted entries (including AES) are skipped — no password is available, so no decryption is attempted.
- **RAR / CBR**: RAR4 and RAR5 via a pure-Rust decoder; encrypted headers/entries are skipped and multi-volume entries are not processed.
- **7z / CB7**: LZMA, LZMA2, PPMd, BZip2, Deflate and LZ4 (both solid and non-solid); the signature header is validated before parsing.
- **TAR / CBT**: Standard TAR archives.

---

## 2. Multi-Byte CJK Encoding Auto-Detection

When the UTF-8 bit is missing in legacy ZIP headers (such as older archivers or Japanese DLsite releases), standard tools default to DOS CP437, causing character corruption for Japanese (Shift-JIS / CP932), Traditional Chinese (Big5 / CP950), and Simplified Chinese (GBK / CP936).

To preserve CJK filenames accurately, the engine reads raw filename bytes directly and queries the Windows MultiByte conversion API:
1. Validates strict UTF-8 compliance first.
2. If non-UTF-8, tests CP932 (Shift-JIS), system ANSI code page, CP950 (Big5), and CP936 (GBK) sequentially.
3. Renders CJK characters accurately without Mojibake.

---

## 3. Cover Selection & Image Processing

- **Natural Sorting**: Sorts entries according to standard Windows Explorer logical order (e.g. `1.jpg` preceding `10.jpg`).
- **Continuous Sequence First-File Selection**: When enabled (`prefer_continuous_sequence`, default on), candidate images are grouped by `(folder_depth, folder_name)` and filename digit-skeleton (digits collapsed to `#`, e.g., `page_01.jpg` -> `page_#.jpg`). The engine selects the shallowest folder containing a sequence of 2 or more images, picks the longest continuous sequence, and chooses its first image in natural sort order—preventing standalone notice files (`0000_info.jpg`) or stray zero-padded entries from overriding the primary page sequence.
- **Tall-Strip Auto-Cropping**: Automatically crops vertical webtoon strips (aspect ratio > 3:1) to the top 2:3 region to preserve thumbnail balance.
- **Low-Detail Cover Filtering**: Skips blank disclaimers, monochrome pages, and single-color covers.
- **Border Decoration Styles**: Supports 6 styles (`No Frame`, `Round Only` / borderless rounded corners, `Filmstrip`, `Drop Shadow`, `Photo Frame`, `Book Cover`) configurable per media category.
- **Neutral Archive Icon Fallback**:
  If an archive contains no images, the engine returns `E_FAIL` so Windows Explorer preserves the default system archive icon. The event is recorded objectively as "No Cover Image" (normal completion), not an error.

---

## 4. Partial Reads: Only the Bytes That Are Needed

A thumbnail never needs the whole archive. Each format reads only what is required to *find* the cover and to *decode* it, so network shares, cloud placeholders and large files transfer only a small fraction of their size:

| Format | What is read | Notes |
|---|---|---|
| **ZIP / CBZ** | Tail (≤ 64 KiB) → central directory → cover entry | The central directory sits at the end of the file; once the cover is chosen the reader jumps straight to its local header and data. Other entries' data is never read. |
| **RAR / CBR** | Block headers one by one (data skipped with seeks) → cover entry | Every entry is preceded by a self-describing header; headers are read through a 4 KiB window, their CRC is verified, and the data area is skipped. With natural sorting off the walk stops at the first usable image; with it on, only headers are walked. Then just *signature + main header + the cover's block* are assembled into a small archive for the decoder. A **solid** entry needs every block from the start of its solid group up to the end of its data (solid streams cannot be decoded from the middle). |
| **7z / CB7** | Signature header (32 B) → end header → the cover's data block | Headers sit at the end; blocks are decoded in order, and a solid block must be decoded from its start (bounded by the 128 MiB drain cap and the deadline). |
| **TAR / CBT** | 512-byte headers one by one (data skipped with seeks) → cover entry | The first pass walks headers only and records each candidate's data position; the chosen entry is then read directly. |

When the RAR fast path meets an unusual layout (encrypted headers, a non-standard SFX) or the decoder rejects the minimal archive, it falls back to the previous "read a 16–96 MiB prefix and parse it" path, so compatibility is unchanged.

> Random access is still required: ZIP and 7z keep their directory at the end, so the reader must be able to seek there; RAR and TAR headers are spread across the file. If a file is still downloading or being copied and does not yet contain the cover, thumbnail extraction fails; it succeeds once the file is complete.
