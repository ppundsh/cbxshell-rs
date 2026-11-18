# Zero-Trust Input Validation & Safeguards

CBX Shell applies Zero-Trust input handling: all external archive packages, media streams, and local configuration files are treated as untrusted and potentially corrupted or out-of-spec.

---

## 1. Archive, Document & Image Bomb Protections

Decompression and decoding workflows enforce strict memory, dimension, and ratio boundaries against high-ratio bombs, forged headers, and parser OOMs:
- **Maximum Image Size Budget**: Single candidate cover extractions are bounded by user configuration (default 256 MiB, configurable between 8 and 2047 MiB) via chunked streaming reads without trusting header-declared allocation sizes.
- **Compression Ratio Cap (Pre- and Post-Extraction)**: Items exceeding 8 MiB with a ratio above 200:1—or a forged 0-byte compressed size—are rejected before extraction, and the actual decompressed byte length is re-verified against the 200:1 cap after extraction to defeat forged-header bombs.
- **RAR Read Cap**: RAR block headers are walked with seeks (each header CRC-verified, ≤ 2 MiB per header) and only the cover's block (or its solid group up to the cover) is read, capped at 16–96 MiB; the bounded prefix read is used only as a fallback.
- **Solid Block Cumulative Drain Cap**: For solid 7z and RAR archives, cumulative decompressed stream draining is capped at 128 MiB with wall-clock deadline checks to limit CPU and disk usage.
- **Image Dimension, Allocation & Downscaling Defense**: Decoders enforce `image::Limits` (`MAX_IMAGE_DIMENSION = 16384`, `MAX_SOURCE_PIXELS = 64,000,000`, `MAX_DECODER_ALLOC_BYTES = 256 MiB`). Images are resampled (`CatmullRom`) to thumbnail dimensions, neutralizing LSB steganography and scaling-attack payloads, and polyglot trailer payloads (`detect_image_trailer_overhang`) are flagged with `RISK`.
- **Document & Audio Cover Parser Hardening**:
  - **OOXML / PSD / Audio Covers**: OOXML embedded thumbnails are bounded to 16 MiB with `image::Limits`; PSD/PSB composite decoding enforces checked arithmetic and channel/dimension/table caps; MP4 `covr/data` atom loops reject `size < 8`; FLAC `METADATA_BLOCK_PICTURE` skips variable-length fields via bounded streaming instead of allocating from untrusted `u32` lengths.
  - **Plain Text & Syntax Rendering**: Reads are capped at 16 KiB and rendered to at most 24 lines × 80 chars, stripping C0/C1 control characters (except `\t`, `\r`, `\n`) and Unicode BiDi/zero-width overrides before GDI shaping. Rendered text also passes through credential redaction (see §7).
- **Strict Limits Everywhere**: Every untrusted image goes through `image_dec::decode_bytes_strict` (dimension, pixel-count and allocation limits) — including EPUB/MOBI/IBA covers, PSD thumbnail resources and the PNG produced by the PDF renderer, which previously used `image::load_from_memory` with the library's permissive defaults.
- **Container Pre-Validation**: ZIP-based formats (CBZ, EPUB, IBA, DOCX, XLSX) must pass the bounded End-of-Central-Directory tail check before the ZIP parser runs. 7z archives must pass a signature-header check (`NextHeaderSize` ≤ 16 MiB and `NextHeaderOffset + size` inside the file) so a forged header size cannot become an allocation bomb that aborts the host process.
- **Loop / Amplification Caps**: Length-driven walks are bounded independently of the deadline — ID3 frames (4,096), FLAC metadata blocks (1,024), MP4 `covr` children (64) and bencode nodes (200,000 per `.torrent`, depth 64, 20-digit integers). These caps matter because the watchdog only *abandons* a stuck worker thread; it cannot stop it.

---

## 2. Security Risk Badges & Granular Rules

Archives containing suspicious structures or disguised payloads can be flagged with a visible `RISK` badge. Each rule category can be toggled independently (applied consistently across ZIP, 7z, RAR, and TAR):

| Rule Identifier | Detection Target & Behavior |
| :--- | :--- |
| **Disguised Executables & Scripts** | Archives containing `.exe`, `.dll`, `.bat`, `.cmd`, `.ps1`, `.vbs`, `.vbe`, `.js`, `.jse`, `.scr`, `.msi`, `.lnk`, `.url`, etc. |
| **Path Traversal & Absolute Paths** | Entry names containing `..`, absolute drive paths (`C:\`), or NTFS Alternate Data Streams (`:stream`). |
| **Hidden Files & Dot Directories** | Items prefixed with `.` (`.git/`, `.env`) or macOS resource forks (`__MACOSX/`). |
| **Plain Text & Documentation** | Encapsulated text documents such as `.txt`, `.nfo`, `.diz`. |
| **Sensitive Certificates & Keys** | Security credentials including `.key`, `.pem`, `.pfx`, `.crt`. |
| **Evasion Techniques (RTLO, Zero-Width & Reserved Words)** | Unicode BiDi overrides (`U+202A..U+202E`, `U+2066..U+2069`), invisible zero-width format characters (`U+200B..U+200F`, `U+2060..U+2064`, `U+FEFF`), trailing spaces or dots, and Windows reserved device names (`CON`, `PRN`, `AUX`, `NUL`, `CONIN$`, `CONOUT$`, `COM0-9`, `LPT0-9`, the superscript forms `COM¹-³` / `LPT¹-³`, and stems with trailing spaces such as `CON .png`). |

---

## 3. Configuration Defense & Defensive Sanitization

- **Registry Tampering Defense**:
  If registry keys are corrupted or manually tampered with out-of-range values, `Settings::sanitize()` automatically validates and rectifies all settings upon load:
  - All numerical parameters are clamped strictly within safe boundaries.
  - If luminance thresholds are inverted (`lower >= upper`), they are restored to recommended defaults.
  - `custom_ffmpeg_path` is sanitized against control characters, shell metacharacters (`"`, `'`, `|`, `&`, `;`, `<`, `>`, `` ` ``, `$`, `^`, `%`), UNC/device paths (`\\`), NTFS ADS colons, relative paths, and path length limits (260 chars). The file must be an absolute path that physically exists and ends with `.exe`.
- **User Interface Input Hardening**:
  Numeric GUI inputs accept only ASCII digits (`0`..=`9`, at most 4). The "Custom Plain Text Extensions" field accepts only letters, digits, `,`, `.`, `_`, `-` and spaces (at most 64 characters), and `Settings::sanitize()` reduces it on save to alphanumeric extensions of at most 16 characters each.

---

## 4. Media Decoding Security & Mitigation of Decoder CVEs (e.g., CVE-2026-8461)

### Decoder Boundary Context
C/C++ multimedia decoding engines (such as FFmpeg `libavcodec`) must parse untrusted bitstreams of high complexity. Vulnerabilities such as **CVE-2026-8461** (out-of-bounds write in `libavcodec/magicyuv.c` before FFmpeg 8.1.2) illustrate the risk: corrupted or out-of-spec container frames can trigger memory corruption, leading to denial of service or potential remote code execution (RCE).

### Architectural Boundaries
CBX Shell reduces the blast radius and exposure of multimedia decoders through layered process boundaries:

1. **Out-of-Process Isolation (COM Surrogate / Subprocess Boundary)**:
   - In default Isolation Mode, third-party decoding libraries are not loaded into the address space of `explorer.exe`.
   - Windows Media Foundation pipeline runs inside the Windows COM surrogate process (`dllhost.exe`, "COM Surrogate") in Isolation Mode.
   - The FFmpeg fallback runs as an ephemeral child subprocess with `CREATE_NO_WINDOW`. If an unhandled memory corruption, out-of-bounds write, or crash occurs inside `libavcodec`, only the short-lived child process terminates with an exit error code, leaving the shell process and desktop environment isolated.

2. **Protocol, Demuxer & Decoder Allowlisting** (`FFMPEG_INPUT_HARDENING_ARGS`):
   - `-protocol_whitelist file`: only the local file protocol. Network protocols (HTTP, HTTPS, TCP, UDP, RTP), `pipe:`/`fd:`, and URL tricks such as `concat:` / `subfile:` are refused.
   - The input is passed as an explicit `file:<absolute path>` URL, so a crafted file name can never be parsed as an option (`-…`) or as another protocol.
   - `-format_whitelist` restricts probing to ordinary video containers (MOV/MP4, Matroska/WebM, AVI, ASF, FLV, MPEG-TS/PS, Ogg, RM, raw H.264/HEVC, …). This defeats **content-type confusion**: a file named `.mp4`/`.avi` that actually contains an HLS/DASH playlist, an `ffconcat` script or an `image2` pattern can no longer make FFmpeg open *other* local files and paint their bytes into the thumbnail (a known local-file-disclosure chain).
   - `-codec_whitelist` permits only mainstream video decoders. A file cannot force FFmpeg to instantiate rare/legacy decoders such as MagicYUV (CVE-2026-8461), Indeo or Cinepak — the decoder class where most memory-safety bugs are found. Extend the list in `video_mf.rs` if a legitimate codec is needed.
   - Frame extraction adds `-an -sn -dn`, so audio, subtitle and data decoders are never opened.

3. **Demuxing & Memory Budgeting**:
   - Stream probing is capped at `-probesize 1048576` (1 MiB) and `-analyzeduration 1000000` (1 s).
   - Limits memory consumption during oversized container header parsing.

4. **Watchdog Timeouts & Bounded Pipes**:
   - A dedicated watchdog timer (configurable between 3 s and 60 s, default 12 s) monitors both COM worker execution and each FFmpeg subprocess invocation. On timeout, the parent terminates the child process (`kill` + `wait`) and reaps pipes; stdout/stderr reads are strictly capped (16 MiB stdout, 512 KiB stderr) and decoded with `image::Limits`.

5. **Codec Surface Reduction**:
   - Thumbnail extraction only requires standard consumer codecs (H.264, HEVC, AV1, VP9, MPEG-4). Unusual, legacy, or experimental codecs (such as MagicYUV, Indeo, Cinepak) can be stripped or disabled in custom FFmpeg builds without impacting standard thumbnail extraction.

---

## 5. Least-Privilege Execution & External Tool Anti-Spoofing

1. **FFmpeg Subprocess Least-Privilege Sandbox**:
   - **Start Suspended, Confine, Then Resume**: The child is created with `CREATE_SUSPENDED`, assigned to the Job Object and stripped of privileges, and only then resumed. Previously the job was applied *after* the process had already started executing (a race window in which a decoder exploit could spawn processes). If the job cannot be created or assigned, the child is killed — FFmpeg never runs unsandboxed (fail closed).
   - **OS Kernel Job Object**: Every `ffmpeg.exe` child is bound to an anonymous Win32 Job Object enforcing `JOB_OBJECT_LIMIT_ACTIVE_PROCESS = 1` (prevents spawning child shells/processes), `JOB_OBJECT_LIMIT_PROCESS_MEMORY = 512 MiB` (kernel-enforced memory ceiling), `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, and `JOBOBJECT_BASIC_UI_RESTRICTIONS` (blocks clipboard, desktop, and cross-window handle access).
   - **Token Privilege Stripping**: All optional token privileges are disabled on the child process (`AdjustTokenPrivileges` with `DisableAllPrivileges = true`).
   - **Environment & CWD Isolation**: `cmd.env_clear()` strips inherited variables, passing only `SystemRoot`, `WINDIR`, and a minimal `PATH` (`<ffmpeg_dir>;System32`), with `current_dir` locked to `<ffmpeg_dir>` to block DLL planting from the browsed media folder.
   - **Binary Discovery & Argument Safety**: `PATH` discovery skips relative directories (`.`), UNC paths, `Temp`, and `Downloads`; executing an `ffmpeg.exe` located in the same folder as the target media file is blocked; input paths are canonicalized to absolute paths and prefixed with `file:` to prevent option or protocol injection.

2. **Everything IPC Anti-Spoofing & Path Verification**:
   - **IPC Owner Verification**: Before querying `EVERYTHING_TASKBAR_NOTIFICATION`, the owning process of the IPC window is inspected via `GetWindowThreadProcessId` and `QueryFullProcessImageNameW` to verify it is an actual `Everything.exe` / `Everything64.exe` running from a local non-temporary path.
   - **Result Validation**: Returned paths must reside on `DRIVE_FIXED` or `DRIVE_REMOVABLE` by default (blocking UNC/SMB NTLM leaks; when the user explicitly allows network shares, the §6 network policy decides instead), contain no `..` traversal or ADS colons, match the queried filename, and match the exact stream byte size (`expected_size`). Search strings reject quotes and control characters.

3. **Explicit System Utility Paths**:
   - Cache cleanup invokes `taskkill.exe` and `explorer.exe` by absolute path under the Windows directory reported by `GetSystemWindowsDirectoryW` (never a hard-coded drive letter) with a locked system `current_dir`, avoiding `cmd.exe` or `PATH` search-order hijacking.

---

## 6. Shell-Supplied Names Are Untrusted (Confused-Deputy Defense)

In Isolation Mode the Shell hands the extension an `IStream`; the accompanying `STATSTG::pwcsName` is frequently a *bare file name* (or a name chosen by a third-party namespace extension, e.g. a ZIP folder or cloud provider). Earlier builds called `Path::is_file()` on that name, which resolves it against the host process's **current directory**, so the thumbnail of one item could be generated from an unrelated local file with the same name.

- A name is opened directly only if `media::is_openable_media_path` accepts it: by default an absolute drive-letter path on a **non-network drive** (`is_trusted_local_abs_path`) with no UNC/device prefix (`\\server`, `\\?\`, `\\.\`), no ADS colon, no control characters and no `..` component. Otherwise it is used purely as an extension hint and the bytes come from the stream.
- **Network shares are opt-in** (Control Center → Video → "Allow reading network shares", off by default, with a warning when enabled): only then are UNC paths, mapped network drives and Everything hits on network locations opened directly. The "Local network only" sub-option (on by default) accepts only servers with private / link-local / loopback IPs, single-label (NetBIOS / LLMNR) names and LAN suffixes such as `.local`, `.lan`, `.home.arpa`; classification performs **no DNS lookups**, and WebDAV (`\\host@SSL\…`) and device namespaces are always rejected. Mapped drives are resolved to their server with `QueryDosDeviceW` and checked by the same rule.
- When Media Foundation cannot decode a stream (FLV, many MKV, Hi10P …), the stream head and the bytes around each target position are fed to FFmpeg through **stdin** (`-protocol_whitelist pipe`, every other allowlist and the sandbox unchanged); the host process never opens the file or a network connection itself.
- Name-based fallback resolution for videos (cache → Everything → CWD → TypedPaths → user folders → Recent → drive roots) is **disabled when the stream size is unknown (0)**, and every candidate must match the stream size exactly.
- `TypedPaths` entries, `.lnk` targets and the last-directory cache are only followed on local fixed/removable drives, so address-bar history or a shortcut can never trigger an outbound SMB/WebDAV connection (NTLM hash leak) or a network stall.
- `IStream::Read` results from third-party streams are validated (`read_bytes ≤ requested`), `BufferedIStream::Seek` uses checked arithmetic, and the requested thumbnail edge is clamped to 16–2560 px.

---

## 7. Privacy Protections

Thumbnails are **persistent**: Explorer stores them in `thumbcache_*.db`, where they survive deletion of the original file and are readable by any process running as the user.

- **Credential redaction in previews** (`cbx_core::redact`): text, Word and Excel previews mask PEM/OpenSSH/PGP private-key blocks, `key = value` pairs whose key names a credential (`password`, `secret`, `token`, `api_key`, `connection_string`, …, matched on whole words so `author` or `tokenizer` are untouched), passwords embedded in URLs (`scheme://user:•••@host`) and well-known token shapes (cloud access keys, VCS/chat tokens, JWTs, long mixed-case high-entropy strings).
- **Display-spoofing removal**: torrent file listings, document lines and spreadsheet cells strip BiDi overrides, zero-width/invisible characters and control characters, so `photo<U+202E>gpj.exe` cannot be shown as `photoexe.jpg`.
- **Live Monitor history is memory-only**: the shared section is backed by the paging file instead of a file in `%TEMP%`, so the list of thumbnailed paths is never written to disk and disappears when the last process holding it exits. The monitor remains disabled by default.
- **No build-machine data in binaries**: `build.ps1` remaps `USERPROFILE`, `CARGO_HOME`, `RUSTUP_HOME` and the repository root with `--remap-path-prefix`, the MSVC linker records only the PDB file name (`/PDBALTPATH:%_PDB%`), and a **privacy gate** fails the build if any release binary still contains the builder's profile path, user name or repository path (ASCII or UTF-16).
- **No personal data in the repository**: no hard-coded drive letters or user folders (system locations come from `GetSystemWindowsDirectoryW`, `%ProgramFiles%`, `%LOCALAPPDATA%`), the local time zone comes from the OS instead of a fixed offset, and the `repository_contains_no_build_machine_paths_or_personal_data` test scans sources, configuration, tests and documentation for user-profile paths and e-mail addresses.

---

## 8. Executable & Supply-Chain Hardening

- **DLL planting**: `CBXShell-Setup.exe`, `cbx-manager.exe` and `cbx-cli.exe` are linked with `/DEPENDENTLOADFLAG:0x800` (static imports resolve from System32 only) and call `SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32)` on start-up, so a `version.dll`/`dwmapi.dll` dropped next to an installer in *Downloads* is never loaded. (The GNU toolchain has no equivalent of the linker flag; the run-time call still applies.)
- **Auto-update**: a downloaded installer runs only after its minisign (Ed25519) signature verifies against the built-in public key; manifest URLs are restricted to this repository's release assets and only newer versions are accepted; a maintainer approval of version + SHA-256 signed with a YubiKey (FIDO2, PIN + touch) is also required, so a compromised GitHub account, CI or signing key alone cannot ship an update. See [Installation, Version Check & Auto-Update](./updates.md).
- **Uninstaller relaunch**: the self-copy used to unlock the install folder is placed in a freshly created, uniquely named sub-directory of `%TEMP%` instead of a fixed, pre-plantable path.
- **CI**: the release workflow defaults to `contents: read`, grants `contents: write` only to the release job and checks out without persisting credentials. Pinning third-party actions to full commit SHAs is recommended.

---

## 9. Adversarial Testing Strategy (Unknown-Attack Detection)

`crates/cbx-core/src/adversarial_tests.rs` complements fixed regression tests with oracles that detect whole *classes* of new bugs:

| Oracle | Detects |
| :--- | :--- |
| No panic (`catch_unwind`) | Out-of-bounds slicing, overflow, `unwrap` on hostile data |
| Per-case wall-clock budget | Infinite loops, algorithmic blow-ups, work that outlives the watchdog |
| I/O amplification budget (instrumented `CountingReader`) | Length-field loops that re-seek/re-read millions of times |
| Output-dimension bound | Decoders trusting header dimensions |
| Property tests on trust-boundary helpers | Path traversal / ADS / device-name / BiDi bypasses in `sanitize_archive_path`; non-local paths accepted by `is_trusted_local_abs_path` |
| Configuration assertions | FFmpeg protocol/demuxer/decoder allowlists, `file:` input prefix |
| Repository privacy scan | Build-machine paths, user names, e-mail addresses |

Inputs come from **structure-aware mutation** of a seed corpus covering every parser (ZIP/CBZ, EPUB, DOCX, XLSX, TAR, 7z, RAR, PSD, ID3, FLAC, MP4, MOBI, torrent, text, PDF): *lying length* (boundary integers written over length fields, BE/LE, 16/32/64-bit), *chunk amplification*, *splicing between formats* (content-type confusion), truncation and byte flips. Each mutant is also dispatched under a different extension. Multi-hundred-MiB hostile files are modelled with a virtual stream instead of real allocations.

Run: `cargo test -p cbx-core` (Windows). For longer campaigns: `$env:CBX_FUZZ_ITERS=20000; cargo test -p cbx-core --release structure_aware`. The PRNG is deterministic; a failure message names the iteration and extension needed to reproduce it.

---

## 10. Residual Risks & Assumptions

- **OS decoders**: Media Foundation, `Windows.Data.Pdf` and the WIC/GDI stack are Windows components; CBX Shell bounds their inputs and outputs but relies on Windows Update for their memory safety. Keep Isolation Mode enabled.
- **Abandoned workers**: on timeout the watchdog returns to Explorer but cannot terminate the worker thread; per-parser caps (§1) bound how long such a thread can run.
- **7z encoded headers**: the signature-header check bounds the header *block*, but the size a compressed header claims to unpack to is still trusted by `sevenz-rust2`.
- **User-writable install folder**: the default per-user install lives under `%LOCALAPPDATA%`; any process running as the same user can modify it (inherent to per-user installs). Elevating `cbx-manager.exe` from that folder therefore trusts that user context.
- **Bundled FFmpeg**: when "install FFmpeg" is selected, the installer copies the `ffmpeg.exe` located next to it. Only place a trusted FFmpeg build there, and prefer FFmpeg ≥ 8.1.2 (CVE-2026-8461).
- **Same-session processes**: the Live Monitor shared section (`Local\…`) and the Everything IPC window are reachable by other processes in the same logon session; their data is treated as untrusted and size-bounded.

