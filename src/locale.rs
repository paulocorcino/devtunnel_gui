//! Locale bundle backed by Fluent (FTL) files embedded at compile time.
//!
//! Usage:
//!   let loc = Locale::load("en-US");
//!   loc.t("btn-refresh")                     // → "Refresh"
//!   loc.t_args("status-port-count", args![count: 3])  // → "3 ports"

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource};
use unic_langid::LanguageIdentifier;

pub struct Locale {
    bundle: FluentBundle<FluentResource>,
}

impl Locale {
    /// Loads the bundle for `lang` (e.g. `"en-US"`).
    /// Unknown locales fall back to `en-US`.
    pub fn load(lang: &str) -> Self {
        // The bundle's langid must match the FTL we actually load, not the raw
        // request: it drives Fluent's CLDR plural selection. Loading en-US text
        // under, say, a pt-BR langid applies Portuguese plural rules to English
        // patterns — and pt classifies 0 as `one`, so `status-port-count` with
        // count 0 wrongly rendered the `[one]` branch ("1 port") for "0 ports".
        let resolved = resolve_lang(lang);
        let source = ftl_source(resolved);
        let res = FluentResource::try_new(source.to_string()).expect("embedded FTL must be valid");
        let langid: LanguageIdentifier =
            resolved.parse().expect("resolved locale tag must be valid");
        let mut bundle = FluentBundle::new(vec![langid]);
        bundle
            .add_resource(res)
            .expect("embedded FTL must have no duplicate messages");
        Locale { bundle }
    }

    /// Translates `key` without arguments. Panics on missing key in debug; returns key in release.
    pub fn t(&self, key: &str) -> String {
        self.format(key, None)
    }

    /// Translates `key` with Fluent arguments.
    ///
    /// ```ignore
    /// use fluent_bundle::FluentArgs;
    /// let mut args = FluentArgs::new();
    /// args.set("count", 5);
    /// loc.t_args("status-port-count", &args)
    /// ```
    pub fn t_args(&self, key: &str, args: &FluentArgs) -> String {
        self.format(key, Some(args))
    }

    fn format(&self, key: &str, args: Option<&FluentArgs>) -> String {
        let Some(msg) = self.bundle.get_message(key) else {
            debug_assert!(false, "missing i18n key: {key}");
            return key.to_string();
        };
        let Some(pattern) = msg.value() else {
            debug_assert!(false, "i18n key has no value: {key}");
            return key.to_string();
        };
        let mut errors = vec![];
        let result = self.bundle.format_pattern(pattern, args, &mut errors);
        debug_assert!(
            errors.is_empty(),
            "i18n format errors for {key}: {errors:?}"
        );
        result.to_string()
    }
}

/// Detects the system locale, returning a BCP-47 tag (e.g. `"en-US"`).
/// Override with the `DEVTUNNEL_LANG` environment variable.
pub fn system_locale() -> String {
    if let Ok(v) = std::env::var("DEVTUNNEL_LANG") {
        return v;
    }
    sys_locale::get_locale().unwrap_or_else(|| "en-US".to_string())
}

/// Resolves a requested BCP-47 tag to the locale we actually ship strings for,
/// so the loaded FTL and the bundle's langid (hence its plural rules) always
/// agree. Until more locales ship, every request resolves to en-US. Add an arm
/// here in lockstep with [`ftl_source`] when adding a locale.
fn resolve_lang(_lang: &str) -> &'static str {
    // e.g. "pt-BR" | "pt" => "pt-BR",
    "en-US"
}

fn ftl_source(lang: &str) -> &'static str {
    // `lang` is already a resolved tag from [`resolve_lang`].
    match lang {
        // "pt-BR" => include_str!("../i18n/pt-BR/app.ftl"),
        _ => include_str!("../i18n/en-US/app.ftl"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_args(n: i64) -> FluentArgs<'static> {
        let mut args = FluentArgs::new();
        args.set("count", n);
        args
    }

    /// Strips the bidi isolation marks (FSI/PDI) Fluent wraps around interpolated
    /// args; they are invisible in the UI but would break literal comparisons.
    fn plain(s: String) -> String {
        s.replace(['\u{2068}', '\u{2069}'], "")
    }

    #[test]
    fn port_count_uses_english_plural_rules() {
        // en-US: 0 and 2+ are "other", only 1 is "one".
        let loc = Locale::load("en-US");
        assert_eq!(
            plain(loc.t_args("status-port-count", &count_args(0))),
            "0 ports"
        );
        assert_eq!(
            plain(loc.t_args("status-port-count", &count_args(1))),
            "1 port"
        );
        assert_eq!(
            plain(loc.t_args("status-port-count", &count_args(3))),
            "3 ports"
        );
    }

    #[test]
    fn pt_br_request_does_not_misplural_english_text() {
        // Regression: a pt-BR system locale loaded en-US strings under a pt-BR
        // langid, and pt classifies 0 as `one` — so "0 ports" rendered as the
        // `[one]` branch ("1 port"). The bundle must use the resolved (en-US)
        // langid so plural rules match the loaded text.
        let loc = Locale::load("pt-BR");
        assert_eq!(
            plain(loc.t_args("status-port-count", &count_args(0))),
            "0 ports"
        );
    }
}
