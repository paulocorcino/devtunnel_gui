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
        let source = ftl_source(lang);
        let res = FluentResource::try_new(source.to_string()).expect("embedded FTL must be valid");
        let langid: LanguageIdentifier = lang
            .parse()
            .unwrap_or_else(|_| "en-US".parse().expect("en-US is valid"));
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

fn ftl_source(_lang: &str) -> &'static str {
    // Add new locales here; unknown tags fall back to en-US.
    include_str!("../i18n/en-US/app.ftl")
}
