use nostrdb::{Filter, FilterField};

/// Encode a single Filter to a canonical querystring.
///
/// Keys are sorted alphabetically for a stable, canonical representation.
/// Array values (ids, authors, kinds, tags) are comma-separated and sorted.
/// Binary values (ids, authors) are hex-encoded.
/// String values are percent-encoded.
///
/// Example: `#t=bitcoin,nostr&kinds=1&limit=100`
pub fn filter_to_querystring(filter: &Filter) -> String {
    let mut pairs: Vec<(String, String)> = Vec::new();

    for field in filter {
        match field {
            FilterField::Ids(ids) => {
                let mut arr: Vec<String> = ids.into_iter().map(|id| hex::encode(id)).collect();
                arr.sort();
                if !arr.is_empty() {
                    pairs.push(("ids".to_string(), arr.join(",")));
                }
            }
            FilterField::Authors(authors) => {
                let mut arr: Vec<String> = authors.into_iter().map(|id| hex::encode(id)).collect();
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

/// Decode a querystring to a Filter.
pub fn filter_from_querystring(qs: &str) -> Option<Filter> {
    if qs.is_empty() {
        return None;
    }

    let mut builder = Filter::new();

    for pair in qs.split('&') {
        let (key, val) = pair.split_once('=')?;

        match key {
            "ids" => {
                builder.start_ids_field().ok()?;
                for hex_id in val.split(',') {
                    let bytes = hex::decode(hex_id).ok()?;
                    let id: &[u8; 32] = bytes.as_slice().try_into().ok()?;
                    builder.add_id_element(id).ok()?;
                }
                builder.end_field();
            }
            "authors" => {
                builder.start_authors_field().ok()?;
                for hex_id in val.split(',') {
                    let bytes = hex::decode(hex_id).ok()?;
                    let id: &[u8; 32] = bytes.as_slice().try_into().ok()?;
                    builder.add_id_element(id).ok()?;
                }
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
            tag_key if tag_key.starts_with('#') => {
                let tag_char = tag_key.chars().nth(1)?;
                builder.start_tag_field(tag_char).ok()?;
                for elem in val.split(',') {
                    // 64 hex chars = 32-byte id, otherwise treat as string
                    if elem.len() == 64 && elem.chars().all(|c| c.is_ascii_hexdigit()) {
                        let bytes = hex::decode(elem).ok()?;
                        let id: &[u8; 32] = bytes.as_slice().try_into().ok()?;
                        builder.add_id_element(id).ok()?;
                    } else {
                        let decoded = urlencoding::decode(elem).ok()?;
                        builder.add_str_element(&decoded).ok()?;
                    }
                }
                builder.end_field();
            }
            _ => {}
        }
    }

    Some(builder.build())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compare two filters by re-encoding to querystring (canonical/sorted)
    fn assert_filters_eq(a: &Filter, b: &Filter) {
        let a_qs = filter_to_querystring(a);
        let b_qs = filter_to_querystring(b);
        assert_eq!(a_qs, b_qs);
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
}
