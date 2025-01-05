use crate::timeline::kind::PubkeySource;
use enostr::Pubkey;

#[derive(Debug, Clone)]
pub struct UnexpectedToken<'fnd, 'exp> {
    pub expected: &'exp str,
    pub found: &'fnd str,
}

#[derive(Debug, Clone)]
pub enum ParseError<'a> {
    /// Not done parsing yet
    Incomplete,

    /// All parsing options failed
    AltAllFailed,

    /// There was some issue decoding the data
    DecodeFailed,

    /// We encountered an unexpected token
    UnexpectedToken(UnexpectedToken<'a, 'static>),

    /// No more tokens
    EOF,
}

pub struct TokenWriter {
    delim: &'static str,
    tokens_written: usize,
    buf: Vec<u8>,
}

impl Default for TokenWriter {
    fn default() -> Self {
        Self::new(":")
    }
}

impl TokenWriter {
    pub fn new(delim: &'static str) -> Self {
        let buf = vec![];
        let tokens_written = 0;
        Self {
            buf,
            tokens_written,
            delim,
        }
    }

    pub fn write_token(&mut self, token: &str) {
        if self.tokens_written > 0 {
            self.buf.extend_from_slice(self.delim.as_bytes())
        }
        self.buf.extend_from_slice(token.as_bytes());
        self.tokens_written += 1;
    }

    pub fn str(&self) -> &str {
        // SAFETY: only &strs are ever serialized, so its guaranteed to be
        // correct here
        unsafe { std::str::from_utf8_unchecked(self.buffer()) }
    }

    pub fn buffer(&self) -> &[u8] {
        &self.buf
    }
}

#[derive(Clone)]
pub struct TokenParser<'a> {
    tokens: &'a [&'a str],
    index: usize,
}

fn _parse_pubkey_src_tokens<'a>(
    parser: &mut TokenParser<'a>,
) -> Result<PubkeySource, ParseError<'a>> {
    match parser.pull_token() {
        // we handle bare payloads and assume they are explicit pubkey sources
        Ok("explicit") => {
            let hex_str = parser.pull_token()?;
            Pubkey::from_hex(hex_str)
                .map_err(|_| ParseError::DecodeFailed)
                .map(PubkeySource::Explicit)
        }

        Err(ParseError::EOF) | Ok("deck_author") => Ok(PubkeySource::DeckAuthor),

        Ok(hex_payload) => Pubkey::from_hex(hex_payload)
            .map_err(|_| ParseError::DecodeFailed)
            .map(PubkeySource::Explicit),

        Err(e) => Err(e),
    }
}

impl<'a> TokenParser<'a> {
    /// alt tries each parser in `routes` until one succeeds.
    /// If all fail, returns `ParseError::AltAllFailed`.
    #[allow(clippy::type_complexity)]
    pub fn alt<R>(
        parser: &mut TokenParser<'a>,
        routes: &[fn(&mut TokenParser<'a>) -> Result<R, ParseError<'a>>],
    ) -> Result<R, ParseError<'a>> {
        let start = parser.index;
        for route in routes {
            match route(parser) {
                Ok(r) => return Ok(r), // if success, stop trying more routes
                Err(_) => {
                    // revert index & try next route
                    parser.index = start;
                }
            }
        }
        // if we tried them all and none succeeded
        Err(ParseError::AltAllFailed)
    }

    pub fn new(tokens: &'a [&'a str]) -> Self {
        let index = 0;
        Self { tokens, index }
    }

    pub fn parse_token(&mut self, expected: &'static str) -> Result<&'a str, ParseError<'a>> {
        let found = self.pull_token()?;
        if found == expected {
            Ok(found)
        } else {
            Err(ParseError::UnexpectedToken(UnexpectedToken {
                expected,
                found,
            }))
        }
    }

    /// “Parse all” meaning: run the provided closure. If it fails, revert
    /// the index.
    pub fn parse_all<R>(
        &mut self,
        parse_fn: impl FnOnce(&mut Self) -> Result<R, ParseError<'a>>,
    ) -> Result<R, ParseError<'a>> {
        let start = self.index;
        let result = parse_fn(self);

        // If the parser closure fails, revert the index
        if result.is_err() {
            self.index = start;
            result
        } else if !self.is_eof() {
            Err(ParseError::Incomplete)
        } else {
            result
        }
    }

    pub fn pull_token(&mut self) -> Result<&'a str, ParseError<'a>> {
        let token = self
            .tokens
            .get(self.index)
            .copied()
            .ok_or(ParseError::EOF)?;
        self.index += 1;
        Ok(token)
    }

    pub fn unpop_token(&mut self) {
        if (self.index as isize) - 1 < 0 {
            return;
        }

        self.index -= 1;
    }

    #[inline]
    pub fn tokens(&self) -> &'a [&'a str] {
        let min_index = self.index.min(self.tokens.len());
        &self.tokens[min_index..]
    }

    #[inline]
    pub fn is_eof(&self) -> bool {
        self.tokens().is_empty()
    }
}

pub trait TokenSerializable: Sized {
    /// Return a list of serialization plans for a type. We do this for
    /// type safety and assume constructing these types are lightweight
    fn parse<'a>(parser: &mut TokenParser<'a>) -> Result<Self, ParseError<'a>>;
    fn serialize(&self, writer: &mut TokenWriter);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_column_serialize() {
        use crate::ui::add_column::{AddAlgoRoute, AddColumnRoute};

        {
            let data_str = "column:algo_selection:last_per_pubkey";
            let data = &data_str.split(":").collect::<Vec<&str>>();
            let mut token_writer = TokenWriter::default();
            let mut parser = TokenParser::new(&data);
            let parsed = AddColumnRoute::parse(&mut parser).unwrap();
            let expected = AddColumnRoute::Algo(AddAlgoRoute::LastPerPubkey);
            parsed.serialize(&mut token_writer);
            assert_eq!(expected, parsed);
            assert_eq!(token_writer.str(), data_str);
        }

        {
            let data_str = "column";
            let mut token_writer = TokenWriter::default();
            let data: &[&str] = &[data_str];
            let mut parser = TokenParser::new(data);
            let parsed = AddColumnRoute::parse(&mut parser).unwrap();
            let expected = AddColumnRoute::Base;
            parsed.serialize(&mut token_writer);
            assert_eq!(expected, parsed);
            assert_eq!(token_writer.str(), data_str);
        }
    }
}
