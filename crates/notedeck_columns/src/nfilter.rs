//! The **nfilter** encoding of a single NIP-01 filter as a query string.
//!
//! This module is the reference implementation. The format itself is specified
//! in `docs/nfilter.md` in [damus-api][api], which is the normative document —
//! when the two disagree, the spec is right and this is a bug. What follows is
//! only what an implementation has to decide that the format does not.
//!
//! [api]: https://github.com/damus-io/api
//!
//! # Shape, in one line
//!
//! `key=value` pairs joined with `&`, values comma-joined, everything sorted:
//! `#t=bitcoin,nostr&kinds=1&limit=100`. Keys are NIP-01's attribute names plus
//! `search` and `relays`, and `#x` for a single-letter tag filter.
//!
//! # Typing tag elements, and the wart that follows
//!
//! The spec is explicit that an element is *text* and that the encoding does
//! not distinguish a 32-byte id from a string of the same 64 characters —
//! NIP-01 does not distinguish them either. nostrdb's filter model does, so
//! this decoder has to make the choice the format declines to make, and the
//! spec requires such a decoder to document its rule. This is that rule:
//!
//! > A tag list decodes to ids if **every** member is 64 hex characters, and to
//! > strings otherwise.
//!
//! It is a property of the whole list rather than of each member because
//! nostrdb requires the members of one field to share a type; a per-member
//! choice builds a mixed field, which nostrdb rejects outright. It cannot be
//! taken from the first member either — which is what nostrdb's own JSON filter
//! parser does — because canonical form sorts the members, so "first" is not
//! stable information about what the encoder meant.
//!
//! The consequence is the wart the spec names: **a `#d` value that is genuinely
//! a string and happens to be 64 hex characters decodes as an id**, and does
//! not survive a round trip. Pinned by `test_tag_str_64hex_does_not_roundtrip`.
//!
//! The test is applied to the raw, still-encoded member, so percent-encoding
//! any one character of a value forces it to decode as the string it is.
//!
//! # What this implementation cannot encode
//!
//! - **A union.** [`filter_to_querystring`] takes one filter, which is the
//!   format's own decision, but [`crate::timeline::kind::FilterVec`] keeps a
//!   `Vec` and carries the list itself.
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
                arr.dedup();
                if !arr.is_empty() {
                    pairs.push(("ids".to_string(), arr.join(",")));
                }
            }
            FilterField::Authors(authors) => {
                let mut arr: Vec<String> = authors.into_iter().map(hex::encode).collect();
                arr.sort();
                arr.dedup();
                if !arr.is_empty() {
                    pairs.push(("authors".to_string(), arr.join(",")));
                }
            }
            FilterField::Kinds(kinds) => {
                let mut arr: Vec<u64> = kinds.into_iter().collect();
                arr.sort();
                arr.dedup();
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
                arr.dedup();
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
                arr.dedup();
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

        // An empty value reads as either "matches nothing" or "no constraint",
        // and those two answers differ by the whole store.
        if val.is_empty() {
            return None;
        }

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
                let decoded = decode_element(val)?;
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
                    let decoded = decode_element(relay)?;
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

/// The single letter of a `#x` tag key, or `None` if `key` is not one.
///
/// NIP-01 tag filters are one letter, so `#tt` is rejected rather than read as
/// `#t` with trailing junk, and `#1` is rejected rather than quietly becoming a
/// filter on a tag nobody indexes.
fn tag_key_char(key: &str) -> Option<char> {
    let mut chars = key.strip_prefix('#')?.chars();
    let tag_char = chars.next().filter(char::is_ascii_alphabetic)?;
    chars.next().is_none().then_some(tag_char)
}

/// Percent-decode one element.
///
/// [`urlencoding::decode`] is not usable here: it passes a malformed escape
/// through as the literal text `%zz`, where the format requires rejecting it,
/// and it does not read `+` as a space, which the format requires because it
/// spells itself `application/x-www-form-urlencoded`. An encoder never emits a
/// bare `+` — a literal plus is not unreserved and so encodes as `%2B` — so
/// this only ever differs from plain percent-decoding on input some other
/// encoder produced.
fn decode_element(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                // `u8::from_str_radix` would accept a leading sign, so the two
                // digits are checked rather than left to the parse.
                let digits = s.get(i + 1..i + 3)?;
                if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return None;
                }
                out.push(u8::from_str_radix(digits, 16).ok()?);
                i += 3;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }

    String::from_utf8(out).ok()
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
            let decoded = decode_element(elem)?;
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

    /// An empty value reads as either "matches nothing" or "no constraint",
    /// and the two differ by the entire store.
    #[test]
    fn test_empty_value_is_rejected() {
        for qs in ["kinds=", "#t=", "search=", "relays=", "ids=", "limit="] {
            assert!(
                filter_from_querystring(qs).is_none(),
                "expected {qs} to be rejected"
            );
        }
    }

    /// Tag keys are one *letter*. `#1` is not a tag filter.
    #[test]
    fn test_non_letter_tag_key_is_rejected() {
        assert!(filter_from_querystring("#1=x").is_none());
        assert!(filter_from_querystring("#-=x").is_none());
        assert!(filter_from_querystring("#=x").is_none());
    }

    /// `+` is a space, as the media type the format names defines it.
    #[test]
    fn test_plus_decodes_as_space() {
        let decoded = filter_from_querystring("search=hello+world").expect("decode failed");
        assert_eq!(filter_to_querystring(&decoded), "search=hello%20world");
    }

    /// A malformed escape must be refused, not passed through as literal text.
    #[test]
    fn test_malformed_escape_is_rejected() {
        for qs in ["#t=%zz", "#t=%2", "#t=%", "#t=%+2", "search=%zz"] {
            assert!(
                filter_from_querystring(qs).is_none(),
                "expected {qs} to be rejected"
            );
        }
    }

    /// An escape that decodes to something that is not UTF-8 is refused.
    #[test]
    fn test_non_utf8_escape_is_rejected() {
        assert!(filter_from_querystring("#t=%FF").is_none());
    }

    /// Canonical form deduplicates set-valued fields: "a repeat contributes
    /// nothing", so it must not contribute a second spelling of one query.
    #[test]
    fn test_canonical_form_deduplicates() {
        let filter = Filter::new()
            .kinds([7, 1, 7])
            .tags(["nostr", "bitcoin", "nostr"], 't')
            .build();

        assert_eq!(filter_to_querystring(&filter), "#t=bitcoin,nostr&kinds=1,7");
    }

    /// A decoder must accept a non-canonical spelling and read it as the query
    /// it denotes — being non-canonical costs a cache miss, not an error.
    #[test]
    fn test_non_canonical_input_is_accepted() {
        // unsorted pairs, unsorted elements, lowercase escapes, leading zeros
        let decoded =
            filter_from_querystring("limit=010&kinds=7,1&#t=nostr,%62itcoin").expect("decode");

        assert_eq!(
            filter_to_querystring(&decoded),
            "#t=bitcoin,nostr&kinds=1,7&limit=10"
        );
    }
}
