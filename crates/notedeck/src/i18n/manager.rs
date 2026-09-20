use super::{IntlError, IntlKeyBuf};
use fluent::{FluentArgs, FluentBundle, FluentResource};
use fluent_langneg::negotiate_languages;
use std::collections::HashMap;
use unic_langid::{langid, LanguageIdentifier};

const EN_US: LanguageIdentifier = langid!("en-US");
const EN_XA: LanguageIdentifier = langid!("en-XA");
const DE: LanguageIdentifier = langid!("de");
const ES_419: LanguageIdentifier = langid!("es-419");
const ES_ES: LanguageIdentifier = langid!("es-ES");
const FR: LanguageIdentifier = langid!("fr");
const JA: LanguageIdentifier = langid!("ja");
const PT_BR: LanguageIdentifier = langid!("pt-BR");
const PT_PT: LanguageIdentifier = langid!("pt-PT");
const TH: LanguageIdentifier = langid!("th");
const ZH_CN: LanguageIdentifier = langid!("zh-CN");
const ZH_TW: LanguageIdentifier = langid!("zh-TW");
const NUM_FTLS: usize = 12;

const EN_US_NATIVE_NAME: &str = "English (US)";
const EN_XA_NATIVE_NAME: &str = "Éñglísh (Pséúdólóçàlé)";
const DE_NATIVE_NAME: &str = "Deutsch";
const ES_419_NATIVE_NAME: &str = "Español (Latinoamérica)";
const ES_ES_NATIVE_NAME: &str = "Español (España)";
const FR_NATIVE_NAME: &str = "Français";
const JA_NATIVE_NAME: &str = "日本語";
const PT_BR_NATIVE_NAME: &str = "Português (Brasil)";
const PT_PT_NATIVE_NAME: &str = "Português (Portugal)";
const TH_NATIVE_NAME: &str = "ภาษาไทย";
const ZH_CN_NATIVE_NAME: &str = "简体中文";
const ZH_TW_NATIVE_NAME: &str = "繁體中文";

struct StaticBundle {
    identifier: LanguageIdentifier,
    ftl: &'static str,
}

const FTLS: [StaticBundle; NUM_FTLS] = [
    StaticBundle {
        identifier: EN_US,
        ftl: include_str!("../../../../assets/translations/en-US/main.ftl"),
    },
    StaticBundle {
        identifier: EN_XA,
        ftl: include_str!("../../../../assets/translations/en-XA/main.ftl"),
    },
    StaticBundle {
        identifier: DE,
        ftl: include_str!("../../../../assets/translations/de/main.ftl"),
    },
    StaticBundle {
        identifier: ES_419,
        ftl: include_str!("../../../../assets/translations/es-419/main.ftl"),
    },
    StaticBundle {
        identifier: ES_ES,
        ftl: include_str!("../../../../assets/translations/es-ES/main.ftl"),
    },
    StaticBundle {
        identifier: FR,
        ftl: include_str!("../../../../assets/translations/fr/main.ftl"),
    },
    StaticBundle {
        identifier: JA,
        ftl: include_str!("../../../../assets/translations/ja/main.ftl"),
    },
    StaticBundle {
        identifier: PT_BR,
        ftl: include_str!("../../../../assets/translations/pt-BR/main.ftl"),
    },
    StaticBundle {
        identifier: PT_PT,
        ftl: include_str!("../../../../assets/translations/pt-PT/main.ftl"),
    },
    StaticBundle {
        identifier: TH,
        ftl: include_str!("../../../../assets/translations/th/main.ftl"),
    },
    StaticBundle {
        identifier: ZH_CN,
        ftl: include_str!("../../../../assets/translations/zh-CN/main.ftl"),
    },
    StaticBundle {
        identifier: ZH_TW,
        ftl: include_str!("../../../../assets/translations/zh-TW/main.ftl"),
    },
];

type Bundle = FluentBundle<FluentResource>;

/// Manages localization resources and provides localized strings
pub struct Localization {
    /// Current locale
    current_locale: LanguageIdentifier,
    /// Available locales
    available_locales: Vec<LanguageIdentifier>,
    /// Fallback locale
    fallback_locale: LanguageIdentifier,
    /// Native names for locales
    locale_native_names: HashMap<LanguageIdentifier, String>,

    /// Cached string results per locale (only for strings without arguments)
    string_cache: HashMap<LanguageIdentifier, HashMap<String, String>>,
    /// Cached normalized keys
    normalized_key_cache: HashMap<String, IntlKeyBuf>,
    /// Bundles
    bundles: HashMap<LanguageIdentifier, Bundle>,

    /// Bumped whenever anything that would change a translation's text changes
    /// — currently the locale. Lets a cache built on top of this one
    /// (e.g. [`RelativeTimeCache`](crate::RelativeTimeCache)) notice it holds
    /// strings from the wrong locale without watching `set_locale` itself.
    cache_generation: u64,

    use_isolating: bool,
}

impl Default for Localization {
    fn default() -> Self {
        // Default to English (US)
        let default_locale = &EN_US;
        let fallback_locale = default_locale.to_owned();

        // Build available locales list
        let available_locales = vec![
            EN_US.clone(),
            EN_XA.clone(),
            DE.clone(),
            ES_419.clone(),
            ES_ES.clone(),
            FR.clone(),
            JA.clone(),
            PT_BR.clone(),
            PT_PT.clone(),
            TH.clone(),
            ZH_CN.clone(),
            ZH_TW.clone(),
        ];

        let locale_native_names = HashMap::from([
            (EN_US, EN_US_NATIVE_NAME.to_owned()),
            (EN_XA, EN_XA_NATIVE_NAME.to_owned()),
            (DE, DE_NATIVE_NAME.to_owned()),
            (ES_419, ES_419_NATIVE_NAME.to_owned()),
            (ES_ES, ES_ES_NATIVE_NAME.to_owned()),
            (FR, FR_NATIVE_NAME.to_owned()),
            (JA, JA_NATIVE_NAME.to_owned()),
            (PT_BR, PT_BR_NATIVE_NAME.to_owned()),
            (PT_PT, PT_PT_NATIVE_NAME.to_owned()),
            (TH, TH_NATIVE_NAME.to_owned()),
            (ZH_CN, ZH_CN_NATIVE_NAME.to_owned()),
            (ZH_TW, ZH_TW_NATIVE_NAME.to_owned()),
        ]);

        Self {
            current_locale: default_locale.to_owned(),
            available_locales,
            fallback_locale,
            locale_native_names,
            use_isolating: true,
            normalized_key_cache: HashMap::new(),
            string_cache: HashMap::new(),
            bundles: HashMap::new(),
            cache_generation: 0,
        }
    }
}

impl Localization {
    /// Creates a new Localization with the specified resource directory
    pub fn new() -> Self {
        Localization::default()
    }

    /// Disable bidirectional isolation markers. mostly useful for tests
    pub fn no_bidi() -> Self {
        Localization {
            use_isolating: false,
            ..Localization::default()
        }
    }

    /// Translates a source `message` into the current locale, normalizing it
    /// to its FTL key on the way.
    ///
    /// `args` are the `tr!` interpolation arguments, and `None` means there are
    /// none. Returns `None` when the current bundle has no value for the key,
    /// which is the caller's cue to fall back to the untranslated `message`.
    ///
    /// This does the whole lookup in one call rather than handing the caller a
    /// normalized key to look up itself, because that is what it takes for the
    /// cached case to allocate nothing: the normalized key lives in a map owned
    /// by `self`, so a caller that receives it and then calls another
    /// `&mut self` method has to be given a clone. `tr!` runs 42 times in a
    /// single Columns timeline frame, and that clone was 42 allocations a frame
    /// (measured by `notedeck_columns`'s `frame_alloc` test).
    pub fn translate(
        &mut self,
        message: &str,
        comment: &str,
        args: Option<&FluentArgs>,
    ) -> Option<String> {
        // Strings with arguments are never cached — see `format_uncached` —
        // so there is nothing to look for.
        if args.is_none() {
            if let Some(cached) = self.cached_translation(message) {
                return Some(cached.to_owned());
            }
        }

        self.format_uncached(message, comment, args)
    }

    /// Load a fluent bundle given a language identifier. Only looks in the static
    /// ftl files baked into the binary
    fn load_bundle(lang: &LanguageIdentifier) -> Result<Bundle, IntlError> {
        for ftl in &FTLS {
            if &ftl.identifier == lang {
                let mut bundle = FluentBundle::new(vec![lang.to_owned()]);
                let resource = FluentResource::try_new(ftl.ftl.to_string());
                match resource {
                    Err((resource, errors)) => {
                        for error in errors {
                            tracing::error!("load_bundle ({lang}): {error}");
                        }

                        tracing::warn!("load_bundle ({}: loading bundle with errors", lang);
                        if let Err(errs) = bundle.add_resource(resource) {
                            for err in errs {
                                tracing::error!("adding resource: {err}");
                            }
                        }
                    }

                    Ok(resource) => {
                        tracing::info!("loaded {} bundle OK!", lang);
                        if let Err(errs) = bundle.add_resource(resource) {
                            for err in errs {
                                tracing::error!("adding resource 2: {err}");
                            }
                        }
                    }
                }

                return Ok(bundle);
            }
        }

        // no static ftl for this LanguageIdentifier
        Err(IntlError::NoFtl(lang.to_owned()))
    }

    fn get_bundle<'a>(&'a self, lang: &LanguageIdentifier) -> &'a Bundle {
        self.bundles
            .get(lang)
            .expect("make sure to call ensure_bundle!")
    }

    fn has_bundle(&self, lang: &LanguageIdentifier) -> bool {
        self.bundles.contains_key(lang)
    }

    fn try_load_bundle(&mut self, lang: &LanguageIdentifier) -> Result<(), IntlError> {
        let mut bundle = Self::load_bundle(lang)?;
        if !self.use_isolating {
            bundle.set_use_isolating(false);
        }
        self.bundles.insert(lang.to_owned(), bundle);
        Ok(())
    }

    /// The already-translated case: two map lookups and no allocation at all.
    ///
    /// Deliberately `&self`. Everything it touches is already in the two caches,
    /// so it can hand back a borrow of the cached translation and leave it to
    /// the caller to decide whether it needs an owned `String`. Both maps are
    /// keyed by `String` and probed by `&str`, so nothing is built to probe
    /// them either.
    fn cached_translation(&self, message: &str) -> Option<&str> {
        let key = self.normalized_key_cache.get(message)?;
        let locale_cache = self.string_cache.get(&self.current_locale)?;
        locale_cache.get(key.as_str()).map(String::as_str)
    }

    fn insert_ftl_key(&mut self, cache_key: &str, comment: &str) {
        let mut result = fixup_key(cache_key);

        // Ensure the key starts with a letter (Fluent requirement)
        if result.is_empty() || !result.chars().next().unwrap().is_ascii_alphabetic() {
            result = format!("k_{result}");
        }

        // If we have a comment, append a hash of it to reduce collisions
        let hash_str = format!("_{}", simple_hash(comment));
        result.push_str(&hash_str);

        tracing::debug!(
            "normalize_ftl_key: original='{}', final='{}'",
            cache_key,
            result
        );

        self.normalized_key_cache
            .insert(cache_key.to_owned(), IntlKeyBuf::new(result));
    }

    fn ensure_bundle(&mut self) -> Result<(), IntlError> {
        let locale = self.current_locale.clone();
        if !self.has_bundle(&locale) {
            match self.try_load_bundle(&locale) {
                Err(err) => {
                    tracing::warn!(
                        "tried to load bundle {} but failed with '{err}'. using fallback {}",
                        &locale,
                        &self.fallback_locale
                    );
                    self.try_load_bundle(&locale)
                        .expect("failed to load fallback bundle!?");

                    Ok(())
                }

                Ok(()) => Ok(()),
            }
        } else {
            Ok(())
        }
    }

    fn get_current_bundle(&self) -> &Bundle {
        if self.has_bundle(&self.current_locale) {
            return self.get_bundle(&self.current_locale);
        }

        self.get_bundle(&self.fallback_locale)
    }

    /// Formats `message` through the current bundle, normalizing and caching
    /// its key on the way, and caching the result if it has no arguments.
    ///
    /// Split out from [`Localization::translate`] so the cached path stays the
    /// short `&self` [`Localization::cached_translation`]; this half needs
    /// `&mut self` to fill both caches.
    ///
    /// It is not purely cold: a `tr!` whose key is missing from the bundle is
    /// never cached, so it lands here every frame. That path allocates nothing
    /// either — the `None` it returns is the fallback signal, and building an
    /// error to describe it would be an allocation per frame per such string.
    fn format_uncached(
        &mut self,
        message: &str,
        comment: &str,
        args: Option<&FluentArgs>,
    ) -> Option<String> {
        if let Err(err) = self.ensure_bundle() {
            tracing::error!("no bundle for {}: {err}", &self.current_locale);
            return None;
        }

        if !self.normalized_key_cache.contains_key(message) {
            self.insert_ftl_key(message, comment);
        }

        let formatted = {
            let key = self.normalized_key_cache.get(message)?;
            let bundle = self.get_current_bundle();
            let pattern = bundle.get_message(key.as_str())?.value()?;

            let mut errors = Vec::with_capacity(0);
            let formatted = bundle.format_pattern(pattern, args, &mut errors);

            if !errors.is_empty() {
                tracing::warn!("Localization errors for {}: {:?}", key, &errors);
            }

            formatted.into_owned()
        };

        // Only strings without arguments are cached: the same message id
        // formatted with different arguments has different results.
        if args.is_none() {
            self.cache_translation(message, &formatted);
        }

        Some(formatted)
    }

    /// Records `formatted` as the current locale's translation of `message`.
    ///
    /// Keyed by the normalized FTL key rather than by `message`, so that
    /// [`Localization::cached_translation`] finds it through the same two
    /// lookups it would do anyway.
    fn cache_translation(&mut self, message: &str, formatted: &str) {
        // Owned because it is about to be a map key, not to end the borrow of
        // `self` — though it does that too.
        let Some(key) = self.normalized_key_cache.get(message) else {
            return;
        };
        let key = key.as_str().to_owned();
        let locale = self.current_locale.clone();

        tracing::debug!("Cached string result for '{key}' in locale: {locale}");

        self.string_cache
            .entry(locale)
            .or_default()
            .insert(key, formatted.to_owned());
    }

    /// Sets the current locale
    pub fn set_locale(&mut self, locale: LanguageIdentifier) -> Result<(), IntlError> {
        tracing::info!("Attempting to set locale to: {}", locale);
        tracing::info!("Available locales: {:?}", self.available_locales);

        // Validate that the locale is available
        if !self.available_locales.contains(&locale) {
            tracing::error!(
                "Locale {} is not available. Available locales: {:?}",
                locale,
                self.available_locales
            );
            return Err(IntlError::LocaleNotAvailable(locale));
        }

        tracing::info!(
            "Switching locale from {} to {}",
            &self.current_locale,
            &locale
        );
        self.current_locale = locale;

        // Clear caches when locale changes since they are locale-specific
        self.string_cache.clear();
        self.cache_generation += 1;
        tracing::debug!("String cache cleared due to locale change");

        Ok(())
    }

    /// Clears the parsed FluentResource cache (useful for development when FTL files change)
    pub fn clear_cache(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.bundles.clear();
        tracing::debug!("Parsed FluentResource cache cleared");

        self.string_cache.clear();
        self.cache_generation += 1;
        tracing::debug!("String result cache cleared");

        Ok(())
    }

    /// A counter that moves whenever translated text this manager already
    /// handed out could have become wrong — currently, a locale change or a
    /// bundle reload.
    ///
    /// Downstream caches keyed on something other than the locale (the note
    /// header's [`RelativeTimeCache`](crate::RelativeTimeCache) is keyed on the
    /// time bucket) compare this against the value they were built at and clear
    /// when it moves.
    pub fn cache_generation(&self) -> u64 {
        self.cache_generation
    }

    /// Gets the current locale
    pub fn get_current_locale(&self) -> &LanguageIdentifier {
        &self.current_locale
    }

    /// Gets all available locales
    pub fn get_available_locales(&self) -> &[LanguageIdentifier] {
        &self.available_locales
    }

    /// Gets the fallback locale
    pub fn get_fallback_locale(&self) -> &LanguageIdentifier {
        &self.fallback_locale
    }

    pub fn get_locale_native_name(&self, locale: &LanguageIdentifier) -> Option<&str> {
        self.locale_native_names.get(locale).map(|s| s.as_str())
    }

    /// Gets cache statistics for monitoring performance
    pub fn get_cache_stats(&self) -> Result<CacheStats, Box<dyn std::error::Error + Send + Sync>> {
        let mut total_strings = 0;
        for locale_cache in self.string_cache.values() {
            total_strings += locale_cache.len();
        }

        Ok(CacheStats {
            resource_cache_size: self.bundles.len(),
            string_cache_size: total_strings,
            cached_locales: self.bundles.keys().cloned().collect(),
        })
    }

    /// Limits the string cache size to prevent memory growth
    pub fn limit_string_cache_size(
        &mut self,
        max_strings_per_locale: usize,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for locale_cache in self.string_cache.values_mut() {
            if locale_cache.len() > max_strings_per_locale {
                // Remove oldest entries (simple approach: just clear and let it rebuild)
                // In a more sophisticated implementation, you might use an LRU cache
                locale_cache.clear();
                tracing::debug!("Cleared string cache for locale due to size limit");
            }
        }

        Ok(())
    }

    /// Negotiates the best locale from a list of preferred locales
    pub fn negotiate_locale(&self, preferred: &[LanguageIdentifier]) -> LanguageIdentifier {
        let available = self.available_locales.clone();
        let negotiated = negotiate_languages(
            preferred,
            &available,
            Some(&self.fallback_locale),
            fluent_langneg::NegotiationStrategy::Filtering,
        );
        negotiated
            .first()
            .map_or(self.fallback_locale.clone(), |v| (*v).clone())
    }
}

/// Statistics about cache usage
#[derive(Debug, Clone)]
pub struct CacheStats {
    pub resource_cache_size: usize,
    pub string_cache_size: usize,
    pub cached_locales: Vec<LanguageIdentifier>,
}

/// Replace each invalid character with exactly one underscore
/// This matches the behavior of the Python extraction script
pub fn fixup_key(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' => out.push(ch),
            _ => out.push('_'), // always push
        }
    }
    let trimmed = out.trim_matches('_');
    trimmed.to_owned()
}

fn simple_hash(s: &str) -> String {
    let digest = md5::compute(s.as_bytes());
    // Take the first 2 bytes and convert to 4 hex characters
    format!("{:02x}{:02x}", digest[0], digest[1])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message that really is in `assets/translations/en-US/main.ftl`,
    /// together with the comment its key was generated from. The pair has to
    /// match a live `tr!` call site, because the key is a function of both.
    const KNOWN: (&str, &str) = ("Reply", "Column title for reply composition");

    #[test]
    fn translates_a_known_message() {
        let mut i18n = Localization::no_bidi();
        assert_eq!(
            i18n.translate(KNOWN.0, KNOWN.1, None).as_deref(),
            Some("Reply")
        );
    }

    #[test]
    fn the_second_lookup_of_a_message_is_a_cache_hit() {
        let mut i18n = Localization::no_bidi();

        assert_eq!(i18n.cached_translation(KNOWN.0), None);

        let first = i18n.translate(KNOWN.0, KNOWN.1, None);
        assert_eq!(first.as_deref(), Some("Reply"));

        // The point of the split: after the first lookup the answer is reachable
        // without touching a bundle, formatting a pattern or allocating.
        assert_eq!(i18n.cached_translation(KNOWN.0), Some("Reply"));
        assert_eq!(i18n.translate(KNOWN.0, KNOWN.1, None), first);
    }

    #[test]
    fn an_untranslated_message_is_none_rather_than_an_error() {
        let mut i18n = Localization::no_bidi();

        // Nothing generated an FTL key for this, so the bundle has no value for
        // it. `tr!` turns the `None` into the source message.
        assert_eq!(
            i18n.translate("no ftl entry exists for this", "nor for this comment", None),
            None
        );
    }

    #[test]
    fn an_untranslated_message_is_not_cached() {
        let mut i18n = Localization::no_bidi();

        i18n.translate("no ftl entry exists for this", "nor for this comment", None);

        // It lands in `format_uncached` again on every later lookup, which is
        // why that path is written to allocate nothing.
        assert_eq!(i18n.get_cache_stats().unwrap().string_cache_size, 0);
    }

    #[test]
    fn strings_with_arguments_are_never_cached() {
        let mut i18n = Localization::no_bidi();
        let mut args = FluentArgs::new();
        args.set("count", 2);

        i18n.translate(KNOWN.0, KNOWN.1, Some(&args));

        assert_eq!(i18n.get_cache_stats().unwrap().string_cache_size, 0);
    }

    #[test]
    fn changing_locale_drops_the_cached_translations() {
        let mut i18n = Localization::no_bidi();

        i18n.translate(KNOWN.0, KNOWN.1, None);
        assert!(i18n.cached_translation(KNOWN.0).is_some());

        i18n.set_locale(EN_XA).unwrap();

        // The normalized key survives — it does not depend on the locale — but
        // the translation must not.
        assert_eq!(i18n.cached_translation(KNOWN.0), None);
    }

    #[test]
    fn tr_falls_back_to_the_source_message() {
        let mut i18n = Localization::no_bidi();
        assert_eq!(
            crate::tr!(i18n, "no ftl entry exists for this", "nor for this comment"),
            "no ftl entry exists for this"
        );
    }
}
