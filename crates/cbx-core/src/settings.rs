use windows::core::{GUID, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY,
    HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
};

/// Brand-new CLSID for `cbxshell-rs` (does not collide with legacy C++ CBXShell).
pub const CLSID_CBXSHELL_RS: GUID = GUID::from_u128(0xd84e6b7a_3c91_4f2e_a518_7b9d02e4c6f1);
pub const CLSID_CBXSHELL_RS_STR: &str = "{D84E6B7A-3C91-4F2E-A518-7B9D02E4C6F1}";

/// Legacy C++ CBXShell CLSID (detected so we can cleanly migrate or replace it).
pub const CLSID_LEGACY_CBXSHELL_STR: &str = "{9E6ECB90-5A61-42BD-B851-D3297D9C7F39}";

pub const APP_REG_KEY: &str =
    r"Software\cbxshell-rs\{D84E6B7A-3C91-4F2E-A518-7B9D02E4C6F1}";

/// ShellEx handler GUIDs.
pub const SHELLEX_THUMBNAIL_PROVIDER: &str = "{E357FCCD-A995-4576-B01F-234630154E96}";
pub const SHELLEX_EXTRACT_IMAGE: &str = "{BB2E617C-0920-11D1-9A0B-00C04FC2D6C1}";
pub const SHELLEX_QUERY_INFO: &str = "{00021500-0000-0000-C000-000000000046}";

#[link(name = "kernel32")]
extern "system" {
    fn GetUserDefaultUILanguage() -> u16;
}

/// UI Display Language for the Setup Wizard and Manager GUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiLang {
    ZhTw,
    En,
}

impl UiLang {
    /// Detects the default UI language from the active Windows user locale.
    pub fn detect_system() -> Self {
        let lang_id = unsafe { GetUserDefaultUILanguage() };
        // Primary language ID 0x04 = LANG_CHINESE
        if (lang_id & 0x03FF) == 0x04 {
            Self::ZhTw
        } else {
            Self::En
        }
    }

    /// Loads saved language preference from the Registry, falling back to `detect_system()`.
    pub fn load_or_detect() -> Self {
        let key_w = to_wide(APP_REG_KEY);
        let mut hk = HKEY::default();
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(key_w.as_ptr()),
                0,
                KEY_READ,
                &mut hk,
            )
        };
        if status == ERROR_SUCCESS {
            let saved = read_dword(hk, "UiLang");
            let _ = unsafe { RegCloseKey(hk) };
            match saved {
                Some(1) => return Self::ZhTw,
                Some(2) => return Self::En,
                _ => {}
            }
        }
        Self::detect_system()
    }

    /// Saves the selected UI language to the Registry (`1 = ZhTw`, `2 = En`).
    pub fn save(self) {
        let key_w = to_wide(APP_REG_KEY);
        let mut hk = HKEY::default();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(key_w.as_ptr()),
                0,
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut hk,
                None,
            )
        };
        if status == ERROR_SUCCESS {
            let code = match self {
                Self::ZhTw => 1,
                Self::En => 2,
            };
            let _ = write_dword(hk, "UiLang", code);
            let _ = unsafe { RegCloseKey(hk) };
        }
    }

    /// Toggles between `ZhTw` and `En`.
    pub fn toggle(self) -> Self {
        match self {
            Self::ZhTw => Self::En,
            Self::En => Self::ZhTw,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    Cbz,
    Zip,
    Cbr,
    Rar,
    Cb7,
    SevenZ,
    Cbt,
    Tar,
    Epub,
}

#[derive(Debug, Clone, Copy)]
pub struct FileTypeInfo {
    pub file_type: FileType,
    pub ext: &'static str,
    pub label: &'static str,
    pub label_zh: &'static str,
    pub description: &'static str,
    pub default_on: bool,
}

impl FileTypeInfo {
    pub fn localized_label(&self, lang: UiLang) -> &'static str {
        match lang {
            UiLang::ZhTw => self.label_zh,
            UiLang::En => self.label,
        }
    }
}

pub const FILE_TYPES: &[FileTypeInfo] = &[
    FileTypeInfo {
        file_type: FileType::Cbz,
        ext: ".cbz",
        label: "CBZ  (ZIP image archive)",
        label_zh: "CBZ  (ZIP 圖片壓縮檔)",
        description: "ZIP Image Archive",
        default_on: true,
    },
    FileTypeInfo {
        file_type: FileType::Zip,
        ext: ".zip",
        label: "ZIP  Archives",
        label_zh: "ZIP  一般壓縮檔",
        description: "ZIP Archive",
        default_on: true,
    },
    FileTypeInfo {
        file_type: FileType::Cbr,
        ext: ".cbr",
        label: "CBR  (RAR4 / RAR5 image archive)",
        label_zh: "CBR  (RAR4 / RAR5 圖片壓縮檔)",
        description: "RAR Image Archive",
        default_on: true,
    },
    FileTypeInfo {
        file_type: FileType::Rar,
        ext: ".rar",
        label: "RAR  Archives",
        label_zh: "RAR  一般壓縮檔",
        description: "RAR Archive",
        default_on: true,
    },
    FileTypeInfo {
        file_type: FileType::Cb7,
        ext: ".cb7",
        label: "CB7  (7z image archive)",
        label_zh: "CB7  (7z 圖片壓縮檔)",
        description: "7z Image Archive",
        default_on: true,
    },
    FileTypeInfo {
        file_type: FileType::SevenZ,
        ext: ".7z",
        label: "7Z   Archives",
        label_zh: "7Z   一般壓縮檔",
        description: "7z Archive",
        default_on: true,
    },
    FileTypeInfo {
        file_type: FileType::Cbt,
        ext: ".cbt",
        label: "CBT  (TAR image archive)",
        label_zh: "CBT  (TAR 圖片壓縮檔)",
        description: "TAR Image Archive",
        default_on: true,
    },
    FileTypeInfo {
        file_type: FileType::Tar,
        ext: ".tar",
        label: "TAR  Archives",
        label_zh: "TAR  一般壓縮檔",
        description: "TAR Archive",
        default_on: true,
    },
    FileTypeInfo {
        file_type: FileType::Epub,
        ext: ".epub",
        label: "EPUB e-books (OPF cover)",
        label_zh: "EPUB 電子書 (OPF 封面解析)",
        description: "EPUB e-book",
        default_on: true,
    },
];

pub fn find_file_type_by_ext(ext: &str) -> Option<&'static FileTypeInfo> {
    FILE_TYPES
        .iter()
        .find(|ft| ft.ext.eq_ignore_ascii_case(ext))
}

#[derive(Debug, Clone)]
pub struct Settings {
    /// Sort image entries using Windows natural sort (`StrCmpLogicalW`).
    pub sort_alphabetically: bool,
    /// Crop ultra-tall pages (aspect ratio > 3:1, e.g. webtoon strips) to their top 2:3 region.
    pub crop_tall_strips: bool,
    /// Register `IQueryInfo` tooltip handler alongside thumbnails.
    pub enable_infotip: bool,
    /// Show bookmark identification badge on EPUB thumbnails.
    pub badge_epub: bool,
    /// Show video identification badge on animated image thumbnails (Animated WebP, GIF, APNG).
    pub badge_animated: bool,
    /// Show static image format identification badge on static image archive thumbnails.
    pub badge_static: bool,
    /// Detect suspicious path traversals, RTLO spoofing, trailing dots, or hidden executables inside archives.
    pub anomaly_detection: bool,
    /// Skip monochromatic or overly simple images (e.g. plain text disclaimers) in archives.
    pub filter_plain_covers: bool,
    /// Maximum uncompressed bytes allowed for a single cover candidate (in MiB).
    pub max_image_mb: u32,
    /// Maximum wall-clock time budget (in seconds) for scanning an archive.
    pub max_scan_seconds: u32,

    // --- Smart Rendering & Media Preview settings ---
    /// Neutral seek position as percentage of video duration (0..100, default 25%).
    pub position_seek: u32,
    /// Seek mode: 0 = Percentage (default), 1 = Fixed seconds offset, 2 = Conditional duration threshold.
    pub seek_mode: u32,
    /// Fixed seek offset in seconds (default 120s / 2 minutes).
    pub seek_fixed_sec: u32,
    /// Duration threshold in minutes for conditional seek mode (default 60 minutes).
    pub seek_threshold_min: u32,
    /// Step probe interval in seconds when searching acceptable keyframes (default 6s).
    pub step_probe_sec: u32,
    /// Maximum keyframe probes / consecutive rejected keyframes before accepting best fallback (default 6, range 1..20).
    pub consecutive_unacceptable_limit: u32,
    /// Enable mean luminance filtering (skip too dark or too bright frames).
    pub luminance_enabled: bool,
    /// Lower luminance limit percentage (default 40%).
    pub luminance_lower_limit: u32,
    /// Upper luminance limit percentage (default 80%).
    pub luminance_upper_limit: u32,
    /// Enable Shannon Entropy filtering (skip flat/blank/single-color frames).
    pub entropy_enabled: bool,
    /// Lower Shannon Entropy limit percentage (default 20%).
    pub entropy_lower_limit: u32,
    /// Maximum forward keyframe hops when Media Foundation snaps back to the same GOP keyframe (default 8, 0 = disabled).
    pub max_gop_hops: u32,
    /// Extract embedded album cover art for audio/video files.
    pub cover_art_enabled: bool,
    /// Enable ACES Filmic Tone-Mapping for HDR video content.
    pub hdr_tonemap_enabled: bool,
    /// Shared border decoration style (kept for compatibility).
    pub border_decoration: u32,
    /// Independent border decoration for archives (0 = None, 1 = Filmstrip, 2 = DropShadow, 3 = PhotoFrame).
    pub border_archive: u32,
    /// Independent border decoration for videos (0 = None, 1 = Filmstrip, 2 = DropShadow, 3 = PhotoFrame).
    pub border_video: u32,
    /// Independent border decoration for audio (0 = None, 1 = Filmstrip, 2 = DropShadow, 3 = PhotoFrame).
    pub border_audio: u32,
    /// Independent border decoration for ebooks (0 = None, 1 = Filmstrip, 2 = DropShadow, 3 = PhotoFrame, 4 = BookCover).
    pub border_ebook: u32,
    /// Enable custom/manual video algorithm parameters (if false, defaults are locked and sliders disabled).
    pub custom_video_settings: bool,
    /// Overlay extra security risk warning badge (RISK) when suspicious content is detected.
    pub badge_risk: bool,
    /// Sub-rule: Disguised executables and scripts (.exe, .bat, .ps1, etc.)
    pub risk_rule_executables: bool,
    /// Sub-rule: Path traversal escapes and absolute paths (.. / ADS)
    pub risk_rule_traversal: bool,
    /// Sub-rule: Hidden files and dot directories (.git/, .env, __MACOSX)
    pub risk_rule_hidden_files: bool,
    /// Sub-rule: Plain text and documentation files (.txt, .nfo, .diz)
    pub risk_rule_text_files: bool,
    /// Sub-rule: Sensitive certificates and credentials (.key, .pem, .pfx)
    pub risk_rule_sensitive: bool,
    /// Sub-rule: Evasion techniques (RTLO BiDi, trailing dots/spaces, reserved names)
    pub risk_rule_evasion: bool,
    /// Show format corner badge on video thumbnails.
    pub badge_video: bool,
    /// Show format corner badge on audio thumbnails.
    pub badge_audio: bool,
    /// Enable real-time activity monitoring log.
    pub monitor_enabled: bool,
    /// Video decoding execution mode: true = Safe Mode (Process Isolation in the dllhost.exe COM surrogate, default), false = Native Mode (In-Process in explorer.exe).
    pub video_sandbox_mode: bool,
    /// Custom FFmpeg executable path (empty string = auto-detect).
    pub custom_ffmpeg_path: String,
    /// Hard watchdog timeout in seconds for processing a single file (default 12s, range 3..60s).
    pub watchdog_timeout_sec: u32,
    /// When sorting archive images naturally, prefer the first image in the largest continuous numbered filename sequence.
    pub prefer_continuous_sequence: bool,
    /// Comma-separated custom extensions for plain text thumbnails in the "Other" category (e.g., "nfo, cfg").
    pub custom_text_extensions: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sort_alphabetically: true,
            crop_tall_strips: true,
            enable_infotip: true,
            badge_epub: true,
            badge_animated: true,
            badge_static: true,
            anomaly_detection: true,
            filter_plain_covers: true,
            max_image_mb: 256,
            max_scan_seconds: 20,

            position_seek: 25,
            seek_mode: 2, // Default: Smart Duration (智慧時長)
            seek_fixed_sec: 120,
            seek_threshold_min: 60,
            step_probe_sec: 6,
            consecutive_unacceptable_limit: 6,
            luminance_enabled: true,
            luminance_lower_limit: 40,
            luminance_upper_limit: 80,
            entropy_enabled: true,
            entropy_lower_limit: 20,
            max_gop_hops: 8,
            cover_art_enabled: true,
            hdr_tonemap_enabled: true,
            border_decoration: 1, // Default to filmstrip
            border_archive: 5,    // Default RoundOnly (無外框圓弧) for archives
            border_video: 1,      // Default Filmstrip for video
            border_audio: 0,      // Default None for audio
            custom_video_settings: false, // Default recommended parameters
            badge_risk: true,
            risk_rule_executables: true,
            risk_rule_traversal: true,
            risk_rule_hidden_files: true,
            risk_rule_text_files: true,
            risk_rule_sensitive: true,
            risk_rule_evasion: true,
            badge_video: false,   // Deprecated / disabled by default
            badge_audio: true,
            monitor_enabled: false, // Default disabled as requested
            video_sandbox_mode: true, // Default to Safe Mode (Process Isolation)
            custom_ffmpeg_path: String::new(), // Default to auto-detect
            watchdog_timeout_sec: 12, // Default 12s hard watchdog limit
            border_ebook: 4, // Default 4 = BookCover
            prefer_continuous_sequence: true,
            custom_text_extensions: String::new(),
        }
    }
}

impl Settings {
    pub const MIN_IMAGE_MB: u32 = 8;
    pub const MAX_IMAGE_MB: u32 = 2047;

    pub fn max_image_bytes(&self) -> u64 {
        (self.max_image_mb as u64) * 1024 * 1024
    }

    pub fn smart_render_config(&self) -> crate::media::smart_render::SmartRenderConfig {
        let default_cfg = if self.custom_video_settings {
            None
        } else {
            Some(Self::default())
        };
        let src = default_cfg.as_ref().unwrap_or(self);
        crate::media::smart_render::SmartRenderConfig {
            position_seek_percent: src.position_seek,
            seek_mode: src.seek_mode,
            seek_fixed_sec: src.seek_fixed_sec,
            seek_threshold_min: src.seek_threshold_min,
            step_probe_sec: src.step_probe_sec,
            max_gop_hops: src.max_gop_hops,
            consecutive_unacceptable_limit: src.consecutive_unacceptable_limit,
            luminance_enabled: src.luminance_enabled,
            luminance_lower_limit: src.luminance_lower_limit,
            luminance_upper_limit: src.luminance_upper_limit,
            entropy_enabled: src.entropy_enabled,
            entropy_lower_limit: src.entropy_lower_limit,
            hdr_tonemap_enabled: self.hdr_tonemap_enabled,
            watchdog_timeout_sec: src.watchdog_timeout_sec.clamp(3, 60),
        }
    }

    pub fn risk_rules(&self) -> crate::archive::RiskRules {
        crate::archive::RiskRules {
            executables: self.risk_rule_executables,
            traversal: self.risk_rule_traversal,
            hidden_files: self.risk_rule_hidden_files,
            text_files: self.risk_rule_text_files,
            sensitive: self.risk_rule_sensitive,
            evasion: self.risk_rule_evasion,
        }
    }

    pub fn border_decoration_enum(&self) -> crate::badge::BorderDecoration {
        crate::badge::BorderDecoration::from_u32(self.border_decoration)
    }

    pub fn border_archive_enum(&self) -> crate::badge::BorderDecoration {
        crate::badge::BorderDecoration::from_u32(self.border_archive)
    }

    pub fn border_video_enum(&self) -> crate::badge::BorderDecoration {
        crate::badge::BorderDecoration::from_u32(self.border_video)
    }

    pub fn border_audio_enum(&self) -> crate::badge::BorderDecoration {
        crate::badge::BorderDecoration::from_u32(self.border_audio)
    }

    pub fn border_ebook_enum(&self) -> crate::badge::BorderDecoration {
        crate::badge::BorderDecoration::from_u32(self.border_ebook)
    }

    /// Parses comma/whitespace-separated custom plain text extensions (lowercase, without leading dot).
    pub fn parsed_custom_text_extensions(&self) -> Vec<String> {
        let mut exts = Vec::new();
        for token in self
            .custom_text_extensions
            .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        {
            let cleaned = token.trim().trim_start_matches('.').to_ascii_lowercase();
            if !cleaned.is_empty()
                && cleaned.len() <= 16
                && cleaned
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                && !exts.contains(&cleaned)
            {
                exts.push(cleaned);
            }
        }
        exts
    }

    /// Zero-Trust defensive validation and sanitization.
    /// Protects against manual registry tampering, corrupted inputs, or out-of-bounds parameters.
    pub fn sanitize(&mut self) {
        self.max_image_mb = self.max_image_mb.clamp(Self::MIN_IMAGE_MB, Self::MAX_IMAGE_MB);
        self.max_scan_seconds = self.max_scan_seconds.clamp(1, 120);

        self.position_seek = self.position_seek.clamp(0, 100);
        self.seek_mode = self.seek_mode.clamp(0, 2);
        self.seek_fixed_sec = self.seek_fixed_sec.clamp(0, 36000);
        self.seek_threshold_min = self.seek_threshold_min.clamp(1, 1440);
        self.step_probe_sec = self.step_probe_sec.clamp(1, 60);
        self.consecutive_unacceptable_limit = self.consecutive_unacceptable_limit.clamp(1, 20);
        self.max_gop_hops = self.max_gop_hops.clamp(0, 20);

        self.luminance_lower_limit = self.luminance_lower_limit.clamp(0, 100);
        self.luminance_upper_limit = self.luminance_upper_limit.clamp(0, 100);
        if self.luminance_lower_limit >= self.luminance_upper_limit {
            // Tampered or inverted bounds: restore safe default separation
            if self.luminance_lower_limit >= 95 {
                self.luminance_lower_limit = 40;
                self.luminance_upper_limit = 80;
            } else {
                self.luminance_upper_limit = (self.luminance_lower_limit + 10).min(100);
            }
        }

        self.entropy_lower_limit = self.entropy_lower_limit.clamp(0, 100);
        self.watchdog_timeout_sec = self.watchdog_timeout_sec.clamp(3, 60);

        self.border_decoration = self.border_decoration.clamp(0, 5);
        self.border_archive = self.border_archive.clamp(0, 5);
        self.border_video = self.border_video.clamp(0, 5);
        self.border_audio = self.border_audio.clamp(0, 5);
        self.border_ebook = self.border_ebook.clamp(0, 5);
        let cleaned_exts = self.parsed_custom_text_extensions();
        self.custom_text_extensions = cleaned_exts.join(", ");

        // Sanitize custom FFmpeg path
        let trimmed = self.custom_ffmpeg_path.trim();
        if trimmed.is_empty() {
            self.custom_ffmpeg_path.clear();
        } else {
            // Check for illegal characters, UNC/device paths, NTFS ADS colons, or relative paths
            let has_invalid_chars = trimmed.chars().any(|c| {
                c.is_control() || matches!(c, '"' | '\'' | '|' | '&' | ';' | '<' | '>' | '`' | '$' | '^' | '%')
            });
            let is_unc_or_device = trimmed.starts_with("\\\\") || trimmed.starts_with("//");
            let has_bad_colon = trimmed
                .char_indices()
                .any(|(idx, ch)| ch == ':' && idx != 1);
            let p = std::path::Path::new(trimmed);
            let is_exe = p.extension().map_or(false, |ext| ext.eq_ignore_ascii_case("exe"));
            if has_invalid_chars
                || is_unc_or_device
                || has_bad_colon
                || !p.is_absolute()
                || trimmed.len() > 260
                || !is_exe
                || !p.is_file()
            {
                self.custom_ffmpeg_path.clear();
            } else {
                self.custom_ffmpeg_path = trimmed.to_string();
            }
        }
    }

    pub fn load() -> Self {
        let mut s = Self::default();
        let key_w = to_wide(APP_REG_KEY);
        let mut hk = HKEY::default();
        // SAFETY: key_w is null-terminated and hk is a valid out-pointer.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(key_w.as_ptr()),
                0,
                KEY_READ,
                &mut hk,
            )
        };
        if status != ERROR_SUCCESS {
            // Check if legacy Media Preview registry exists in HKLM for seamless migration
            s.migrate_from_legacy_media_preview();
            s.sanitize();
            return s;
        }

        if let Some(v) = read_dword(hk, "NoSort") {
            s.sort_alphabetically = v == 0;
        }
        if let Some(v) = read_dword(hk, "NoCropTall") {
            s.crop_tall_strips = v == 0;
        }
        if let Some(v) = read_dword(hk, "NoInfoTip") {
            s.enable_infotip = v == 0;
        }
        if let Some(v) = read_dword(hk, "NoBadgeEpub") {
            s.badge_epub = v == 0;
        }
        if let Some(v) = read_dword(hk, "NoBadgeAnimated") {
            s.badge_animated = v == 0;
        }
        if let Some(v) = read_dword(hk, "NoBadgeStatic") {
            s.badge_static = v == 0;
        }
        if let Some(v) = read_dword(hk, "NoAnomalyDetection") {
            s.anomaly_detection = v == 0;
        }
        if let Some(v) = read_dword(hk, "FilterPlainCovers") {
            s.filter_plain_covers = v != 0;
        }
        if let Some(v) = read_dword(hk, "MaxImageMB") {
            s.max_image_mb = v.clamp(Self::MIN_IMAGE_MB, Self::MAX_IMAGE_MB);
        }
        if let Some(v) = read_dword(hk, "MaxScanSeconds") {
            s.max_scan_seconds = v.clamp(2, 300);
        }

        // Multimedia settings
        if let Some(v) = read_dword(hk, "PositionSeek") {
            s.position_seek = v.clamp(0, 100);
        }
        if let Some(v) = read_dword(hk, "SeekMode") {
            s.seek_mode = v.clamp(0, 2);
        }
        if let Some(v) = read_dword(hk, "SeekFixedSec") {
            s.seek_fixed_sec = v.clamp(0, 3600);
        }
        if let Some(v) = read_dword(hk, "SeekThresholdMin") {
            s.seek_threshold_min = v.clamp(1, 480);
        }
        if let Some(v) = read_dword(hk, "Luminance") {
            s.luminance_enabled = v != 0;
        }
        if let Some(v) = read_dword(hk, "LuminanceLowerLimit") {
            s.luminance_lower_limit = v.clamp(0, 100);
        }
        if let Some(v) = read_dword(hk, "LuminanceUpperLimit") {
            s.luminance_upper_limit = v.clamp(0, 100);
        }
        if let Some(v) = read_dword(hk, "Entropy") {
            s.entropy_enabled = v != 0;
        }
        if let Some(v) = read_dword(hk, "EntropyLowerLimit") {
            s.entropy_lower_limit = v.clamp(0, 100);
        }
        if let Some(v) = read_dword(hk, "CoverArt") {
            s.cover_art_enabled = v != 0;
        }
        if let Some(v) = read_dword(hk, "HdrToneMap") {
            s.hdr_tonemap_enabled = v != 0;
        }
        if let Some(v) = read_dword(hk, "BorderDecoration") {
            s.border_decoration = v.clamp(0, 5);
        }
        if let Some(v) = read_dword(hk, "BadgeVideo") {
            s.badge_video = v != 0;
        }
        if let Some(v) = read_dword(hk, "BadgeAudio") {
            s.badge_audio = v != 0;
        }
        if let Some(v) = read_dword(hk, "BorderArchive") {
            s.border_archive = v.clamp(0, 5);
        }
        if let Some(v) = read_dword(hk, "BorderVideo") {
            s.border_video = v.clamp(0, 5);
        }
        if let Some(v) = read_dword(hk, "BorderAudio") {
            s.border_audio = v.clamp(0, 5);
        }
        if let Some(v) = read_dword(hk, "CustomVideoSettings") {
            s.custom_video_settings = v != 0;
        }
        if let Some(v) = read_dword(hk, "NoBadgeRisk") {
            s.badge_risk = v == 0;
        }
        if let Some(v) = read_dword(hk, "StepProbeSec") {
            s.step_probe_sec = v.clamp(1, 60);
        }
        if let Some(v) = read_dword(hk, "ConsecutiveLimit") {
            s.consecutive_unacceptable_limit = v.clamp(1, 20);
        }
        if let Some(v) = read_dword(hk, "MaxGopHops") {
            s.max_gop_hops = v.clamp(0, 20);
        }
        if let Some(v) = read_dword(hk, "RiskRuleExecutables") {
            s.risk_rule_executables = v != 0;
        }
        if let Some(v) = read_dword(hk, "RiskRuleTraversal") {
            s.risk_rule_traversal = v != 0;
        }
        if let Some(v) = read_dword(hk, "RiskRuleHiddenFiles") {
            s.risk_rule_hidden_files = v != 0;
        }
        if let Some(v) = read_dword(hk, "RiskRuleTextFiles") {
            s.risk_rule_text_files = v != 0;
        }
        if let Some(v) = read_dword(hk, "RiskRuleSensitive") {
            s.risk_rule_sensitive = v != 0;
        }
        if let Some(v) = read_dword(hk, "RiskRuleEvasion") {
            s.risk_rule_evasion = v != 0;
        }
        if let Some(v) = read_dword(hk, "NoMonitor") {
            s.monitor_enabled = v == 0;
        }
        if let Some(v) = read_dword(hk, "VideoSandboxMode") {
            s.video_sandbox_mode = v != 0;
        }
        if let Some(path) = read_string(hk, "CustomFFmpegPath") {
            s.custom_ffmpeg_path = path;
        }
        if let Some(v) = read_dword(hk, "WatchdogTimeoutSec") {
            s.watchdog_timeout_sec = v.clamp(3, 60);
        }
        if let Some(v) = read_dword(hk, "BorderEbook") {
            s.border_ebook = v.clamp(0, 5);
        }
        if let Some(v) = read_dword(hk, "PreferContinuousSequence") {
            s.prefer_continuous_sequence = v != 0;
        }
        if let Some(v) = read_string(hk, "CustomTextExtensions") {
            s.custom_text_extensions = v;
        }

        // SAFETY: hk was opened above.
        let _ = unsafe { RegCloseKey(hk) };
        s.sanitize();
        s
    }

    fn migrate_from_legacy_media_preview(&mut self) {
        let legacy_key_w = to_wide(r"Software\BabelSoft\Media Preview");
        let mut hk = HKEY::default();
        let status = unsafe {
            RegOpenKeyExW(
                windows::Win32::System::Registry::HKEY_LOCAL_MACHINE,
                PCWSTR(legacy_key_w.as_ptr()),
                0,
                KEY_READ,
                &mut hk,
            )
        };
        if status == ERROR_SUCCESS {
            if let Some(v) = read_dword(hk, "PositionSeek") {
                self.position_seek = v.clamp(0, 100);
            }
            if let Some(v) = read_dword(hk, "Luminance") {
                self.luminance_enabled = v != 0;
            }
            if let Some(v) = read_dword(hk, "LuminanceLowerLimit") {
                self.luminance_lower_limit = v.clamp(0, 100);
            }
            if let Some(v) = read_dword(hk, "LuminanceUpperLimit") {
                self.luminance_upper_limit = v.clamp(0, 100);
            }
            if let Some(v) = read_dword(hk, "Entropy") {
                self.entropy_enabled = v != 0;
            }
            if let Some(v) = read_dword(hk, "EntropyLowerLimit") {
                self.entropy_lower_limit = v.clamp(0, 100);
            }
            if let Some(v) = read_dword(hk, "CoverArt") {
                self.cover_art_enabled = v != 0;
            }
            let _ = unsafe { RegCloseKey(hk) };
        }
    }

    pub fn save(&self) -> bool {
        let key_w = to_wide(APP_REG_KEY);
        let mut hk = HKEY::default();
        // SAFETY: key_w is null-terminated and hk is a valid out-pointer.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(key_w.as_ptr()),
                0,
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_WRITE,
                None,
                &mut hk,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            return false;
        }

        let ok = write_dword(hk, "NoSort", if self.sort_alphabetically { 0 } else { 1 })
            && write_dword(hk, "NoCropTall", if self.crop_tall_strips { 0 } else { 1 })
            && write_dword(hk, "NoInfoTip", if self.enable_infotip { 0 } else { 1 })
            && write_dword(hk, "NoBadgeEpub", if self.badge_epub { 0 } else { 1 })
            && write_dword(hk, "NoBadgeAnimated", if self.badge_animated { 0 } else { 1 })
            && write_dword(hk, "NoBadgeStatic", if self.badge_static { 0 } else { 1 })
            && write_dword(
                hk,
                "NoAnomalyDetection",
                if self.anomaly_detection { 0 } else { 1 },
            )
            && write_dword(
                hk,
                "FilterPlainCovers",
                if self.filter_plain_covers { 1 } else { 0 },
            )
            && write_dword(
                hk,
                "MaxImageMB",
                self.max_image_mb.clamp(Self::MIN_IMAGE_MB, Self::MAX_IMAGE_MB),
            )
            && write_dword(hk, "MaxScanSeconds", self.max_scan_seconds.clamp(2, 300))
            && write_dword(hk, "PositionSeek", self.position_seek.clamp(0, 100))
            && write_dword(hk, "SeekMode", self.seek_mode.clamp(0, 2))
            && write_dword(hk, "SeekFixedSec", self.seek_fixed_sec.clamp(0, 3600))
            && write_dword(hk, "SeekThresholdMin", self.seek_threshold_min.clamp(1, 480))
            && write_dword(hk, "Luminance", if self.luminance_enabled { 1 } else { 0 })
            && write_dword(hk, "LuminanceLowerLimit", self.luminance_lower_limit.clamp(0, 100))
            && write_dword(hk, "LuminanceUpperLimit", self.luminance_upper_limit.clamp(0, 100))
            && write_dword(hk, "Entropy", if self.entropy_enabled { 1 } else { 0 })
            && write_dword(hk, "EntropyLowerLimit", self.entropy_lower_limit.clamp(0, 100))
            && write_dword(hk, "MaxGopHops", self.max_gop_hops.clamp(0, 20))
            && write_dword(hk, "CoverArt", if self.cover_art_enabled { 1 } else { 0 })
            && write_dword(hk, "HdrToneMap", if self.hdr_tonemap_enabled { 1 } else { 0 })
            && write_dword(hk, "BorderDecoration", self.border_decoration.clamp(0, 5))
            && write_dword(hk, "BadgeVideo", if self.badge_video { 1 } else { 0 })
            && write_dword(hk, "BadgeAudio", if self.badge_audio { 1 } else { 0 })
            && write_dword(hk, "BorderArchive", self.border_archive.clamp(0, 5))
            && write_dword(hk, "BorderVideo", self.border_video.clamp(0, 5))
            && write_dword(hk, "BorderAudio", self.border_audio.clamp(0, 5))
            && write_dword(hk, "CustomVideoSettings", if self.custom_video_settings { 1 } else { 0 })
            && write_dword(hk, "NoBadgeRisk", if self.badge_risk { 0 } else { 1 })
            && write_dword(hk, "StepProbeSec", self.step_probe_sec.clamp(1, 60))
            && write_dword(hk, "ConsecutiveLimit", self.consecutive_unacceptable_limit.clamp(1, 20))
            && write_dword(hk, "RiskRuleExecutables", if self.risk_rule_executables { 1 } else { 0 })
            && write_dword(hk, "RiskRuleTraversal", if self.risk_rule_traversal { 1 } else { 0 })
            && write_dword(hk, "RiskRuleHiddenFiles", if self.risk_rule_hidden_files { 1 } else { 0 })
            && write_dword(hk, "RiskRuleTextFiles", if self.risk_rule_text_files { 1 } else { 0 })
            && write_dword(hk, "RiskRuleSensitive", if self.risk_rule_sensitive { 1 } else { 0 })
            && write_dword(hk, "RiskRuleEvasion", if self.risk_rule_evasion { 1 } else { 0 })
            && write_dword(hk, "NoMonitor", if self.monitor_enabled { 0 } else { 1 })
            && write_dword(hk, "VideoSandboxMode", if self.video_sandbox_mode { 1 } else { 0 })
            && write_string(hk, "CustomFFmpegPath", &self.custom_ffmpeg_path)
            && write_dword(hk, "WatchdogTimeoutSec", self.watchdog_timeout_sec.clamp(3, 60))
            && write_dword(hk, "BorderEbook", self.border_ebook.clamp(0, 5))
            && write_dword(hk, "PreferContinuousSequence", u32::from(self.prefer_continuous_sequence))
            && write_string(hk, "CustomTextExtensions", &self.custom_text_extensions);

        // SAFETY: hk was created above.
        let _ = unsafe { RegCloseKey(hk) };
        ok
    }
}

pub fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Returns `true` when Windows is configured to use Dark Mode for apps
/// (`HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize\AppsUseLightTheme == 0`).
pub fn is_system_dark_mode() -> bool {
    let subkey = to_wide(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
    let mut hk = HKEY::default();
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(subkey.as_ptr()), 0, KEY_READ, &mut hk) };
    if status != ERROR_SUCCESS {
        return false;
    }
    let val = read_dword(hk, "AppsUseLightTheme");
    let _ = unsafe { RegCloseKey(hk) };
    matches!(val, Some(0))
}

fn read_dword(hk: HKEY, name: &str) -> Option<u32> {
    let name_w = to_wide(name);
    let mut data: u32 = 0;
    let mut cb = std::mem::size_of::<u32>() as u32;
    let mut ty = REG_DWORD;
    // SAFETY: valid buffers and sizes.
    let status = unsafe {
        RegQueryValueExW(
            hk,
            PCWSTR(name_w.as_ptr()),
            None,
            Some(&mut ty),
            Some((&mut data as *mut u32).cast::<u8>()),
            Some(&mut cb),
        )
    };
    if status == ERROR_SUCCESS && ty == REG_DWORD && cb == 4 {
        Some(data)
    } else {
        None
    }
}

fn write_dword(hk: HKEY, name: &str, val: u32) -> bool {
    let name_w = to_wide(name);
    let bytes = val.to_ne_bytes();
    // SAFETY: bytes is a 4-byte slice.
    unsafe {
        RegSetValueExW(
            hk,
            PCWSTR(name_w.as_ptr()),
            0,
            REG_DWORD,
            Some(&bytes),
        ) == ERROR_SUCCESS
    }
}

pub fn read_string(hk: HKEY, name: &str) -> Option<String> {
    let name_w = to_wide(name);
    let mut cb: u32 = 0;
    let mut ty = REG_SZ;
    // Query buffer size first
    let status = unsafe {
        RegQueryValueExW(
            hk,
            PCWSTR(name_w.as_ptr()),
            None,
            Some(&mut ty),
            None,
            Some(&mut cb),
        )
    };
    if status != ERROR_SUCCESS || (ty != REG_SZ && ty != windows::Win32::System::Registry::REG_EXPAND_SZ) || cb == 0 {
        return None;
    }
    let u16_len = (cb as usize) / 2;
    let mut buf = vec![0u16; u16_len + 1];
    let status = unsafe {
        RegQueryValueExW(
            hk,
            PCWSTR(name_w.as_ptr()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr().cast::<u8>()),
            Some(&mut cb),
        )
    };
    if status == ERROR_SUCCESS {
        let actual_len = (cb as usize) / 2;
        let slice = &buf[..actual_len];
        let trimmed = if let Some(pos) = slice.iter().position(|&c| c == 0) {
            &slice[..pos]
        } else {
            slice
        };
        Some(String::from_utf16_lossy(trimmed))
    } else {
        None
    }
}

pub fn write_string(hk: HKEY, name: &str, val: &str) -> bool {
    let name_w = to_wide(name);
    let val_w = to_wide(val);
    let byte_slice = unsafe {
        std::slice::from_raw_parts(val_w.as_ptr().cast::<u8>(), val_w.len() * 2)
    };
    unsafe {
        RegSetValueExW(
            hk,
            PCWSTR(name_w.as_ptr()),
            0,
            REG_SZ,
            Some(byte_slice),
        ) == ERROR_SUCCESS
    }
}

/// Checks whether BabelSoft Media Preview is installed on the system (HKLM or HKCU).
pub fn is_media_preview_installed() -> bool {
    let key_w = to_wide(r"Software\BabelSoft\Media Preview");
    for root in [windows::Win32::System::Registry::HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        let mut hk = HKEY::default();
        let status = unsafe {
            RegOpenKeyExW(
                root,
                PCWSTR(key_w.as_ptr()),
                0,
                KEY_READ,
                &mut hk,
            )
        };
        if status == ERROR_SUCCESS {
            let _ = unsafe { RegCloseKey(hk) };
            return true;
        }
    }
    false
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DisplayLuid {
    low_part: u32,
    high_part: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DisplayConfigDeviceInfoHeader {
    r#type: u32,
    size: u32,
    adapter_id: DisplayLuid,
    id: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DisplayConfigGetAdvancedColorInfo {
    header: DisplayConfigDeviceInfoHeader,
    flags: u32,
    color_encoding: u32,
    bits_per_color_channel: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DisplayConfigPathSourceInfo {
    adapter_id: DisplayLuid,
    id: u32,
    mode_info_idx: u32,
    status_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DisplayConfigPathTargetInfo {
    adapter_id: DisplayLuid,
    id: u32,
    mode_info_idx: u32,
    output_technology: u32,
    rotation: u32,
    scaling: u32,
    refresh_rate_numerator: u32,
    refresh_rate_denominator: u32,
    scan_line_ordering: u32,
    target_available: u32,
    status_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct DisplayConfigPathInfo {
    source_info: DisplayConfigPathSourceInfo,
    target_info: DisplayConfigPathTargetInfo,
    flags: u32,
}

#[link(name = "user32")]
extern "system" {
    fn GetDisplayConfigBufferSizes(
        flags: u32,
        num_path_array_elements: *mut u32,
        num_mode_info_array_elements: *mut u32,
    ) -> i32;

    fn QueryDisplayConfig(
        flags: u32,
        num_path_array_elements: *mut u32,
        path_array: *mut DisplayConfigPathInfo,
        num_mode_info_array_elements: *mut u32,
        mode_info_array: *mut std::ffi::c_void,
        current_topology_id: *mut std::ffi::c_void,
    ) -> i32;

    fn DisplayConfigGetDeviceInfo(
        request_packet: *mut DisplayConfigGetAdvancedColorInfo,
    ) -> i32;
}

/// Detects whether any active Windows display has HDR (Advanced Color) enabled.
pub fn is_hdr_display_enabled() -> bool {
    unsafe {
        let mut path_count: u32 = 0;
        let mut mode_count: u32 = 0;
        // QDC_ONLY_SPECIFIED = 0x00000002
        if GetDisplayConfigBufferSizes(0x02, &mut path_count, &mut mode_count) != 0 || path_count == 0 {
            return false;
        }
        let mut paths = vec![DisplayConfigPathInfo::default(); path_count as usize];
        if QueryDisplayConfig(
            0x02,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) != 0
        {
            return false;
        }
        for path in paths.iter().take(path_count as usize) {
            let mut info = DisplayConfigGetAdvancedColorInfo::default();
            info.header.r#type = 9; // DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO
            info.header.size = std::mem::size_of::<DisplayConfigGetAdvancedColorInfo>() as u32;
            info.header.adapter_id = path.target_info.adapter_id;
            info.header.id = path.target_info.id;
            if DisplayConfigGetDeviceInfo(&mut info) == 0 {
                // bit 1 is advancedColorEnabled (0x2)
                if (info.flags & 0x02) != 0 {
                    return true;
                }
            }
        }
        false
    }
}
