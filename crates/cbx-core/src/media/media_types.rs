use crate::settings::UiLang;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaCategory {
    ImageArchive, // 圖檔壓縮檔 (Image Archives: CBZ, CBR, CB7, CBT, ZIP, RAR, 7Z, TAR)
    Ebook,        // 電子書格式 (E-Books: EPUB, MOBI, IBA, AZW, PDF)
    Video,        // 主流與現代影片格式 (MP4, MKV, WebM, AVI, MOV, TS, WMV, FLV 等)
    Audio,        // 音訊檔案與專輯封面 (MP3, FLAC, M4A, OGG, OPUS, WAV, APE 等)
    Other,        // 其他文件與設計檔 (DOCX, DOC, XLSX, XLS, PSD, TXT, MD)
    Syntax,       // 常見語法與純文字格式 (JSON, CSV, HTML, YAML, PS1, PY 等，預設關閉)
}

#[derive(Debug, Clone)]
pub struct MediaFormatInfo {
    pub ext: &'static str,
    pub name: &'static str,
    pub category: MediaCategory,
    pub default_on: bool,
    pub description_zh: &'static str,
    pub description_en: &'static str,
}

impl MediaFormatInfo {
    pub fn description(&self, lang: UiLang) -> &'static str {
        match lang {
            UiLang::ZhTw => self.description_zh,
            UiLang::En => self.description_en,
        }
    }
}

pub const SUPPORTED_MEDIA_FORMATS: &[MediaFormatInfo] = &[
    // --- 1. 圖片壓縮檔 (Image Archives - 預設全開) ---
    MediaFormatInfo {
        ext: ".cbz",
        name: "CBZ",
        category: MediaCategory::ImageArchive,
        default_on: true,
        description_zh: "ZIP 圖片封存檔 (CBZ)",
        description_en: "ZIP Image Archive (CBZ)",
    },
    MediaFormatInfo {
        ext: ".zip",
        name: "ZIP",
        category: MediaCategory::ImageArchive,
        default_on: true,
        description_zh: "ZIP 一般壓縮檔",
        description_en: "ZIP Standard Archive",
    },
    MediaFormatInfo {
        ext: ".cbr",
        name: "CBR",
        category: MediaCategory::ImageArchive,
        default_on: true,
        description_zh: "RAR 圖片封存檔 (CBR)",
        description_en: "RAR Image Archive (CBR)",
    },
    MediaFormatInfo {
        ext: ".rar",
        name: "RAR",
        category: MediaCategory::ImageArchive,
        default_on: true,
        description_zh: "RAR 一般壓縮檔",
        description_en: "RAR Standard Archive",
    },
    MediaFormatInfo {
        ext: ".cb7",
        name: "CB7",
        category: MediaCategory::ImageArchive,
        default_on: true,
        description_zh: "7z 圖片封存檔 (CB7)",
        description_en: "7z Image Archive (CB7)",
    },
    MediaFormatInfo {
        ext: ".7z",
        name: "7Z",
        category: MediaCategory::ImageArchive,
        default_on: true,
        description_zh: "7z 一般壓縮檔",
        description_en: "7z Standard Archive",
    },
    MediaFormatInfo {
        ext: ".cbt",
        name: "CBT",
        category: MediaCategory::ImageArchive,
        default_on: true,
        description_zh: "TAR 圖片封存檔 (CBT)",
        description_en: "TAR Image Archive (CBT)",
    },
    MediaFormatInfo {
        ext: ".tar",
        name: "TAR",
        category: MediaCategory::ImageArchive,
        default_on: true,
        description_zh: "TAR 一般壓縮檔",
        description_en: "TAR Standard Archive",
    },
    // --- 1.5. 電子書格式 (E-Book Formats - PDF 預設不啟用) ---
    MediaFormatInfo {
        ext: ".epub",
        name: "EPUB",
        category: MediaCategory::Ebook,
        default_on: true,
        description_zh: "EPUB 電子書標準封面",
        description_en: "EPUB Electronic Publication",
    },
    MediaFormatInfo {
        ext: ".mobi",
        name: "MOBI",
        category: MediaCategory::Ebook,
        default_on: true,
        description_zh: "Mobipocket / Kindle 電子書 (MOBI)",
        description_en: "Mobipocket / Kindle E-Book (MOBI)",
    },
    MediaFormatInfo {
        ext: ".iba",
        name: "IBA",
        category: MediaCategory::Ebook,
        default_on: true,
        description_zh: "Apple iBooks Author 電子書 (IBA)",
        description_en: "Apple iBooks Author Book (IBA)",
    },
    MediaFormatInfo {
        ext: ".azw",
        name: "AZW",
        category: MediaCategory::Ebook,
        default_on: true,
        description_zh: "Amazon Kindle 電子書 (AZW)",
        description_en: "Amazon Kindle E-Book (AZW)",
    },
    MediaFormatInfo {
        ext: ".pdf",
        name: "PDF",
        category: MediaCategory::Ebook,
        default_on: false,
        description_zh: "PDF 可攜式文件格式 (預設關閉)",
        description_en: "Portable Document Format (Default Off)",
    },

    // --- 2. 主流與現代影片格式 (Video Formats - 預設全開) ---
    MediaFormatInfo {
        ext: ".mp4",
        name: "MP4",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "MPEG-4 Part 14 視訊 (MP4 / H.264 / HEVC / AV1)",
        description_en: "MPEG-4 Part 14 Video (MP4 / H.264 / HEVC / AV1)",
    },
    MediaFormatInfo {
        ext: ".m4v",
        name: "M4V",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "Apple M4V 視訊",
        description_en: "Apple M4V Video",
    },
    MediaFormatInfo {
        ext: ".mkv",
        name: "MKV",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "Matroska 高畫質多音軌視訊 (MKV)",
        description_en: "Matroska Multimedia Container (MKV)",
    },
    MediaFormatInfo {
        ext: ".webm",
        name: "WebM",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "WebM 開放視訊 (VP8 / VP9 / AV1)",
        description_en: "WebM Open Media (VP8 / VP9 / AV1)",
    },
    MediaFormatInfo {
        ext: ".avi",
        name: "AVI",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "Audio Video Interleaved (AVI)",
        description_en: "Audio Video Interleaved (AVI)",
    },
    MediaFormatInfo {
        ext: ".mov",
        name: "MOV",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "QuickTime 視訊 (MOV / ProRes)",
        description_en: "QuickTime Movie (MOV / ProRes)",
    },
    MediaFormatInfo {
        ext: ".wmv",
        name: "WMV",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "Windows Media Video (WMV)",
        description_en: "Windows Media Video (WMV)",
    },
    MediaFormatInfo {
        ext: ".asf",
        name: "ASF",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "Advanced Systems Format (ASF)",
        description_en: "Advanced Systems Format (ASF)",
    },
    MediaFormatInfo {
        ext: ".flv",
        name: "FLV",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "Flash 視訊 (FLV)",
        description_en: "Flash Video (FLV)",
    },
    MediaFormatInfo {
        ext: ".f4v",
        name: "F4V",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "Flash MP4 視訊 (F4V)",
        description_en: "Flash MP4 Video (F4V)",
    },
    MediaFormatInfo {
        ext: ".ts",
        name: "TS",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "MPEG-2 傳輸串流 (TS)",
        description_en: "MPEG-2 Transport Stream (TS)",
    },
    MediaFormatInfo {
        ext: ".m2ts",
        name: "M2TS",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "BDAV 藍光 / AVCHD 高畫質視訊 (M2TS)",
        description_en: "Blu-ray BDAV / AVCHD Video (M2TS)",
    },
    MediaFormatInfo {
        ext: ".mts",
        name: "MTS",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "AVCHD 攝錄影機視訊 (MTS)",
        description_en: "AVCHD Camcorder Video (MTS)",
    },
    MediaFormatInfo {
        ext: ".vob",
        name: "VOB",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "DVD Video Object (VOB)",
        description_en: "DVD Video Object (VOB)",
    },
    MediaFormatInfo {
        ext: ".3gp",
        name: "3GP",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "3GPP 行動視訊 (3GP)",
        description_en: "3GPP Mobile Video (3GP)",
    },
    MediaFormatInfo {
        ext: ".3g2",
        name: "3G2",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "3GPP2 行動視訊 (3G2)",
        description_en: "3GPP2 Mobile Video (3G2)",
    },
    MediaFormatInfo {
        ext: ".ogv",
        name: "OGV",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "Ogg Theora 視訊 (OGV)",
        description_en: "Ogg Theora Video (OGV)",
    },
    MediaFormatInfo {
        ext: ".ogm",
        name: "OGM",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "Ogg Media 視訊 (OGM)",
        description_en: "Ogg Media (OGM)",
    },
    MediaFormatInfo {
        ext: ".rm",
        name: "RM",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "RealMedia 經典視訊 (RM)",
        description_en: "RealMedia Video (RM)",
    },
    MediaFormatInfo {
        ext: ".rmvb",
        name: "RMVB",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "RealMedia 可變位元率視訊 (RMVB)",
        description_en: "RealMedia Variable Bitrate (RMVB)",
    },
    MediaFormatInfo {
        ext: ".divx",
        name: "DivX",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "DivX 視訊",
        description_en: "DivX Video",
    },
    MediaFormatInfo {
        ext: ".xvid",
        name: "Xvid",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "Xvid 視訊",
        description_en: "Xvid Video",
    },
    MediaFormatInfo {
        ext: ".mpg",
        name: "MPG",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "MPEG-1 / MPEG-2 視訊 (MPG)",
        description_en: "MPEG Video (MPG)",
    },
    MediaFormatInfo {
        ext: ".mpeg",
        name: "MPEG",
        category: MediaCategory::Video,
        default_on: true,
        description_zh: "MPEG-1 / MPEG-2 視訊 (MPEG)",
        description_en: "MPEG Video (MPEG)",
    },

    // --- 3. 音訊格式與專輯封面 (Audio Formats - 預設全開) ---
    MediaFormatInfo {
        ext: ".mp3",
        name: "MP3",
        category: MediaCategory::Audio,
        default_on: true,
        description_zh: "MPEG Audio Layer 3 (MP3 內嵌封面)",
        description_en: "MPEG Audio Layer 3 (MP3 ID3v2 APIC)",
    },
    MediaFormatInfo {
        ext: ".flac",
        name: "FLAC",
        category: MediaCategory::Audio,
        default_on: true,
        description_zh: "無損音訊編碼 (FLAC 內嵌封面)",
        description_en: "Free Lossless Audio Codec (FLAC Picture)",
    },
    MediaFormatInfo {
        ext: ".m4a",
        name: "M4A",
        category: MediaCategory::Audio,
        default_on: true,
        description_zh: "Apple MPEG-4 音訊 (M4A / AAC 封面)",
        description_en: "Apple MPEG-4 Audio (M4A / AAC covr)",
    },
    MediaFormatInfo {
        ext: ".aac",
        name: "AAC",
        category: MediaCategory::Audio,
        default_on: true,
        description_zh: "進階音訊編碼 (AAC)",
        description_en: "Advanced Audio Coding (AAC)",
    },
    MediaFormatInfo {
        ext: ".ogg",
        name: "OGG",
        category: MediaCategory::Audio,
        default_on: true,
        description_zh: "Ogg Vorbis 音訊 (OGG 封面)",
        description_en: "Ogg Vorbis Audio (OGG Picture)",
    },
    MediaFormatInfo {
        ext: ".opus",
        name: "Opus",
        category: MediaCategory::Audio,
        default_on: true,
        description_zh: "Opus 互動音訊",
        description_en: "Opus Interactive Audio Codec",
    },
    MediaFormatInfo {
        ext: ".wav",
        name: "WAV",
        category: MediaCategory::Audio,
        default_on: true,
        description_zh: "Waveform 無損音訊 (WAV)",
        description_en: "Waveform Audio File (WAV)",
    },
    MediaFormatInfo {
        ext: ".wma",
        name: "WMA",
        category: MediaCategory::Audio,
        default_on: true,
        description_zh: "Windows Media Audio (WMA)",
        description_en: "Windows Media Audio (WMA)",
    },
    MediaFormatInfo {
        ext: ".ape",
        name: "APE",
        category: MediaCategory::Audio,
        default_on: true,
        description_zh: "Monkey's Audio 無損音訊 (APE)",
        description_en: "Monkey's Audio Lossless (APE)",
    },
    MediaFormatInfo {
        ext: ".wv",
        name: "WavPack",
        category: MediaCategory::Audio,
        default_on: true,
        description_zh: "WavPack 混合無損音訊 (WV)",
        description_en: "WavPack Hybrid Lossless (WV)",
    },
    MediaFormatInfo {
        ext: ".mka",
        name: "MKA",
        category: MediaCategory::Audio,
        default_on: true,
        description_zh: "Matroska 純音訊 (MKA 內嵌封面)",
        description_en: "Matroska Audio Container (MKA)",
    },

    // --- 4. 其他文件與設計圖 (Other Documents & Artwork) ---
    MediaFormatInfo {
        ext: ".docx",
        name: "DOCX",
        category: MediaCategory::Other,
        default_on: true,
        description_zh: "Microsoft Word 文件 (DOCX)",
        description_en: "Microsoft Word Document (DOCX)",
    },
    MediaFormatInfo {
        ext: ".doc",
        name: "DOC",
        category: MediaCategory::Other,
        default_on: true,
        description_zh: "Microsoft Word 傳統文件 (DOC)",
        description_en: "Microsoft Word Legacy Document (DOC)",
    },
    MediaFormatInfo {
        ext: ".xlsx",
        name: "XLSX",
        category: MediaCategory::Other,
        default_on: true,
        description_zh: "Microsoft Excel 試算表 (XLSX)",
        description_en: "Microsoft Excel Workbook (XLSX)",
    },
    MediaFormatInfo {
        ext: ".xls",
        name: "XLS",
        category: MediaCategory::Other,
        default_on: true,
        description_zh: "Microsoft Excel 傳統試算表 (XLS)",
        description_en: "Microsoft Excel Legacy Workbook (XLS)",
    },
    MediaFormatInfo {
        ext: ".psd",
        name: "PSD",
        category: MediaCategory::Other,
        default_on: true,
        description_zh: "Adobe Photoshop 設計檔案 (PSD)",
        description_en: "Adobe Photoshop Document (PSD)",
    },
    MediaFormatInfo {
        ext: ".txt",
        name: "TXT",
        category: MediaCategory::Other,
        default_on: true,
        description_zh: "純文字文件 (TXT)",
        description_en: "Plain Text File (TXT)",
    },
    MediaFormatInfo {
        ext: ".md",
        name: "MD",
        category: MediaCategory::Other,
        default_on: true,
        description_zh: "Markdown 文件 (MD)",
        description_en: "Markdown Document (MD)",
    },
    MediaFormatInfo {
        ext: ".torrent",
        name: "TORRENT",
        category: MediaCategory::Other,
        default_on: true,
        description_zh: "BitTorrent 種子檔案 (TORRENT)",
        description_en: "BitTorrent Metadata File (TORRENT)",
    },
    // --- 5. 常見語法與純文字格式 (Syntax & Plain-Text Formats - 預設關閉) ---
    MediaFormatInfo {
        ext: ".json",
        name: "JSON",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "JSON 資料格式 (JSON)",
        description_en: "JSON Data File (JSON)",
    },
    MediaFormatInfo {
        ext: ".csv",
        name: "CSV",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "CSV 逗號分隔值表格 (CSV)",
        description_en: "Comma-Separated Values (CSV)",
    },
    MediaFormatInfo {
        ext: ".html",
        name: "HTML",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "HTML 網頁標記文件 (HTML)",
        description_en: "HTML Document (HTML)",
    },
    MediaFormatInfo {
        ext: ".htm",
        name: "HTM",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "HTM 網頁標記文件 (HTM)",
        description_en: "HTM Document (HTM)",
    },
    MediaFormatInfo {
        ext: ".xml",
        name: "XML",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "XML 可延伸標記文件 (XML)",
        description_en: "XML Document (XML)",
    },
    MediaFormatInfo {
        ext: ".yaml",
        name: "YAML",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "YAML 組態設定檔 (YAML)",
        description_en: "YAML Configuration (YAML)",
    },
    MediaFormatInfo {
        ext: ".yml",
        name: "YML",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "YML 組態設定檔 (YML)",
        description_en: "YML Configuration (YML)",
    },
    MediaFormatInfo {
        ext: ".toml",
        name: "TOML",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "TOML 組態設定檔 (TOML)",
        description_en: "TOML Configuration (TOML)",
    },
    MediaFormatInfo {
        ext: ".ini",
        name: "INI",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "INI 組態設定檔 (INI)",
        description_en: "INI Configuration (INI)",
    },
    MediaFormatInfo {
        ext: ".log",
        name: "LOG",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "LOG 記錄與日誌檔 (LOG)",
        description_en: "Log File (LOG)",
    },
    MediaFormatInfo {
        ext: ".ps1",
        name: "PS1",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "PowerShell 指令碼 (PS1)",
        description_en: "PowerShell Script (PS1)",
    },
    MediaFormatInfo {
        ext: ".cmd",
        name: "CMD",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "Windows 命令指令碼 (CMD)",
        description_en: "Windows Command Script (CMD)",
    },
    MediaFormatInfo {
        ext: ".bat",
        name: "BAT",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "批次檔指令碼 (BAT)",
        description_en: "Batch Script (BAT)",
    },
    MediaFormatInfo {
        ext: ".sh",
        name: "SH",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "Shell 指令碼 (SH)",
        description_en: "Shell Script (SH)",
    },
    MediaFormatInfo {
        ext: ".sql",
        name: "SQL",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "SQL 資料庫查詢指令碼 (SQL)",
        description_en: "SQL Query Script (SQL)",
    },
    MediaFormatInfo {
        ext: ".py",
        name: "PY",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "Python 原始碼 (PY)",
        description_en: "Python Source File (PY)",
    },
    MediaFormatInfo {
        ext: ".js",
        name: "JS",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "JavaScript 原始碼 (JS)",
        description_en: "JavaScript Source File (JS)",
    },
    MediaFormatInfo {
        ext: ".tsx",
        name: "TSX",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "TypeScript JSX 原始碼 (TSX)",
        description_en: "TypeScript JSX Source File (TSX)",
    },
    MediaFormatInfo {
        ext: ".css",
        name: "CSS",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "CSS 階層式樣式表 (CSS)",
        description_en: "Cascading Style Sheets (CSS)",
    },
    MediaFormatInfo {
        ext: ".rs",
        name: "RS",
        category: MediaCategory::Syntax,
        default_on: false,
        description_zh: "Rust 原始碼 (RS)",
        description_en: "Rust Source File (RS)",
    },
];

/// Returns the category of a given extension (case-insensitive, e.g. ".mp4" or "mp4").
pub fn get_extension_category(ext: &str) -> Option<MediaCategory> {
    let clean = if ext.starts_with('.') {
        ext.to_ascii_lowercase()
    } else {
        format!(".{}", ext.to_ascii_lowercase())
    };
    SUPPORTED_MEDIA_FORMATS
        .iter()
        .find(|f| f.ext.eq_ignore_ascii_case(&clean))
        .map(|f| f.category)
}

/// Checks if an extension is a supported video format.
pub fn is_video_extension(ext: &str) -> bool {
    get_extension_category(ext) == Some(MediaCategory::Video)
}

/// Checks if an extension is a supported audio format.
pub fn is_audio_extension(ext: &str) -> bool {
    get_extension_category(ext) == Some(MediaCategory::Audio)
}

/// Checks if an extension is an image archive format.
pub fn is_image_archive_extension(ext: &str) -> bool {
    get_extension_category(ext) == Some(MediaCategory::ImageArchive)
}

/// Checks if an extension is a supported e-book format.
pub fn is_ebook_extension(ext: &str) -> bool {
    get_extension_category(ext) == Some(MediaCategory::Ebook)
}

/// Checks if an extension belongs to the built-in "Other" document/design/text formats.
pub fn is_other_extension(ext: &str) -> bool {
    get_extension_category(ext) == Some(MediaCategory::Other)
}

/// Checks if an extension belongs to the common syntax/plain-text checklist formats.
pub fn is_syntax_extension(ext: &str) -> bool {
    get_extension_category(ext) == Some(MediaCategory::Syntax)
}

/// Checks if an extension is a built-in plain-text, markup, data, or script format
/// supported by the Plain Text previewer without cluttering the UI extension checklist.
pub fn is_builtin_plain_text_extension(ext: &str) -> bool {
    let clean = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    matches!(
        clean.as_str(),
        "json" | "jsonc" | "jsonl" | "csv" | "tsv" | "html" | "htm" | "xhtml" | "xml" | "svg"
        | "ps1" | "psm1" | "psd1" | "cmd" | "bat" | "ini" | "cfg" | "conf" | "toml" | "yaml"
        | "yml" | "log" | "sql" | "sh" | "bash" | "zsh" | "py" | "js" | "mjs" | "cjs" | "ts"
        | "tsx" | "jsx" | "css" | "scss" | "less" | "rs" | "c" | "cpp" | "cc" | "cxx" | "h"
        | "hpp" | "cs" | "java" | "go" | "rb" | "php" | "lua" | "r" | "pl" | "swift" | "kt"
        | "scala" | "reg" | "inf" | "srt" | "ass" | "ssa" | "vtt" | "lrc" | "nfo" | "diz"
        | "tex" | "bib" | "rst" | "adoc" | "org" | "env" | "gitignore" | "gitattributes"
        | "editorconfig" | "dockerfile" | "makefile" | "cmake" | "gradle" | "properties"
        | "vue" | "svelte"
    )
}

/// Checks if an extension matches a custom plain text extension list (without or with leading '.').
pub fn is_custom_text_extension(ext: &str, custom_exts: &[String]) -> bool {
    let clean = ext.trim().trim_start_matches('.').to_ascii_lowercase();
    !clean.is_empty() && custom_exts.iter().any(|c| c.eq_ignore_ascii_case(&clean))
}

/// Checks if an extension is handled by the "Other" pipeline (built-in Other, Syntax, built-in plain text, or custom text extension).
pub fn is_other_or_custom_text_extension(ext: &str, custom_exts: &[String]) -> bool {
    is_other_extension(ext)
        || is_syntax_extension(ext)
        || is_builtin_plain_text_extension(ext)
        || is_custom_text_extension(ext, custom_exts)
}
