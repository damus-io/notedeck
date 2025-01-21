use crate::timeline::kind::PubkeySource;
use enostr::{NoteId, Pubkey};

#[derive(Debug, Clone)]
pub struct UnexpectedToken<'fnd, 'exp> {
    pub expected: &'exp str,
    pub found: &'fnd str,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum TokenPayload {
    PubkeySource,
    Pubkey,
    NoteId,
}

pub struct TokenAlternatives {
    /// This is the preferred token. It should be serialized this way
    preferred: &'static str,

    /// These are deprecated tokens that should still be handled and parsed
    deprecated: &'static [&'static str],
}

impl TokenAlternatives {
    pub const fn new(preferred: &'static str, deprecated: &'static [&'static str]) -> Self {
        Self {
            preferred,
            deprecated,
        }
    }
}

/// Token is a unified serialization helper. By specifying a list of
/// tokens for each thing you want to parse, you can type-safely parse
/// and serialize things
pub enum Token {
    /// A simple identifier
    Identifier(&'static str),

    /// There are multiple ways to parse this identifier
    Alternatives(TokenAlternatives),

    /// Different payload types, pubkeys etc
    Payload(TokenPayload),
}

#[derive(Debug, Clone)]
pub enum Payload {
    PubkeySource(PubkeySource),
    Pubkey(Pubkey),
    NoteId(NoteId),
}

impl Payload {
    pub fn token_payload(&self) -> TokenPayload {
        match self {
            Payload::PubkeySource(_) => TokenPayload::PubkeySource,
            Payload::Pubkey(_) => TokenPayload::Pubkey,
            Payload::NoteId(_) => TokenPayload::NoteId,
        }
    }

    pub fn parse_note_id(payload: Option<Payload>) -> Result<NoteId, ParseError<'static>> {
        payload
            .and_then(|p| p.get_note_id().cloned())
            .ok_or(ParseError::ExpectedPayload(TokenPayload::NoteId))
    }

    pub fn parse_pubkey(payload: Option<Payload>) -> Result<Pubkey, ParseError<'static>> {
        payload
            .and_then(|p| p.get_pubkey().cloned())
            .ok_or(ParseError::ExpectedPayload(TokenPayload::Pubkey))
    }

    pub fn parse_pubkey_source(
        payload: Option<Payload>,
    ) -> Result<PubkeySource, ParseError<'static>> {
        payload
            .and_then(|p| p.get_pubkey_source().cloned())
            .ok_or(ParseError::ExpectedPayload(TokenPayload::Pubkey))
    }

    pub fn parse<'a>(
        expected: TokenPayload,
        parser: &mut TokenParser<'a>,
    ) -> Result<Self, ParseError<'a>> {
        match expected {
            TokenPayload::PubkeySource => Ok(Payload::pubkey_source(
                PubkeySource::parse_from_tokens(parser)?,
            )),
            TokenPayload::Pubkey => {
                let pubkey = parser.try_parse(|p| {
                    let hex = p.pull_token()?;
                    Pubkey::from_hex(hex).map_err(|_| ParseError::HexDecodeFailed)
                })?;

                Ok(Payload::pubkey(pubkey))
            }
            TokenPayload::NoteId => {
                let note_id = parser.try_parse(|p| {
                    let hex = p.pull_token()?;
                    NoteId::from_hex(hex).map_err(|_| ParseError::HexDecodeFailed)
                })?;

                Ok(Payload::note_id(note_id))
            }
        }
    }

    pub fn pubkey(pubkey: Pubkey) -> Self {
        Self::Pubkey(pubkey)
    }

    pub fn pubkey_source(pubkey_src: PubkeySource) -> Self {
        Self::PubkeySource(pubkey_src)
    }

    pub fn note_id(note_id: NoteId) -> Self {
        Self::NoteId(note_id)
    }

    pub fn get_pubkey(&self) -> Option<&Pubkey> {
        if let Self::Pubkey(pubkey) = self {
            Some(pubkey)
        } else {
            None
        }
    }

    pub fn get_pubkey_source(&self) -> Option<&PubkeySource> {
        if let Self::PubkeySource(pk_src) = self {
            Some(pk_src)
        } else {
            None
        }
    }

    pub fn get_note_id(&self) -> Option<&NoteId> {
        if let Self::NoteId(note_id) = self {
            Some(note_id)
        } else {
            None
        }
    }
}

impl Token {
    pub fn parse<'a>(
        &self,
        parser: &mut TokenParser<'a>,
    ) -> Result<Option<Payload>, ParseError<'a>> {
        match self {
            Token::Identifier(s) => {
                parser.parse_token(s)?;
                Ok(None)
            }

            Token::Payload(payload) => {
                let payload = Payload::parse(*payload, parser)?;
                Ok(Some(payload))
            }

            Token::Alternatives(alts) => {
                if parser.try_parse(|p| p.parse_token(alts.preferred)).is_ok() {
                    return Ok(None);
                }

                for token in alts.deprecated {
                    if parser.try_parse(|p| p.parse_token(token)).is_ok() {
                        return Ok(None);
                    }
                }

                Err(ParseError::AltAllFailed)
            }
        }
    }

    /// Parse all of the tokens in sequence, ensuring that we extract a payload
    /// if we find one. This only handles a single payload, if you need more,
    /// then use a custom parser
    pub fn parse_all<'a>(
        parser: &mut TokenParser<'a>,
        tokens: &[Token],
    ) -> Result<Option<Payload>, ParseError<'a>> {
        parser.try_parse(|p| {
            let mut payload: Option<Payload> = None;
            for token in tokens {
                if let Some(pl) = token.parse(p)? {
                    payload = Some(pl);
                }
            }

            Ok(payload)
        })
    }

    pub fn serialize_all(writer: &mut TokenWriter, tokens: &[Token], payload: Option<&Payload>) {
        for token in tokens {
            token.serialize(writer, payload)
        }
    }

    pub fn serialize(&self, writer: &mut TokenWriter, payload: Option<&Payload>) {
        match self {
            Token::Identifier(s) => writer.write_token(s),
            Token::Alternatives(alts) => writer.write_token(alts.preferred),
            Token::Payload(token_payload) => match token_payload {
                TokenPayload::PubkeySource => {
                    payload
                        .and_then(|p| p.get_pubkey_source())
                        .expect("expected pubkey payload")
                        .serialize_tokens(writer);
                }

                TokenPayload::Pubkey => {
                    let pubkey = payload
                        .and_then(|p| p.get_pubkey())
                        .expect("expected note_id payload");
                    writer.write_token(&hex::encode(pubkey.bytes()));
                }

                TokenPayload::NoteId => {
                    let note_id = payload
                        .and_then(|p| p.get_note_id())
                        .expect("expected note_id payload");
                    writer.write_token(&hex::encode(note_id.bytes()));
                }
            },
        }
    }

    pub const fn id(s: &'static str) -> Self {
        Token::Identifier(s)
    }

    pub const fn alts(primary: &'static str, deprecated: &'static [&'static str]) -> Self {
        Token::Alternatives(TokenAlternatives::new(primary, deprecated))
    }

    pub const fn pubkey() -> Self {
        Token::Payload(TokenPayload::Pubkey)
    }

    pub const fn pubkey_source() -> Self {
        Token::Payload(TokenPayload::PubkeySource)
    }

    pub const fn note_id() -> Self {
        Token::Payload(TokenPayload::NoteId)
    }
}

#[derive(Debug, Clone)]
pub enum ParseError<'a> {
    /// Not done parsing yet
    Incomplete,

    /// All parsing options failed
    AltAllFailed,

    /// There was some issue decoding the data
    DecodeFailed,

    /// There was some issue decoding the data
    ExpectedPayload(TokenPayload),

    HexDecodeFailed,

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

    pub fn peek_parse_token(&mut self, expected: &'static str) -> Result<&'a str, ParseError<'a>> {
        let found = self.peek_token()?;
        if found == expected {
            Ok(found)
        } else {
            Err(ParseError::UnexpectedToken(UnexpectedToken {
                expected,
                found,
            }))
        }
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

    /// Attempt to parse something, backtrack if we fail.
    pub fn try_parse<R>(
        &mut self,
        parse_fn: impl FnOnce(&mut Self) -> Result<R, ParseError<'a>>,
    ) -> Result<R, ParseError<'a>> {
        let start = self.index;
        let result = parse_fn(self);

        // If the parser closure fails, revert the index
        if result.is_err() {
            self.index = start;
            result
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

    pub fn peek_token(&self) -> Result<&'a str, ParseError<'a>> {
        self.tokens()
            .first()
            .ok_or(ParseError::DecodeFailed)
            .copied()
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
    fn parse_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Self, ParseError<'a>>;
    fn serialize_tokens(&self, writer: &mut TokenWriter);
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
            let parsed = AddColumnRoute::parse_from_tokens(&mut parser).unwrap();
            let expected = AddColumnRoute::Algo(AddAlgoRoute::LastPerPubkey);
            parsed.serialize_tokens(&mut token_writer);
            assert_eq!(expected, parsed);
            assert_eq!(token_writer.str(), data_str);
        }

        {
            let data_str = "column";
            let mut token_writer = TokenWriter::default();
            let data: &[&str] = &[data_str];
            let mut parser = TokenParser::new(data);
            let parsed = AddColumnRoute::parse_from_tokens(&mut parser).unwrap();
            let expected = AddColumnRoute::Base;
            parsed.serialize_tokens(&mut token_writer);
            assert_eq!(expected, parsed);
            assert_eq!(token_writer.str(), data_str);
        }
    }
}
