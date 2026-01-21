//! D-tag generation utilities for NKBIP-01 publications
//!
//! D-tags are namespaced to prevent collisions when the same section title
//! appears in different publications.

/// Generate a namespaced d-tag from publication and section titles
///
/// Format: `{publication_abbreviation}-{section_slug}`
///
/// # Examples
///
/// ```
/// use notedeck_publications::dtag::generate_dtag;
///
/// assert_eq!(generate_dtag("Understanding Nostr", "Introduction"), "un-introduction");
/// assert_eq!(generate_dtag("My Book", "Chapter 1"), "mb-chapter-1");
/// ```
pub fn generate_dtag(publication_title: &str, section_title: &str) -> String {
    let abbrev = title_abbreviation(publication_title);
    let slug = slugify(section_title);
    format!("{}-{}", abbrev, slug)
}

/// Generate an abbreviation from a title
///
/// Takes the first letter of each word, lowercased.
///
/// # Examples
///
/// ```
/// use notedeck_publications::dtag::title_abbreviation;
///
/// assert_eq!(title_abbreviation("Understanding Nostr"), "un");
/// assert_eq!(title_abbreviation("My Test Article"), "mta");
/// ```
pub fn title_abbreviation(title: &str) -> String {
    title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .filter_map(|w| w.chars().next())
        .map(|c| c.to_lowercase().to_string())
        .collect()
}

/// Convert a string to a URL-safe slug
///
/// - Lowercases the string
/// - Replaces non-alphanumeric characters with hyphens
/// - Collapses multiple hyphens
/// - Removes leading/trailing hyphens
///
/// # Examples
///
/// ```
/// use notedeck_publications::dtag::slugify;
///
/// assert_eq!(slugify("Hello World"), "hello-world");
/// assert_eq!(slugify("Chapter 1: Introduction"), "chapter-1-introduction");
/// assert_eq!(slugify("  Multiple   Spaces  "), "multiple-spaces");
/// ```
pub fn slugify(s: &str) -> String {
    let slug: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();

    // Collapse multiple hyphens and trim
    slug.split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Parse a d-tag to extract the publication abbreviation
///
/// Returns the prefix before the first hyphen.
pub fn extract_abbreviation(dtag: &str) -> Option<&str> {
    dtag.split('-').next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_dtag() {
        assert_eq!(
            generate_dtag("Understanding Nostr", "Introduction"),
            "un-introduction"
        );
        assert_eq!(
            generate_dtag("Alexandria Guide", "Getting Started"),
            "ag-getting-started"
        );
        assert_eq!(generate_dtag("My Book", "Chapter 1"), "mb-chapter-1");
    }

    #[test]
    fn test_title_abbreviation() {
        assert_eq!(title_abbreviation("Understanding Nostr"), "un");
        assert_eq!(title_abbreviation("My Test Article"), "mta");
        assert_eq!(title_abbreviation("A"), "a");
        assert_eq!(title_abbreviation("single"), "s");
        assert_eq!(title_abbreviation("with-hyphens-here"), "whh");
    }

    #[test]
    fn test_slugify() {
        assert_eq!(slugify("Hello World"), "hello-world");
        assert_eq!(slugify("Chapter 1"), "chapter-1");
        assert_eq!(slugify("  spaces  "), "spaces");
        assert_eq!(slugify("UPPERCASE"), "uppercase");
        assert_eq!(slugify("special!@#chars"), "special-chars");
        assert_eq!(slugify("multiple---hyphens"), "multiple-hyphens");
    }

    #[test]
    fn test_extract_abbreviation() {
        assert_eq!(extract_abbreviation("un-introduction"), Some("un"));
        assert_eq!(extract_abbreviation("mb-chapter-1"), Some("mb"));
        assert_eq!(extract_abbreviation("nodash"), Some("nodash"));
    }

    #[test]
    fn test_empty_inputs() {
        assert_eq!(title_abbreviation(""), "");
        assert_eq!(slugify(""), "");
        assert_eq!(generate_dtag("", "test"), "-test");
    }

    #[test]
    fn test_unicode() {
        // Unicode letters should be preserved
        assert_eq!(slugify("Café"), "café");
        assert_eq!(title_abbreviation("日本語 Title"), "日t");
    }
}
