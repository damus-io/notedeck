use enostr::Pubkey;
use nostrdb::{Filter, FilterBuilder};
use rmpv::Value;
use tokenator::{ParseError, TokenParser, TokenSerializable, TokenWriter};

#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub struct SearchQuery {
    author: Option<Pubkey>,
    pub search: String,
}

impl TokenSerializable for SearchQuery {
    fn serialize_tokens(&self, writer: &mut TokenWriter) {
        writer.write_token(&self.to_nfilter())
    }

    fn parse_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Self, ParseError<'a>> {
        if let Some(query) = SearchQuery::from_nfilter(parser.pull_token()?) {
            Ok(query)
        } else {
            Err(ParseError::DecodeFailed)
        }
    }
}

impl SearchQuery {
    pub fn new(search: String) -> Self {
        let author: Option<Pubkey> = None;
        Self { search, author }
    }
    /// Convert the query to a filter-compatible MessagePack value
    fn to_msgpack_value(&self) -> Value {
        let mut values: Vec<(Value, Value)> = Vec::with_capacity(2);
        let search_str: &str = &self.search;
        values.push(("search".into(), search_str.into()));
        if let Some(pubkey) = self.author() {
            values.push((
                "authors".into(),
                Value::Array(vec![Value::Binary(pubkey.bytes().to_vec())]),
            ))
        }

        Value::Map(values)
    }

    pub fn to_nfilter(&self) -> String {
        let hrp = bech32::Hrp::parse_unchecked("nfilter");
        let msgpack_value = self.to_msgpack_value();
        let mut buf = vec![];
        rmpv::encode::write_value(&mut buf, &msgpack_value)
            .expect("expected nfilter to encode ok. too big?");

        bech32::encode::<bech32::Bech32>(hrp, &buf).expect("expected bech32 nfilter to encode ok")
    }

    fn decode_value(value: &Value) -> Option<Self> {
        let mut search: Option<String> = None;
        let mut author: Option<Pubkey> = None;

        let values = if let Value::Map(values) = value {
            values
        } else {
            return None;
        };

        for (key, value) in values {
            let key_str: &str = if let Value::String(s) = key {
                s.as_str()?
            } else {
                continue;
            };

            if key_str == "search" {
                if let Value::String(search_str) = value {
                    search = search_str.clone().into_str();
                } else {
                    continue;
                }
            } else if key_str == "authors" {
                let authors = if let Value::Array(authors) = value {
                    authors
                } else {
                    continue;
                };

                let author_value = if let Some(author_value) = authors.first() {
                    author_value
                } else {
                    continue;
                };

                let author_bytes: &[u8] = if let Value::Binary(author_bytes) = author_value {
                    author_bytes
                } else {
                    continue;
                };

                let pubkey = Pubkey::new(author_bytes.try_into().ok()?);
                author = Some(pubkey);
            }
        }

        let search = search?;

        Some(Self { search, author })
    }

    pub fn filter(&self) -> FilterBuilder {
        Filter::new().search(&self.search).kinds([1])
    }

    pub fn from_nfilter(nfilter: &str) -> Option<Self> {
        let (hrp, msgpack_data) = bech32::decode(nfilter).ok()?;
        if hrp.as_str() != "nfilter" {
            return None;
        }

        let value = rmpv::decode::read_value(&mut &msgpack_data[..]).ok()?;

        Self::decode_value(&value)
    }

    pub fn author(&self) -> Option<&Pubkey> {
        self.author.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::Pubkey;
    use rmpv::Value;
    use tokenator::{TokenParser, TokenSerializable, TokenWriter};

    fn test_pubkey() -> Pubkey {
        let bytes: [u8; 32] = [1; 32]; // Example public key
        Pubkey::new(bytes)
    }

    #[test]
    fn test_to_msgpack_value() {
        let query = SearchQuery {
            author: Some(test_pubkey()),
            search: "nostrdb".to_string(),
        };
        let msgpack_value = query.to_msgpack_value();

        if let Value::Map(values) = msgpack_value {
            assert!(
                values
                    .iter()
                    .any(|(k, v)| *k == Value::String("search".into())
                        && *v == Value::String("nostrdb".into()))
            );
            assert!(
                values
                    .iter()
                    .any(|(k, _v)| *k == Value::String("authors".into()))
            );
        } else {
            panic!("Failed to encode SearchQuery to MessagePack");
        }
    }

    #[test]
    fn test_to_nfilter() {
        let query = SearchQuery {
            author: Some(test_pubkey()),
            search: "nostrdb".to_string(),
        };
        let encoded = query.to_nfilter();
        assert!(encoded.starts_with("nfilter"), "nfilter encoding failed");
    }

    #[test]
    fn test_from_nfilter() {
        let query = SearchQuery {
            author: Some(test_pubkey()),
            search: "nostrdb".to_string(),
        };
        let encoded = query.to_nfilter();
        let decoded = SearchQuery::from_nfilter(&encoded).expect("Failed to decode nfilter");
        assert_eq!(query, decoded);
    }

    #[test]
    fn test_nfilter_roundtrip() {
        let queries = vec![
            SearchQuery {
                author: None,
                search: "nostrdb".to_string(),
            },
            SearchQuery {
                author: Some(test_pubkey()),
                search: "test".to_string(),
            },
        ];

        for query in queries {
            let encoded = query.to_nfilter();
            let decoded =
                SearchQuery::from_nfilter(&encoded).expect("Failed to decode valid nfilter");
            assert_eq!(query, decoded, "Roundtrip encoding/decoding failed");
        }
    }

    #[test]
    fn test_invalid_nfilter() {
        let invalid_nfilter = "nfilter1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
        assert!(SearchQuery::from_nfilter(invalid_nfilter).is_none());
    }

    #[test]
    fn test_invalid_hrp() {
        let invalid_nfilter = "invalid1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq";
        assert!(SearchQuery::from_nfilter(invalid_nfilter).is_none());
    }

    #[test]
    fn test_parse_from_tokens() {
        let query = SearchQuery {
            author: Some(test_pubkey()),
            search: "nostrdb".to_string(),
        };
        let mut writer = TokenWriter::default();
        query.serialize_tokens(&mut writer);
        let tokens = [writer.str()];
        let mut parser = TokenParser::new(&tokens);

        let parsed =
            SearchQuery::parse_from_tokens(&mut parser).expect("Failed to parse from tokens");

        assert_eq!(query, parsed);
    }

    #[test]
    fn test_parse_from_invalid_tokens() {
        let mut parser = TokenParser::new(&[]);
        assert!(SearchQuery::parse_from_tokens(&mut parser).is_err());
    }
}
