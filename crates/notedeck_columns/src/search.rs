use crate::nfilter::{filter_from_querystring, filter_to_querystring};
use enostr::Pubkey;
use nostrdb::{Filter, FilterBuilder, FilterField};
use tokenator::{ParseError, TokenParser, TokenSerializable, TokenWriter};

#[derive(Debug, Eq, PartialEq, Clone, Hash)]
pub struct SearchQuery {
    author: Option<Pubkey>,
    pub search: String,
}

impl TokenSerializable for SearchQuery {
    fn serialize_tokens(&self, writer: &mut TokenWriter) {
        writer.write_token(&self.to_querystring())
    }

    fn parse_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Self, ParseError<'a>> {
        if let Some(query) = SearchQuery::from_querystring(parser.pull_token()?) {
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

    pub fn filter(&self) -> FilterBuilder {
        let mut builder = Filter::new().search(&self.search).kinds([1]);
        if let Some(pk) = self.author() {
            builder = builder.authors([pk.bytes()]);
        }
        builder
    }

    fn to_filter(&self) -> Filter {
        self.filter().build()
    }

    pub fn to_querystring(&self) -> String {
        filter_to_querystring(&self.to_filter())
    }

    pub fn from_querystring(qs: &str) -> Option<Self> {
        let filter = filter_from_querystring(qs)?;

        let mut search: Option<String> = None;
        let mut author: Option<Pubkey> = None;

        for field in &filter {
            match field {
                FilterField::Search(s) => {
                    search = Some(s.to_string());
                }
                FilterField::Authors(authors) => {
                    if let Some(pk_bytes) = authors.into_iter().next() {
                        author = Some(Pubkey::new(*pk_bytes));
                    }
                }
                _ => {}
            }
        }

        Some(Self {
            search: search?,
            author,
        })
    }

    pub fn author(&self) -> Option<&Pubkey> {
        self.author.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use enostr::Pubkey;
    use tokenator::{TokenParser, TokenSerializable, TokenWriter};

    fn test_pubkey() -> Pubkey {
        let bytes: [u8; 32] = [1; 32];
        Pubkey::new(bytes)
    }

    #[test]
    fn test_to_querystring() {
        let query = SearchQuery {
            author: Some(test_pubkey()),
            search: "nostrdb".to_string(),
        };
        let encoded = query.to_querystring();
        assert!(encoded.contains("search=nostrdb"));
        assert!(encoded.contains("authors="));
    }

    #[test]
    fn test_from_querystring() {
        let query = SearchQuery {
            author: Some(test_pubkey()),
            search: "nostrdb".to_string(),
        };
        let encoded = query.to_querystring();
        let decoded = SearchQuery::from_querystring(&encoded).expect("Failed to decode");
        assert_eq!(query, decoded);
    }

    #[test]
    fn test_querystring_roundtrip() {
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
            let encoded = query.to_querystring();
            let decoded = SearchQuery::from_querystring(&encoded).expect("Failed to decode");
            assert_eq!(query, decoded, "Roundtrip encoding/decoding failed");
        }
    }

    #[test]
    fn test_invalid_querystring() {
        assert!(SearchQuery::from_querystring("").is_none());
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
