use fluent::FluentArgs;
use fluent::{FluentBundle, FluentResource};
use fluent_langneg::negotiate_languages;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};
use unic_langid::LanguageIdentifier;

/// Manages localization resources and provides localized strings
pub struct LocalizationManager {
    /// Current locale
    current_locale: RwLock<LanguageIdentifier>,
    /// Available locales
    available_locales: Vec<LanguageIdentifier>,
    /// Fallback locale
    fallback_locale: LanguageIdentifier,
    /// Resource directory path
    resource_dir: std::path::PathBuf,
    /// Cached parsed FluentResource per locale
    resource_cache: RwLock<HashMap<LanguageIdentifier, Arc<FluentResource>>>,
    /// Cached string results per locale (only for strings without arguments)
    string_cache: RwLock<HashMap<LanguageIdentifier, HashMap<String, String>>>,
}

impl LocalizationManager {
    /// Creates a new LocalizationManager with the specified resource directory
    pub fn new(resource_dir: &Path) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        // Default to English (US)
        let default_locale: LanguageIdentifier = "en-US"
            .parse()
            .map_err(|e| format!("Locale parse error: {e:?}"))?;
        let fallback_locale = default_locale.clone();

        // Check if pseudolocale is enabled via environment variable
        let enable_pseudolocale = std::env::var("NOTEDECK_PSEUDOLOCALE").is_ok();

        // Build available locales list
        let mut available_locales = vec![default_locale.clone()];

        // Add en-XA if pseudolocale is enabled
        if enable_pseudolocale {
            let pseudolocale: LanguageIdentifier = "en-XA"
                .parse()
                .map_err(|e| format!("Pseudolocale parse error: {e:?}"))?;
            available_locales.push(pseudolocale);
            tracing::info!(
                "Pseudolocale (en-XA) enabled via NOTEDECK_PSEUDOLOCALE environment variable"
            );
        }

        Ok(Self {
            current_locale: RwLock::new(default_locale),
            available_locales,
            fallback_locale,
            resource_dir: resource_dir.to_path_buf(),
            resource_cache: RwLock::new(HashMap::new()),
            string_cache: RwLock::new(HashMap::new()),
        })
    }

    /// Gets a localized string by its ID
    pub fn get_string(&self, id: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        tracing::debug!(
            "Getting string '{}' for locale '{}'",
            id,
            self.get_current_locale()?
        );
        let result = self.get_string_with_args(id, None);
        if let Err(ref e) = result {
            tracing::error!("Failed to get string '{}': {}", id, e);
        }
        result
    }

    /// Loads and caches a parsed FluentResource for the given locale
    fn load_resource_for_locale(
        &self,
        locale: &LanguageIdentifier,
    ) -> Result<Arc<FluentResource>, Box<dyn std::error::Error + Send + Sync>> {
        // Construct the path using the stored resource directory
        let expected_path = self.resource_dir.join(format!("{locale}/main.ftl"));

        // Try to open the file directly
        if let Err(e) = std::fs::File::open(&expected_path) {
            tracing::error!(
                "Direct file open failed: {} ({})",
                expected_path.display(),
                e
            );
            return Err(format!("Failed to open FTL file: {e}").into());
        }

        // Load the FTL file directly instead of using ResourceManager
        let ftl_string = std::fs::read_to_string(&expected_path)
            .map_err(|e| format!("Failed to read FTL file: {e}"))?;

        // Parse the FTL content
        let resource = FluentResource::try_new(ftl_string)
            .map_err(|e| format!("Failed to parse FTL content: {e:?}"))?;

        tracing::debug!(
            "Loaded and cached parsed FluentResource for locale: {}",
            locale
        );
        Ok(Arc::new(resource))
    }

    /// Gets cached parsed FluentResource for the current locale, loading it if necessary
    fn get_cached_resource(
        &self,
    ) -> Result<Arc<FluentResource>, Box<dyn std::error::Error + Send + Sync>> {
        let locale = self
            .current_locale
            .read()
            .map_err(|e| format!("Lock error: {e}"))?;

        // Try to get from cache first
        {
            let cache = self
                .resource_cache
                .read()
                .map_err(|e| format!("Cache lock error: {e}"))?;
            if let Some(resource) = cache.get(&locale) {
                tracing::debug!("Using cached parsed FluentResource for locale: {}", locale);
                return Ok(resource.clone());
            }
        }

        // Not in cache, load and cache it
        let resource = self.load_resource_for_locale(&locale)?;

        // Store in cache
        {
            let mut cache = self
                .resource_cache
                .write()
                .map_err(|e| format!("Cache lock error: {e}"))?;
            cache.insert(locale.clone(), resource.clone());
            tracing::debug!("Cached parsed FluentResource for locale: {}", locale);
        }

        Ok(resource)
    }

    /// Gets cached string result, or formats it and caches the result
    fn get_cached_string(
        &self,
        id: &str,
        args: Option<&FluentArgs>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        let locale = self
            .current_locale
            .read()
            .map_err(|e| format!("Lock error: {e}"))?;

        // Only cache simple strings without arguments
        // For strings with arguments, we can't cache the final result since args may vary
        if args.is_none() {
            // Try to get from string cache first
            {
                let cache = self
                    .string_cache
                    .read()
                    .map_err(|e| format!("String cache lock error: {e}"))?;
                if let Some(locale_cache) = cache.get(&locale) {
                    if let Some(cached_string) = locale_cache.get(id) {
                        tracing::debug!(
                            "Using cached string result for '{}' in locale: {}",
                            id,
                            locale
                        );
                        return Ok(cached_string.clone());
                    }
                }
            }
        }

        // Not in cache or has arguments, format it using cached resource
        let resource = self.get_cached_resource()?;

        // Create a bundle for this request (not cached due to thread-safety issues)
        let mut bundle = FluentBundle::new(vec![locale.clone()]);
        bundle
            .add_resource(resource.as_ref())
            .map_err(|e| format!("Failed to add resource to bundle: {e:?}"))?;

        let message = bundle
            .get_message(id)
            .ok_or_else(|| format!("Message not found: {id}"))?;

        let pattern = message
            .value()
            .ok_or_else(|| format!("Message has no value: {id}"))?;

        // Format the message
        let mut errors = Vec::new();
        let result = bundle.format_pattern(pattern, args, &mut errors);

        if !errors.is_empty() {
            tracing::warn!("Localization errors for {}: {:?}", id, errors);
        }

        let result_string = result.into_owned();

        // Only cache simple strings without arguments
        // This prevents caching issues when the same message ID is used with different arguments
        if args.is_none() {
            let mut cache = self
                .string_cache
                .write()
                .map_err(|e| format!("String cache lock error: {e}"))?;
            let locale_cache = cache.entry(locale.clone()).or_insert_with(HashMap::new);
            locale_cache.insert(id.to_string(), result_string.clone());
            tracing::debug!("Cached string result for '{}' in locale: {}", id, locale);
        } else {
            tracing::debug!("Not caching string '{}' due to arguments", id);
        }

        Ok(result_string)
    }

    /// Gets a localized string by its ID with optional arguments
    pub fn get_string_with_args(
        &self,
        id: &str,
        args: Option<&FluentArgs>,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        self.get_cached_string(id, args)
    }

    /// Sets the current locale
    pub fn set_locale(
        &self,
        locale: LanguageIdentifier,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tracing::info!("Attempting to set locale to: {}", locale);
        tracing::info!("Available locales: {:?}", self.available_locales);

        // Validate that the locale is available
        if !self.available_locales.contains(&locale) {
            tracing::error!(
                "Locale {} is not available. Available locales: {:?}",
                locale,
                self.available_locales
            );
            return Err(format!("Locale {locale} is not available").into());
        }

        let mut current = self
            .current_locale
            .write()
            .map_err(|e| format!("Lock error: {e}"))?;
        tracing::info!("Switching locale from {} to {locale}", *current);
        *current = locale.clone();
        tracing::info!("Successfully set locale to: {locale}");

        // Clear caches when locale changes since they are locale-specific
        let mut string_cache = self
            .string_cache
            .write()
            .map_err(|e| format!("String cache lock error: {e}"))?;
        string_cache.clear();
        tracing::debug!("String cache cleared due to locale change");

        Ok(())
    }

    /// Clears the parsed FluentResource cache (useful for development when FTL files change)
    pub fn clear_cache(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut cache = self
            .resource_cache
            .write()
            .map_err(|e| format!("Cache lock error: {e}"))?;
        cache.clear();
        tracing::info!("Parsed FluentResource cache cleared");

        let mut string_cache = self
            .string_cache
            .write()
            .map_err(|e| format!("String cache lock error: {e}"))?;
        string_cache.clear();
        tracing::info!("String result cache cleared");

        Ok(())
    }

    /// Gets the current locale
    pub fn get_current_locale(
        &self,
    ) -> Result<LanguageIdentifier, Box<dyn std::error::Error + Send + Sync>> {
        let current = self
            .current_locale
            .read()
            .map_err(|e| format!("Lock error: {e}"))?;
        Ok(current.clone())
    }

    /// Gets all available locales
    pub fn get_available_locales(&self) -> &[LanguageIdentifier] {
        &self.available_locales
    }

    /// Gets the fallback locale
    pub fn get_fallback_locale(&self) -> &LanguageIdentifier {
        &self.fallback_locale
    }

    /// Gets cache statistics for monitoring performance
    pub fn get_cache_stats(&self) -> Result<CacheStats, Box<dyn std::error::Error + Send + Sync>> {
        let resource_cache = self
            .resource_cache
            .read()
            .map_err(|e| format!("Cache lock error: {e}"))?;
        let string_cache = self
            .string_cache
            .read()
            .map_err(|e| format!("String cache lock error: {e}"))?;

        let mut total_strings = 0;
        for locale_cache in string_cache.values() {
            total_strings += locale_cache.len();
        }

        Ok(CacheStats {
            resource_cache_size: resource_cache.len(),
            string_cache_size: total_strings,
            cached_locales: resource_cache.keys().cloned().collect(),
        })
    }

    /// Limits the string cache size to prevent memory growth
    pub fn limit_string_cache_size(
        &self,
        max_strings_per_locale: usize,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut string_cache = self
            .string_cache
            .write()
            .map_err(|e| format!("String cache lock error: {e}"))?;

        for locale_cache in string_cache.values_mut() {
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

/// Context for sharing localization across the application
#[derive(Clone)]
pub struct LocalizationContext {
    /// The localization manager
    manager: Arc<LocalizationManager>,
}

impl LocalizationContext {
    /// Creates a new LocalizationContext
    pub fn new(manager: Arc<LocalizationManager>) -> Self {
        let context = Self { manager };

        // Auto-switch to pseudolocale if environment variable is set
        if std::env::var("NOTEDECK_PSEUDOLOCALE").is_ok() {
            tracing::info!("NOTEDECK_PSEUDOLOCALE environment variable detected");
            if let Ok(pseudolocale) = "en-XA".parse::<LanguageIdentifier>() {
                tracing::info!("Attempting to switch to pseudolocale: {}", pseudolocale);
                if let Err(e) = context.set_locale(pseudolocale) {
                    tracing::warn!("Failed to switch to pseudolocale: {}", e);
                } else {
                    tracing::info!("Automatically switched to pseudolocale (en-XA)");
                }
            } else {
                tracing::error!("Failed to parse en-XA as LanguageIdentifier");
            }
        } else {
            tracing::info!("NOTEDECK_PSEUDOLOCALE environment variable not set");
        }

        context
    }

    /// Gets a localized string by its ID
    pub fn get_string(&self, id: &str) -> Option<String> {
        self.manager.get_string(id).ok()
    }

    /// Gets a localized string by its ID with optional arguments
    pub fn get_string_with_args(&self, id: &str, args: Option<&FluentArgs>) -> String {
        self.manager
            .get_string_with_args(id, args)
            .unwrap_or_else(|_| format!("[MISSING: {id}]"))
    }

    /// Sets the current locale
    pub fn set_locale(
        &self,
        locale: LanguageIdentifier,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.manager.set_locale(locale)
    }

    /// Gets the current locale
    pub fn get_current_locale(
        &self,
    ) -> Result<LanguageIdentifier, Box<dyn std::error::Error + Send + Sync>> {
        self.manager.get_current_locale()
    }

    /// Clears the resource cache (useful for development when FTL files change)
    pub fn clear_cache(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.manager.clear_cache()
    }

    /// Gets the underlying manager
    pub fn manager(&self) -> &Arc<LocalizationManager> {
        &self.manager
    }
}

/// Trait for objects that can be localized
pub trait Localizable {
    /// Gets a localized string by its ID
    fn get_localized_string(&self, id: &str) -> String;

    /// Gets a localized string by its ID with optional arguments
    fn get_localized_string_with_args(&self, id: &str, args: Option<&FluentArgs>) -> String;
}

impl Localizable for LocalizationContext {
    fn get_localized_string(&self, id: &str) -> String {
        self.get_string(id)
            .unwrap_or_else(|| format!("[MISSING: {id}]"))
    }

    fn get_localized_string_with_args(&self, id: &str, args: Option<&FluentArgs>) -> String {
        self.get_string_with_args(id, args)
    }
}

/// Statistics about cache usage
#[derive(Debug, Clone)]
pub struct CacheStats {
    pub resource_cache_size: usize,
    pub string_cache_size: usize,
    pub cached_locales: Vec<LanguageIdentifier>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_localization_manager_creation() {
        let temp_dir = std::env::temp_dir().join("notedeck_i18n_test");
        std::fs::create_dir_all(&temp_dir).unwrap();

        let manager = LocalizationManager::new(&temp_dir);
        assert!(manager.is_ok());

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[test]
    fn test_locale_management() {
        let temp_dir = std::env::temp_dir().join("notedeck_i18n_test2");
        std::fs::create_dir_all(&temp_dir).unwrap();

        let manager = LocalizationManager::new(&temp_dir).unwrap();

        // Test default locale
        let current = manager.get_current_locale().unwrap();
        assert_eq!(current.to_string(), "en-US");

        // Test available locales
        let available = manager.get_available_locales();
        assert_eq!(available.len(), 1);
        assert_eq!(available[0].to_string(), "en-US");

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[test]
    fn test_ftl_caching() {
        let temp_dir = std::env::temp_dir().join("notedeck_i18n_test3");
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Create a test FTL file
        let en_us_dir = temp_dir.join("en-US");
        std::fs::create_dir_all(&en_us_dir).unwrap();
        let ftl_content = "test_key = Test Value\nanother_key = Another Value";
        std::fs::write(en_us_dir.join("main.ftl"), ftl_content).unwrap();

        let manager = LocalizationManager::new(&temp_dir).unwrap();

        // First call should load and cache the FTL content
        let result1 = manager.get_string("test_key");
        assert!(result1.is_ok());
        assert_eq!(result1.as_ref().unwrap(), "Test Value");

        // Second call should use cached FTL content
        let result2 = manager.get_string("test_key");
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap(), "Test Value");

        // Test another key from the same FTL content
        let result3 = manager.get_string("another_key");
        assert!(result3.is_ok());
        assert_eq!(result3.unwrap(), "Another Value");

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[test]
    fn test_cache_clearing() {
        let temp_dir = std::env::temp_dir().join("notedeck_i18n_test4");
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Create a test FTL file
        let en_us_dir = temp_dir.join("en-US");
        std::fs::create_dir_all(&en_us_dir).unwrap();
        let ftl_content = "test_key = Test Value";
        std::fs::write(en_us_dir.join("main.ftl"), ftl_content).unwrap();

        let manager = LocalizationManager::new(&temp_dir).unwrap();

        // Load and cache the FTL content
        let result1 = manager.get_string("test_key");
        assert!(result1.is_ok());

        // Clear the cache
        let clear_result = manager.clear_cache();
        assert!(clear_result.is_ok());

        // Should still work after clearing cache (will reload)
        let result2 = manager.get_string("test_key");
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap(), "Test Value");

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[test]
    fn test_context_caching() {
        let temp_dir = std::env::temp_dir().join("notedeck_i18n_test5");
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Create a test FTL file
        let en_us_dir = temp_dir.join("en-US");
        std::fs::create_dir_all(&en_us_dir).unwrap();
        let ftl_content = "test_key = Test Value";
        std::fs::write(en_us_dir.join("main.ftl"), ftl_content).unwrap();

        let manager = Arc::new(LocalizationManager::new(&temp_dir).unwrap());
        let context = LocalizationContext::new(manager);

        // Debug: check what the normalized key should be
        let normalized_key = crate::i18n::normalize_ftl_key("test_key", None);
        println!("Normalized key: '{}'", normalized_key);

        // First call should load and cache the FTL content
        let result1 = context.get_string("test_key");
        println!("First result: {:?}", result1);
        assert!(result1.is_some());
        assert_eq!(result1.unwrap(), "Test Value");

        // Second call should use cached FTL content
        let result2 = context.get_string("test_key");
        assert!(result2.is_some());
        assert_eq!(result2.unwrap(), "Test Value");

        // Test cache clearing through context
        let clear_result = context.clear_cache();
        assert!(clear_result.is_ok());

        // Should still work after clearing cache
        let result3 = context.get_string("test_key");
        assert!(result3.is_some());
        assert_eq!(result3.unwrap(), "Test Value");

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[test]
    fn test_bundle_caching() {
        let temp_dir = std::env::temp_dir().join("notedeck_i18n_test6");
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Create a test FTL file
        let en_us_dir = temp_dir.join("en-US");
        std::fs::create_dir_all(&en_us_dir).unwrap();
        let ftl_content = "test_key = Test Value\nanother_key = Another Value";
        std::fs::write(en_us_dir.join("main.ftl"), ftl_content).unwrap();

        let manager = LocalizationManager::new(&temp_dir).unwrap();

        // First call should create bundle and cache the resource
        let result1 = manager.get_string("test_key");
        assert!(result1.is_ok());
        assert_eq!(result1.unwrap(), "Test Value");

        // Second call should use cached resource but create new bundle
        let result2 = manager.get_string("another_key");
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap(), "Another Value");

        // Check cache stats
        let stats = manager.get_cache_stats().unwrap();
        assert_eq!(stats.resource_cache_size, 1);
        assert_eq!(stats.string_cache_size, 2); // Both strings should be cached

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[test]
    fn test_string_caching() {
        let temp_dir = std::env::temp_dir().join("notedeck_i18n_test7");
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Create a test FTL file
        let en_us_dir = temp_dir.join("en-US");
        std::fs::create_dir_all(&en_us_dir).unwrap();
        let ftl_content = "test_key = Test Value";
        std::fs::write(en_us_dir.join("main.ftl"), ftl_content).unwrap();

        let manager = LocalizationManager::new(&temp_dir).unwrap();

        // First call should format and cache the string
        let result1 = manager.get_string("test_key");
        assert!(result1.is_ok());
        assert_eq!(result1.unwrap(), "Test Value");

        // Second call should use cached string
        let result2 = manager.get_string("test_key");
        assert!(result2.is_ok());
        assert_eq!(result2.unwrap(), "Test Value");

        // Check cache stats
        let stats = manager.get_cache_stats().unwrap();
        assert_eq!(stats.string_cache_size, 1);

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[test]
    fn test_cache_clearing_on_locale_change() {
        let temp_dir = std::env::temp_dir().join("notedeck_i18n_test8");
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Create test FTL files for two locales
        let en_us_dir = temp_dir.join("en-US");
        std::fs::create_dir_all(&en_us_dir).unwrap();
        std::fs::write(en_us_dir.join("main.ftl"), "test_key = Test Value").unwrap();

        let en_xa_dir = temp_dir.join("en-XA");
        std::fs::create_dir_all(&en_xa_dir).unwrap();
        std::fs::write(en_xa_dir.join("main.ftl"), "test_key = Test Value XA").unwrap();

        // Enable pseudolocale for this test
        std::env::set_var("NOTEDECK_PSEUDOLOCALE", "1");

        let manager = LocalizationManager::new(&temp_dir).unwrap();

        // Load some strings in en-US
        let result1 = manager.get_string("test_key");
        assert!(result1.is_ok());

        // Check that caches are populated
        let stats1 = manager.get_cache_stats().unwrap();
        assert!(stats1.resource_cache_size > 0);
        assert!(stats1.string_cache_size > 0);

        // Switch to en-XA
        let en_xa: LanguageIdentifier = "en-XA".parse().unwrap();
        manager.set_locale(en_xa).unwrap();

        // Check that string cache is cleared (resource cache remains for both locales)
        let stats2 = manager.get_cache_stats().unwrap();
        assert_eq!(stats2.string_cache_size, 0);

        // Cleanup
        std::env::remove_var("NOTEDECK_PSEUDOLOCALE");
        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[test]
    fn test_string_caching_with_arguments() {
        let temp_dir = std::env::temp_dir().join("notedeck_i18n_test9");
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Create a test FTL file with a message that takes arguments
        let en_us_dir = temp_dir.join("en-US");
        std::fs::create_dir_all(&en_us_dir).unwrap();
        let ftl_content = "welcome_message = Welcome {$name}!";
        std::fs::write(en_us_dir.join("main.ftl"), ftl_content).unwrap();

        let manager = LocalizationManager::new(&temp_dir).unwrap();

        // First call with arguments should not be cached
        let mut args = fluent::FluentArgs::new();
        args.set("name", "Alice");
        let result1 = manager.get_string_with_args("welcome_message", Some(&args));
        assert!(result1.is_ok());
        // Note: Fluent may add bidirectional text control characters, so we check contains
        let result1_str = result1.unwrap();
        assert!(result1_str.contains("Alice"));

        // Check that it's not in the string cache
        let stats1 = manager.get_cache_stats().unwrap();
        assert_eq!(stats1.string_cache_size, 0);

        // Second call with different arguments should work correctly
        let mut args2 = fluent::FluentArgs::new();
        args2.set("name", "Bob");
        let result2 = manager.get_string_with_args("welcome_message", Some(&args2));
        assert!(result2.is_ok());
        let result2_str = result2.unwrap();
        assert!(result2_str.contains("Bob"));

        // Check that it's still not in the string cache
        let stats2 = manager.get_cache_stats().unwrap();
        assert_eq!(stats2.string_cache_size, 0);

        // Test a simple string without arguments - should be cached
        let ftl_content_simple = "simple_message = Hello World";
        std::fs::write(en_us_dir.join("main.ftl"), ftl_content_simple).unwrap();

        // Clear cache to start fresh
        manager.clear_cache().unwrap();

        let result3 = manager.get_string("simple_message");
        assert!(result3.is_ok());
        assert_eq!(result3.unwrap(), "Hello World");

        // Check that simple string is cached
        let stats3 = manager.get_cache_stats().unwrap();
        assert_eq!(stats3.string_cache_size, 1);

        // Cleanup
        std::fs::remove_dir_all(&temp_dir).unwrap();
    }
}
