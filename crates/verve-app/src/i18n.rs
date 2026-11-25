//! Application localization helpers backed by `rust-i18n` (`zh-CN` / `en-US`).

/// Supported desktop UI locales for Verve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AppLocale {
    /// Simplified Chinese (`zh-CN`).
    #[default]
    ZhCn,
    /// American English (`en-US`).
    EnUs,
}

impl AppLocale {
    /// Parses a persisted configuration locale string (case-insensitive).
    pub fn from_config_str(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
        match normalized.as_str() {
            "zh" | "zh-cn" => Some(Self::ZhCn),
            "en" | "en-us" => Some(Self::EnUs),
            _ => None,
        }
    }

    /// Returns the canonical BCP 47 tag persisted in `settings.toml`.
    pub const fn as_config_str(self) -> &'static str {
        match self {
            Self::ZhCn => "zh-CN",
            Self::EnUs => "en-US",
        }
    }
}

/// Applies `raw` (defaulting to `zh-CN` when unrecognized) to `rust_i18n` globally.
pub fn apply_locale(raw: &str) -> AppLocale {
    let locale = AppLocale::from_config_str(raw).unwrap_or_default();
    rust_i18n::set_locale(locale.as_config_str());
    locale
}

/// Returns the currently active [`AppLocale`] from `rust_i18n::locale()`.
pub fn current_locale() -> AppLocale {
    AppLocale::from_config_str(&rust_i18n::locale()).unwrap_or_default()
}
