//! A querystring encoding of a single NIP-01 filter.
//!
//! This module is the reference implementation of the format; the written spec
//! lives in damus-api's `docs/`, beside `nip-http-req.md`. What follows is the
//! set of properties this implementation actually relies on, so the two cannot
//! drift silently.
//!
//! # Shape
//!
//! `key=value` pairs joined with `&`, e.g. `#t=bitcoin,nostr&kinds=1&limit=100`.
//! The keys are the NIP-01 filter attribute names — `ids`, `authors`, `kinds`,
//! `since`, `until`, `limit` — plus `search` and `relays`, which NIP-01 proper
//! does not define, and `#x` for a single-letter tag filter.
//!
//! # Canonical form
//!
//! The encoding is canonical so that the text can be used directly as a cache
//! or identity key: keys are sorted, and each array value is sorted and
//! comma-joined. Two filters with the same constraints therefore produce
//! byte-identical text.
//!
//! Sorting array values is only sound because **NIP-01 filter arrays are sets**
//! — order carries no meaning and multiplicity is not observable. An
//! implementer who preserves the caller's order instead will produce a
//! different string for the same filter and break the canonical property.
//!
//! # Tag filters
//!
//! A tag filter is keyed `#x`, matching NIP-01's own JSON key. `#` is part of
//! the key, not an encoding artifact — it is escaped as `%23` at the URL
//! boundary because `#` would otherwise start the fragment. The key is `#`
//! followed by exactly one character; `#tt` is not a spelling of `#t`.
//!
//! # Element types, and a known ambiguity
//!
//! nostrdb types the members of a tag filter as either 32-byte ids or strings,
//! and requires the members of one field to share a type. This encoding has no
//! syntax for that distinction: an id and a string holding the same 64 hex
//! characters encode to identical text. So the decoder infers it — a tag list
//! is a list of ids only if *every* member is 64 hex characters, and a list of
//! strings otherwise.
//!
//! The inference is a property of the whole list rather than of each member on
//! purpose. Deciding per-member would produce a mixed-type field, which nostrdb
//! rejects outright; deciding from the first member (which is what nostrdb's
//! own JSON filter parser does) would depend on order, and this encoding sorts.
//!
//! What survives is a real gap: **a tag value that is genuinely a string and
//! happens to be 64 hex characters decodes as an id.** A `#d` tag can hold such
//! a value. Closing it needs the *encoder* to mark the distinction, which is a
//! change to the format and therefore a decision for the spec, not for this
//! implementation — see `test_tag_str_64hex_does_not_roundtrip`, which pins the
//! current behavior so that a format change has to update it deliberately.
//!
//! Note that the 64-hex test is applied to the raw, still-percent-encoded
//! member. Percent-encoding any one character of a value therefore already
//! forces it to decode as a string, which is available as the escape hatch if
//! the spec wants one.
//!
//! # Percent-encoding
//!
//! String values (tag members, `search`, `relays`) are percent-encoded with the
//! unreserved set of RFC 3986, so `,`, `&` and `=` inside a value cannot be
//! confused with the separators around it. Spaces become `%20`, never `+`, and
//! the decoder does not treat `+` as a space. A consumer that carries this text
//! as an `application/x-www-form-urlencoded` body must not run form decoding
//! over it first, or a literal `+` in a value will be corrupted.
//!
//! # What the format cannot express
//!
//! - **A union.** [`filter_to_querystring`] takes one filter, but a NIP-01 REQ
//!   is a list of them, and [`crate::timeline::kind::FilterVec`] keeps a `Vec`.
//!   Every consumer invents its own way to carry the list.
//! - **nostrdb custom predicates.** A filter carrying a `custom` callback is
//!   not serializable at all, and the encoder drops it silently, which yields a
//!   *wider* filter than the original. No caller does this today; making it
//!   explicit needs a fallible encoder signature.

use nostrdb::{Filter, FilterBuilder, FilterField};

/// Encode a single [`Filter`] to its canonical querystring.
///
/// See the [module documentation](self) for the format.
pub fn filter_to_querystring(filter: &Filter) -> String {
    let mut pairs: Vec<(String, String)> = Vec::new();

    for field in filter {
        match field {
            FilterField::Ids(ids) => {
                let mut arr: Vec<String> = ids.into_iter().map(hex::encode).collect();
                arr.sort();
                if !arr.is_empty() {
                    pairs.push(("ids".to_string(), arr.join(",")));
                }
            }
            FilterField::Authors(authors) => {
                let mut arr: Vec<String> = authors.into_iter().map(hex::encode).collect();
                arr.sort();
                if !arr.is_empty() {
                    pairs.push(("authors".to_string(), arr.join(",")));
                }
            }
            FilterField::Kinds(kinds) => {
                let mut arr: Vec<u64> = kinds.into_iter().collect();
                arr.sort();
                if !arr.is_empty() {
                    let val = arr
                        .iter()
                        .map(|k| k.to_string())
                        .collect::<Vec<_>>()
                        .join(",");
                    pairs.push(("kinds".to_string(), val));
                }
            }
            FilterField::Tags(tag_char, elements) => {
                let key = format!("#{tag_char}");
                let mut arr: Vec<String> = Vec::new();
                for elem in elements {
                    match elem {
                        nostrdb::FilterElement::Str(s) => {
                            arr.push(urlencoding::encode(s).into_owned())
                        }
                        nostrdb::FilterElement::Id(id) => arr.push(hex::encode(id)),
                        nostrdb::FilterElement::Int(n) => arr.push(n.to_string()),
                        nostrdb::FilterElement::Custom => {}
                    }
                }
                arr.sort();
                if !arr.is_empty() {
                    pairs.push((key, arr.join(",")));
                }
            }
            FilterField::Search(s) => {
                pairs.push(("search".to_string(), urlencoding::encode(s).into_owned()));
            }
            FilterField::Since(n) => {
                pairs.push(("since".to_string(), n.to_string()));
            }
            FilterField::Until(n) => {
                pairs.push(("until".to_string(), n.to_string()));
            }
            FilterField::Limit(n) => {
                pairs.push(("limit".to_string(), n.to_string()));
            }
            FilterField::Relays(relays) => {
                let mut arr: Vec<String> = relays
                    .into_iter()
                    .map(|s| urlencoding::encode(s).into_owned())
                    .collect();
                arr.sort();
                if !arr.is_empty() {
                    pairs.push(("relays".to_string(), arr.join(",")));
                }
            }
            FilterField::Custom(_) => {}
        }
    }

    // Sort by key for canonical representation
    pairs.sort_by(|a, b| a.0.cmp(&b.0));

    pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Decode a canonical querystring back to a [`Filter`].
///
/// Returns `None` for anything it cannot represent exactly, rather than
/// decoding a filter that is *wider* than the text asked for. In particular an
/// unrecognized key is rejected instead of ignored: silently dropping a
/// constraint answers a different question than the one that was asked, and
/// [`crate::timeline::kind::TimelineKind`] relies on this returning `None` to
/// tell a filter token apart from an unrelated one.
///
/// See the [module documentation](self) for the format.
pub fn filter_from_querystring(qs: &str) -> Option<Filter> {
    if qs.is_empty() {
        return None;
    }

    let mut builder = Filter::new();
    let mut seen: Vec<&str> = Vec::new();

    for pair in qs.split('&') {
        let (key, val) = pair.split_once('=')?;

        // A repeated key is ambiguous — first-wins, last-wins and union are all
        // defensible readings — so reject rather than silently pick one.
        if seen.contains(&key) {
            return None;
        }
        seen.push(key);

        match key {
            "ids" => {
                builder.start_ids_field().ok()?;
                add_id_elements(&mut builder, val)?;
                builder.end_field();
            }
            "authors" => {
                builder.start_authors_field().ok()?;
                add_id_elements(&mut builder, val)?;
                builder.end_field();
            }
            "kinds" => {
                builder.start_kinds_field().ok()?;
                for k in val.split(',') {
                    builder.add_int_element(k.parse().ok()?).ok()?;
                }
                builder.end_field();
            }
            "search" => {
                let decoded = urlencoding::decode(val).ok()?;
                builder = builder.search(&decoded);
            }
            "since" => {
                builder = builder.since(val.parse().ok()?);
            }
            "until" => {
                builder = builder.until(val.parse().ok()?);
            }
            "limit" => {
                builder = builder.limit(val.parse().ok()?);
            }
            "relays" => {
                builder.start_relays_field().ok()?;
                for relay in val.split(',') {
                    let decoded = urlencoding::decode(relay).ok()?;
                    builder.add_str_element(&decoded).ok()?;
                }
                builder.end_field();
            }
            _ => {
                let tag_char = tag_key_char(key)?;
                add_tag_field(&mut builder, tag_char, val)?;
            }
        }
    }

    Some(builder.build())
}

/// The single character of a `#x` tag key, or `None` if `key` is not one.
///
/// NIP-01 tag filters are single-letter, so `#tt` is rejected rather than read
/// as `#t` with trailing junk.
fn tag_key_char(key: &str) -> Option<char> {
    let mut chars = key.strip_prefix('#')?.chars();
    let tag_char = chars.next()?;
    chars.next().is_none().then_some(tag_char)
}

/// A 32-byte id in the 64-character hex form the encoder emits.
fn is_hex32(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Add the comma-separated members of an `ids` or `authors` field.
fn add_id_elements(builder: &mut FilterBuilder, val: &str) -> Option<()> {
    for hex_id in val.split(',') {
        let bytes = hex::decode(hex_id).ok()?;
        let id: &[u8; 32] = bytes.as_slice().try_into().ok()?;
        builder.add_id_element(id).ok()?;
    }

    Some(())
}

/// Add a `#x=a,b,c` tag field, inferring the element type from the whole list.
///
/// nostrdb requires the members of one field to share a type, so the choice is
/// made once for the list — ids only if every member is 64 hex characters — and
/// not per member. See the [module documentation](self) for why this cannot be
/// decided from the first member and for the ambiguity that remains.
fn add_tag_field(builder: &mut FilterBuilder, tag_char: char, val: &str) -> Option<()> {
    let ids = val.split(',').all(is_hex32);

    builder.start_tag_field(tag_char).ok()?;

    for elem in val.split(',') {
        if ids {
            let bytes = hex::decode(elem).ok()?;
            let id: &[u8; 32] = bytes.as_slice().try_into().ok()?;
            builder.add_id_element(id).ok()?;
        } else {
            let decoded = urlencoding::decode(elem).ok()?;
            builder.add_str_element(&decoded).ok()?;
        }
    }

    builder.end_field();

    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render the tag fields of a filter including each element's *type*.
    ///
    /// The querystring is deliberately type-erased: an `Id` element and a
    /// `Str` element holding the same 64 hex characters encode to identical
    /// text. Comparing two filters by re-encoding them therefore cannot see a
    /// decoder that produced the wrong element type, so round-trip assertions
    /// need this alongside the querystring.
    fn tag_elements(filter: &Filter) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();

        for field in filter {
            let FilterField::Tags(tag_char, elements) = field else {
                continue;
            };

            for elem in elements {
                out.push(match elem {
                    nostrdb::FilterElement::Str(s) => format!("#{tag_char}=str:{s}"),
                    nostrdb::FilterElement::Id(id) => {
                        format!("#{tag_char}=id:{}", hex::encode(id))
                    }
                    nostrdb::FilterElement::Int(n) => format!("#{tag_char}=int:{n}"),
                    nostrdb::FilterElement::Custom => format!("#{tag_char}=custom"),
                });
            }
        }

        out.sort();
        out
    }

    /// Compare two filters by their canonical querystring *and* by the element
    /// types the querystring cannot express.
    fn assert_filters_eq(a: &Filter, b: &Filter) {
        assert_eq!(filter_to_querystring(a), filter_to_querystring(b));
        assert_eq!(
            tag_elements(a),
            tag_elements(b),
            "tag element types differ despite identical querystrings"
        );
    }

    #[test]
    fn test_roundtrip_simple() {
        let filter = Filter::new().kinds([1]).limit(50).build();

        let encoded = filter_to_querystring(&filter);
        assert_eq!(encoded, "kinds=1&limit=50");

        let decoded = filter_from_querystring(&encoded).expect("decode failed");
        assert_filters_eq(&filter, &decoded);
    }

    #[test]
    fn test_roundtrip_with_tags() {
        let filter = Filter::new()
            .kinds([1])
            .tags(["bitcoin", "nostr"], 't')
            .limit(100)
            .build();

        let encoded = filter_to_querystring(&filter);
        assert!(encoded.contains("#t="));
        assert!(encoded.contains("kinds=1"));

        let decoded = filter_from_querystring(&encoded).expect("decode failed");
        assert_filters_eq(&filter, &decoded);
    }

    #[test]
    fn test_roundtrip_with_authors() {
        let pk = [0xab_u8; 32];
        let filter = Filter::new().kinds([1]).authors([&pk]).limit(10).build();

        let encoded = filter_to_querystring(&filter);
        assert!(encoded.contains("authors="));
        assert!(encoded.contains(&hex::encode(pk)));

        let decoded = filter_from_querystring(&encoded).expect("decode failed");
        assert_filters_eq(&filter, &decoded);
    }

    #[test]
    fn test_roundtrip_with_search() {
        let filter = Filter::new().search("hello world").kinds([1]).build();

        let encoded = filter_to_querystring(&filter);
        assert!(encoded.contains("search=hello%20world"));

        let decoded = filter_from_querystring(&encoded).expect("decode failed");
        assert_filters_eq(&filter, &decoded);
    }

    #[test]
    fn test_canonical_sorting() {
        let filter = Filter::new().search("test").kinds([7, 1]).limit(10).build();

        let encoded = filter_to_querystring(&filter);
        // Keys should be sorted: kinds < limit < search
        let keys: Vec<&str> = encoded
            .split('&')
            .filter_map(|p| p.split('=').next())
            .collect();
        let mut sorted_keys = keys.clone();
        sorted_keys.sort();
        assert_eq!(keys, sorted_keys, "keys should be alphabetically sorted");

        // Kind values should be sorted too
        assert!(encoded.contains("kinds=1,7"));
    }

    #[test]
    fn test_empty_returns_none() {
        assert!(filter_from_querystring("").is_none());
    }

    #[test]
    fn test_human_readable() {
        let filter = Filter::new()
            .kinds([1])
            .tags(["bitcoin"], 't')
            .limit(50)
            .build();

        let encoded = filter_to_querystring(&filter);
        assert!(
            encoded.contains("kinds=1"),
            "querystring should be readable: {encoded}"
        );
        assert!(
            encoded.contains("#t=bitcoin"),
            "querystring should be readable: {encoded}"
        );
    }

    /// The known gap in the format, pinned so a change to it is deliberate.
    ///
    /// A `#d` value that is genuinely a string but happens to be 64 hex
    /// characters comes back as a 32-byte id, because the encoding has no
    /// syntax that distinguishes the two. Closing this needs the encoder to
    /// mark the difference, which is a format change and so a decision for the
    /// spec — see the module documentation. Until then this asserts what
    /// actually happens rather than what we would like to happen.
    #[test]
    fn test_tag_str_64hex_does_not_roundtrip() {
        let hexish = "a".repeat(64);
        let filter = Filter::new().tags([hexish.as_str()], 'd').build();

        let encoded = filter_to_querystring(&filter);
        assert_eq!(encoded, format!("#d={hexish}"));

        let decoded = filter_from_querystring(&encoded).expect("decode failed");

        // the text round-trips ...
        assert_eq!(filter_to_querystring(&decoded), encoded);
        // ... but the element type does not.
        assert_eq!(tag_elements(&filter), vec![format!("#d=str:{hexish}")]);
        assert_eq!(tag_elements(&decoded), vec![format!("#d=id:{hexish}")]);
    }

    /// The escape hatch that falls out of testing the raw member: percent-encode
    /// any one character and the value decodes as the string it is.
    #[test]
    fn test_percent_encoding_forces_a_string_tag_value() {
        let hexish = "a".repeat(64);
        let escaped = format!("%61{}", "a".repeat(63));

        let decoded = filter_from_querystring(&format!("#d={escaped}")).expect("decode failed");

        assert_eq!(tag_elements(&decoded), vec![format!("#d=str:{hexish}")]);
    }

    /// An all-ids tag list keeps its id typing.
    #[test]
    fn test_tag_id_list_roundtrip() {
        let a = hex::encode([0x11_u8; 32]);
        let b = hex::encode([0x22_u8; 32]);

        let decoded = filter_from_querystring(&format!("#e={a},{b}")).expect("decode failed");

        assert_eq!(
            tag_elements(&decoded),
            vec![format!("#e=id:{a}"), format!("#e=id:{b}")]
        );
    }

    /// A tag field mixing a 64-hex value with a shorter one. nostrdb requires
    /// the elements of one field to share a type, so guessing per-element makes
    /// the whole filter fail to decode rather than just mis-typing an element.
    #[test]
    fn test_tag_mixed_value_lengths_roundtrip() {
        let hexish = "b".repeat(64);
        let filter = Filter::new().tags([hexish.as_str(), "short"], 'd').build();

        let encoded = filter_to_querystring(&filter);
        let decoded = filter_from_querystring(&encoded).expect("decode failed");

        assert_filters_eq(&filter, &decoded);
    }

    /// An unrecognized key must not be dropped on the floor: silently ignoring
    /// a constraint yields a filter *wider* than the one that was asked for.
    #[test]
    fn test_unknown_key_is_rejected() {
        assert!(filter_from_querystring("kinds=1&frobnicate=9").is_none());
    }

    /// The dangerous form of the previous case. `TimelineKind` uses
    /// `filter_from_querystring(..).is_some()` to decide whether a token is a
    /// filter, so an arbitrary `k=v` token that decodes to a *field-less*
    /// filter is both misread as a filter and matches every note.
    #[test]
    fn test_unrelated_token_is_not_a_filter() {
        assert!(filter_from_querystring("foo=bar").is_none());
    }

    /// Tag keys are a single letter. `#tt` must not silently decode as `#t`.
    #[test]
    fn test_multichar_tag_key_is_rejected() {
        assert!(filter_from_querystring("#tt=bitcoin").is_none());
    }

    /// A repeated key is ambiguous — last-wins, first-wins and merge are all
    /// defensible, so reject rather than pick one silently.
    #[test]
    fn test_duplicate_key_is_rejected() {
        assert!(filter_from_querystring("kinds=1&kinds=2").is_none());
        assert!(filter_from_querystring("#t=a&#t=b").is_none());
    }
}
