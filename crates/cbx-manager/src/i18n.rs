use std::path::PathBuf;

pub fn init_locale(lang: cbx_core::settings::UiLang) {
    let locales_dir: Option<PathBuf> = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("locales")));
    cbx_core::i18n::init_locale_with_dir(lang, locales_dir.as_deref());
}

pub fn t(key: &str) -> String {
    cbx_core::i18n::t(key)
}

#[allow(dead_code)]
pub fn t_fmt(key: &str, fallback: &str) -> String {
    let res = cbx_core::i18n::t(key);
    if res == key {
        fallback.to_string()
    } else {
        res
    }
}
