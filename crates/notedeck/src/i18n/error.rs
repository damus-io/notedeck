use unic_langid::LanguageIdentifier;

/// App related errors
#[derive(thiserror::Error, Debug)]
pub enum IntlError {
    #[error("Locale({0}) parse error: {1}")]
    LocaleParse(LanguageIdentifier, String),

    #[error("locale not available: {0}")]
    LocaleNotAvailable(LanguageIdentifier),

    #[error("FTL for '{0}' is not available")]
    NoFtl(LanguageIdentifier),

    #[error("Bundle for '{0}' is not available")]
    NoBundle(LanguageIdentifier),
}
