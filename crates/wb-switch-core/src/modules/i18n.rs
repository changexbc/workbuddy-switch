//! Locale for strings the host renders outside the webview: tray menu labels,
//! notifications, and update prompts.
//!
//! The webview keeps its own catalog in `src/lib/i18n`, and the frontend tells
//! this module which locale the user picked through the `set_locale` command.
//! Call sites pass both strings, so a missing translation is a compile error
//! rather than a runtime fallback.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::modules::config;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Locale {
    En = 0,
    Ko = 1,
}

impl Locale {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "en" => Some(Self::En),
            "ko" => Some(Self::Ko),
            _ => None,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ko => "ko",
        }
    }
}

impl Default for Locale {
    fn default() -> Self {
        Self::En
    }
}

static ACTIVE: AtomicU8 = AtomicU8::new(Locale::En as u8);

pub fn set_locale(locale: Locale) {
    ACTIVE.store(locale as u8, Ordering::Relaxed);
}

/// The locale survives a restart, so the tray renders in the chosen language
/// before the webview loads.
pub fn locale_file() -> PathBuf {
    config::store_dir().join("locale")
}

pub fn load_locale() -> Locale {
    std::fs::read_to_string(locale_file())
        .ok()
        .and_then(|text| Locale::parse(text.trim()))
        .unwrap_or_default()
}

pub fn save_locale(locale: Locale) -> std::io::Result<()> {
    let dir = config::store_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(locale_file(), locale.code())
}

pub fn locale() -> Locale {
    if ACTIVE.load(Ordering::Relaxed) == Locale::Ko as u8 {
        Locale::Ko
    } else {
        Locale::En
    }
}

/// Picks the string for the active locale.
pub fn tr(en: &'static str, ko: &'static str) -> &'static str {
    match locale() {
        Locale::En => en,
        Locale::Ko => ko,
    }
}

/// Picks an already-built string for the active locale. Use this when the
/// message needs interpolation.
pub fn pick(en: String, ko: String) -> String {
    match locale() {
        Locale::En => en,
        Locale::Ko => ko,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_codes_only() {
        assert_eq!(Locale::parse("en"), Some(Locale::En));
        assert_eq!(Locale::parse("ko"), Some(Locale::Ko));
        assert_eq!(Locale::parse("zh"), None);
    }

    #[test]
    fn tr_follows_the_active_locale() {
        set_locale(Locale::En);
        assert_eq!(tr("Open", "열기"), "Open");
        set_locale(Locale::Ko);
        assert_eq!(tr("Open", "열기"), "열기");
        set_locale(Locale::En);
    }
}
