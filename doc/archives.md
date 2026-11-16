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
