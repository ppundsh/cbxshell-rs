# E-Book Cover Extraction Support

CBX Shell extracts embedded cover artwork from major e-book formats directly from file streams without requiring external reader software.

---

## 1. Supported E-Book Formats

- **EPUB / IBA (`.epub`, `.iba`)**:
  - Tries, in order:
    1. iBooks / QuickLook preview images (e.g. `QuickLook/Thumbnail.png`, `iBooksArtwork`);
    2. `META-INF/container.xml` → OPF, resolving `properties="cover-image"` (EPUB 3), then `<meta name="cover">` (EPUB 2), then image items whose id/href contains `cover`;
    3. images named `cover`, `front` or `thumbnail`;
    4. the first image chosen by natural sort and continuous-sequence rules.
  - Cover images are always decoded under strict dimension and allocation limits.
- **MOBI / AZW (`.mobi`, `.azw`)**:
  - Reads PalmDB record tables and MOBI/EXTH header metadata (`EXTH` type `201` cover offset / `202` thumbnail offset); each record is capped at `MAX_MOBI_SCAN_BYTES = 64 MiB`, and at most 200 image records are scanned as a fallback. ZIP-packaged files are handled by the EPUB path.
- **PDF (`.pdf`)**:
  - Optional support (disabled by default; can be enabled in the Control Center under E-Books). The first page is rendered by the built-in `Windows.Data.Pdf` API, with a file read limit of `MAX_PDF_MEMORY_BYTES = 128 MiB`.

---

## 2. Presentation Options

- **Book Cover Border**: Applies spine crease and layered page depth styling (or choose any of the 6 border styles, including `No Frame` and `Round Only`).
- **Opening App Badge**: Optionally overlays the associated default reader icon on e-book thumbnails.
