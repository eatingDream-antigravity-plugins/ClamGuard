use crate::platform::macos::run_cmd_timeout;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    ZhCn,
}

impl Lang {
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::ZhCn => "zh-CN",
        }
    }
}

/// Parse a locale tag (`en`, `en_US.UTF-8`, `zh`, `zh-CN`, `zh-Hans-CN`, `C`, etc.).
/// Returns `None` for empty strings or `"auto"`. Non-`zh` locales resolve to `Lang::En`.
pub fn parse_lang_tag(raw: &str) -> Option<Lang> {
    let cleaned = raw.trim().trim_matches('"').trim_matches('\'').trim();
    if cleaned.is_empty() || cleaned.eq_ignore_ascii_case("auto") {
        return None;
    }
    let low = cleaned.to_ascii_lowercase();
    if low == "zh" || low.starts_with("zh-") || low.starts_with("zh_") || low.starts_with("zh.") {
        Some(Lang::ZhCn)
    } else {
        Some(Lang::En)
    }
}

/// Normalize a user-configured `settings.json` `lang` value.
/// Returns `None` for `"auto"` or empty (meaning clear override), `Some("zh-CN")` or `Some("en")` for valid tags.
pub fn normalize_setting_lang(raw: &str) -> Option<String> {
    parse_lang_tag(raw).map(|l| l.code().to_string())
}

/// Parse `defaults read -g AppleLanguages` array output (first language entry wins).
pub fn parse_apple_languages_output(out: &str) -> Option<Lang> {
    for line in out.lines() {
        let token = line
            .trim()
            .trim_start_matches('(')
            .trim_end_matches(')')
            .trim_end_matches(',')
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim();
        if token.is_empty() {
            continue;
        }
        if let Some(lang) = parse_lang_tag(token) {
            return Some(lang);
        }
    }
    None
}

fn macos_apple_languages_fallback() -> Option<Lang> {
    static CACHED: OnceLock<Option<Lang>> = OnceLock::new();
    *CACHED.get_or_init(|| {
        #[cfg(target_os = "macos")]
        {
            let (ok, out) =
                run_cmd_timeout("/usr/bin/defaults", &["read", "-g", "AppleLanguages"], 400);
            if ok {
                return parse_apple_languages_output(&out);
            }
            None
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    })
}

pub fn resolve_lang_with_env<F>(
    settings_lang: Option<&str>,
    get_env: F,
    apple_fallback: fn() -> Option<Lang>,
) -> Lang
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(s) = settings_lang.and_then(parse_lang_tag) {
        return s;
    }
    for key in ["MAC_CLAMSHELL_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(val) = get_env(key) {
            if let Some(lang) = parse_lang_tag(&val) {
                return lang;
            }
        }
    }
    apple_fallback().unwrap_or(Lang::En)
}

pub fn detect_lang(settings_lang: Option<&str>) -> Lang {
    resolve_lang_with_env(
        settings_lang,
        |k| std::env::var(k).ok(),
        macos_apple_languages_fallback,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn locale_resolution_chain_and_apple_languages_parsing() {
        let apple_zh = || Some(Lang::ZhCn);
        let apple_en = || Some(Lang::En);

        // 1. Settings override wins over env and AppleLanguages
        let mut env = HashMap::new();
        env.insert("MAC_CLAMSHELL_LANG".to_string(), "en_US.UTF-8".to_string());
        env.insert("LANG".to_string(), "en_US.UTF-8".to_string());
        assert_eq!(
            resolve_lang_with_env(Some("zh-CN"), |k| env.get(k).cloned(), apple_en),
            Lang::ZhCn
        );

        // 2. MAC_CLAMSHELL_LANG wins over LC_ALL / LANG
        env.insert("MAC_CLAMSHELL_LANG".to_string(), "zh_CN.UTF-8".to_string());
        env.insert("LC_ALL".to_string(), "en_US.UTF-8".to_string());
        assert_eq!(
            resolve_lang_with_env(None, |k| env.get(k).cloned(), apple_en),
            Lang::ZhCn
        );

        // 3. LC_ALL -> LC_MESSAGES -> LANG -> non-zh defaults to En
        env.remove("MAC_CLAMSHELL_LANG");
        env.insert("LC_ALL".to_string(), "fr_FR.UTF-8".to_string());
        assert_eq!(
            resolve_lang_with_env(None, |k| env.get(k).cloned(), apple_zh),
            Lang::En
        );

        env.remove("LC_ALL");
        env.insert("LANG".to_string(), "zh-Hans-CN".to_string());
        assert_eq!(
            resolve_lang_with_env(None, |k| env.get(k).cloned(), apple_en),
            Lang::ZhCn
        );

        // 4. Empty env falls back to AppleLanguages
        env.clear();
        assert_eq!(
            resolve_lang_with_env(Some("auto"), |k| env.get(k).cloned(), apple_zh),
            Lang::ZhCn
        );

        // 5. AppleLanguages parser
        let sample = "(\n    \"zh-Hans-CN\",\n    \"en-US\"\n)\n";
        assert_eq!(parse_apple_languages_output(sample), Some(Lang::ZhCn));
        let sample_en = "(\n    \"en-GB\",\n    \"zh-Hans-CN\"\n)\n";
        assert_eq!(parse_apple_languages_output(sample_en), Some(Lang::En));
    }
}
