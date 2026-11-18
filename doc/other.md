# Documents, Design Files, Syntax Previews & Companion Tools

CBX Shell extends thumbnail generation beyond archives and media to Office documents, Photoshop files, plain-text/code previews, and includes a Windows 11 companion suite.

---

## 1. Documents & Design Files ("Other" Category)

- **Office Documents (`.docx`, `.doc`, `.xlsx`, `.xls`)**:
  - `.docx` / `.xlsx`: Extracts embedded OOXML preview thumbnails or renders structured document/spreadsheet summary cards.
  - `.doc`: Extracts readable text runs from the first 256 KiB as a summary card (not a full OLE2 parse).
  - `.xls`: Renders an Excel-styled card without parsing the content.
- **Adobe Photoshop (`.psd`)**:
  - Reads composite image data or embedded thumbnail resources directly from PSD headers without requiring Photoshop to be installed.
- **Plain Text & Markdown (`.txt`, `.md`)**:
  - Renders readable high-DPI text thumbnails (first 16 KiB, up to 24 lines).
  - Supports custom user-defined text extensions (empty by default; configurable in Control Center under Custom Plain Text Extensions).
- **BitTorrent Metadata (`.torrent`)**:
  - Parses Bencode `info` metadata to render the torrent's folder/file tree with typed mini icons, and displays the default torrent client icon in the bottom-right corner. File names are stripped of BiDi-override and zero-width spoofing characters first.
- **Preview Privacy Masking**: Plain-text, syntax, Word and Excel previews mask private-key blocks, password/token `key = value` settings, passwords embedded in URLs and common token formats before drawing, so secrets are never written into the Windows thumbnail cache (see [`security.md` §7](./security.md)).

---

## 2. Syntax-Highlighted Code & Config Previews

CBX Shell supports syntax-colorized thumbnail previews for 20 common developer and configuration formats (disabled by default; can be enabled individually or in batch via Control Center):

- **Data & Markup**: `.json`, `.csv`, `.html`, `.htm`, `.xml`, `.yaml`, `.yml`, `.toml`, `.ini`, `.log`
- **Scripts & Code**: `.ps1`, `.cmd`, `.bat`, `.sh`, `.sql`, `.py`, `.js`, `.tsx`, `.css`, `.rs`

When enabled, keywords, strings, comments, numbers, and punctuation are colorized on the preview card.

---

## 3. Border Decoration Styles

Thumbnails across all categories can be customized with 6 border styles:
1. **No Frame**: Raw thumbnail image without outer frame.
2. **Round Only (Borderless Rounded)**: Rounded corners only, without any outer frame or border ring.
3. **Filmstrip**: Perforated filmstrip border with rounded corners.
4. **Drop Shadow**: Soft elevation shadow with rounded corners.
5. **Photo Frame**: Clean frame border with subtle gradient highlight and shadow.
6. **Book Cover**: Spine crease and cover depth effect for e-books and documents.

---

## 4. Companion Applications & CLI

- **Control Center (`cbx-manager.exe`)**:
  - Follows Windows system Dark/Light mode and accent color settings automatically.
  - The navigation rail uses the Windows 11 Acrylic (frosted glass) backdrop — the system backdrop material on Windows 11 22H2+, a compatible blur on older builds — and falls back to an opaque base color when Transparency effects are off or unsupported.
  - Per-category extension association toggles, collapsible sub-cards, border style previews, "Drop Zone: Probe & Pre-generate" card (drop a file to probe and preview it, with plain-text fallback inspection; drop a folder to pre-generate its thumbnails), and a live activity monitor.
  - **Pre-generate thumbnails** (Advanced › System Repair & Cache): drop a folder on the Advanced page, or click "📁 Pre-generate" and pick one. Files associated with CBX Shell in that folder and up to 3 levels of sub-folders are sent through the system thumbnail cache (`IThumbnailCache::GetThumbnail`) from several worker threads (half the logical CPUs, 2–8), which stores the results in Windows' `thumbcache_*.db`. When Explorer later opens those folders it serves the thumbnails from the cache without calling CBX Shell again. Sizes 96 / 256 / 768 px match the medium / large / extra-large icon views; "Force Refresh" regenerates items already cached; network folders need "Include Network Devices". Hidden and system entries and junctions / symlinks are skipped; one run is capped at 200,000 files. Progress shows done / total, new / cached / failed, elapsed time and an estimated time left (from the average time per file so far). When a run ends with failures, a "↻ Retry N failed" button appears in the title row; it regenerates just those files, forced and with half the workers. Disk Cleanup or Storage Sense clearing "Thumbnails" also removes pre-generated entries.
  - **Activity logging** is off by default (also after a fresh install). Unless the "🔒 lock" next to the switch is engaged, logging stops on its own 10 minutes after being turned on (the button shows the minutes left; the extension enforces the same deadline, so it expires even with the manager closed). Only a locked log keeps recording; switching logging off also releases the lock. Auto-stop keeps what was recorded so it can still be exported. The manager accumulates the latest 1,000 entries (the shared memory keeps only 64), and filtering, scrolling and export use that log, so a burst of new entries no longer pushes filtered results out; only the latest 200 successful entries are kept, the rest of the room holds failures, and a file that later succeeds loses its earlier failure entries. The monitor's "↻ Redo N" regenerates the failed files from that log that still exist (full paths only; activity logging must be on).
- **Setup Wizard (released as `CBXShell-Setup.exe`, crate `cbx-installer`)**:
  - Follows Windows system Dark/Light mode with component selection, registry/shell association management, icon cache refresh, and automatic FFmpeg / Everything detection with official download links.
- **Command-Line Tool (`cbx-cli.exe`)**:
  - Run `cbx-cli.exe --help` for full usage, including `--selftest`, `--adversarial`, `--test-com <file>`, `--dump-icon <out.ico>`, `--apply-associations`, and single-file thumbnail inspection/export.
