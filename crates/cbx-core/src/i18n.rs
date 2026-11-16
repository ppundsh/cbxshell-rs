use std::collections::HashMap;
use std::path::Path;
use std::sync::{OnceLock, RwLock};

use crate::settings::UiLang;

const EMBEDDED_ZH: &str = include_str!("../../cbx-manager/locales/zh-TW.json");
const EMBEDDED_EN: &str = include_str!("../../cbx-manager/locales/en-US.json");

static CURRENT_LOCALE_MAP: RwLock<Option<HashMap<String, String>>> = RwLock::new(None);
static BASE_EN_MAP: OnceLock<HashMap<String, String>> = OnceLock::new();
static BASE_ZH_MAP: OnceLock<HashMap<String, String>> = OnceLock::new();

/// Minimal, robust JSON string key-value parser (no external crates required).
pub fn parse_flat_json(input: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut idx = 0;

    while idx < len {
        while idx < len && chars[idx] != '"' {
            idx += 1;
        }
        if idx >= len {
            break;
        }
        idx += 1;

        let mut key = String::new();
        while idx < len {
            let ch = chars[idx];
            if ch == '\\' && idx + 1 < len {
                idx += 1;
                match chars[idx] {
                    '"' => key.push('"'),
                    '\\' => key.push('\\'),
                    'n' => key.push('\n'),
                    'r' => key.push('\r'),
                    't' => key.push('\t'),
                    other => key.push(other),
                }
            } else if ch == '"' {
                idx += 1;
                break;
            } else {
                key.push(ch);
            }
            idx += 1;
        }

        while idx < len && chars[idx] != ':' {
            idx += 1;
        }
        if idx >= len {
            break;
        }
        idx += 1;

        while idx < len && chars[idx] != '"' {
            idx += 1;
        }
        if idx >= len {
            break;
        }
        idx += 1;

        let mut val = String::new();
        while idx < len {
            let ch = chars[idx];
            if ch == '\\' && idx + 1 < len {
                idx += 1;
                match chars[idx] {
                    '"' => val.push('"'),
                    '\\' => val.push('\\'),
                    'n' => val.push('\n'),
                    'r' => val.push('\r'),
                    't' => val.push('\t'),
                    other => val.push(other),
                }
            } else if ch == '"' {
                idx += 1;
                break;
            } else {
                val.push(ch);
            }
            idx += 1;
        }

        if !key.is_empty() {
            map.insert(key, val);
        }
    }

    map
}

fn en_map() -> &'static HashMap<String, String> {
    BASE_EN_MAP.get_or_init(|| parse_flat_json(EMBEDDED_EN))
}

fn zh_map() -> &'static HashMap<String, String> {
    BASE_ZH_MAP.get_or_init(|| parse_flat_json(EMBEDDED_ZH))
}

/// Initializes the active locale dictionary, optionally overlaying external JSON files from `locales_dir`.
pub fn init_locale_with_dir(lang: UiLang, locales_dir: Option<&Path>) {
    let mut map = en_map().clone();

    if let UiLang::ZhTw = lang {
        for (k, v) in zh_map() {
            if !v.trim().is_empty() {
                map.insert(k.clone(), v.clone());
            }
        }
    }

    if let Some(dir) = locales_dir {
        let file_name = match lang {
            UiLang::ZhTw => "zh-TW.json",
            UiLang::En => "en-US.json",
        };
        let external_path = dir.join(file_name);
        if external_path.is_file() {
            if let Ok(content) = std::fs::read_to_string(&external_path) {
                let external_map = parse_flat_json(&content);
                for (k, v) in external_map {
                    if !v.trim().is_empty() {
                        map.insert(k, v);
                    }
                }
            }
        }
    }

    if let Ok(mut lock) = CURRENT_LOCALE_MAP.write() {
        *lock = Some(map);
    }
}

/// Initializes the active locale using the current `UiLang` (detected from settings or system locale).
pub fn init_locale(lang: UiLang) {
    init_locale_with_dir(lang, None);
}

fn ensure_initialized() {
    let needs_init = CURRENT_LOCALE_MAP
        .read()
        .map(|lock| lock.is_none())
        .unwrap_or(false);
    if needs_init {
        init_locale(UiLang::load_or_detect());
    }
}

/// Looks up a localized string for the active UI language, falling back to English and then `key`.
pub fn t(key: &str) -> String {
    ensure_initialized();
    if let Ok(lock) = CURRENT_LOCALE_MAP.read() {
        if let Some(ref map) = *lock {
            if let Some(val) = map.get(key) {
                if !val.trim().is_empty() {
                    return val.clone();
                }
            }
        }
    }
    if let Some(val) = en_map().get(key) {
        if !val.trim().is_empty() {
            return val.clone();
        }
    }
    key.to_string()
}

/// Looks up a localized string for an explicit `UiLang` from the embedded locale dictionaries.
pub fn t_for(lang: UiLang, key: &str) -> String {
    if let UiLang::ZhTw = lang {
        if let Some(val) = zh_map().get(key) {
            if !val.trim().is_empty() {
                return val.clone();
            }
        }
    }
    if let Some(val) = en_map().get(key) {
        if !val.trim().is_empty() {
            return val.clone();
        }
    }
    key.to_string()
}

/// Looks up a localized string and replaces `{name}` placeholders.
pub fn t_fmt(key: &str, args: &[(&str, &str)]) -> String {
    let mut out = t(key);
    for (k, v) in args {
        let pat = format!("{{{k}}}");
        out = out.replace(&pat, v);
    }
    out
}

/// Looks up a localized string for an explicit `UiLang` and replaces `{name}` placeholders.
pub fn t_for_fmt(lang: UiLang, key: &str, args: &[(&str, &str)]) -> String {
    let mut out = t_for(lang, key);
    for (k, v) in args {
        let pat = format!("{{{k}}}");
        out = out.replace(&pat, v);
    }
    out
}
