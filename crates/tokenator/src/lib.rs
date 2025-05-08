#[derive(Debug, Clone)]
pub struct UnexpectedToken<'fnd, 'exp> {
    pub expected: &'exp str,
    pub found: &'fnd str,
}

#[derive(Debug, Clone)]
pub struct UnexpectedTokenOwned {
    pub expected: String,
    pub found: String,
}

#[derive(Debug, Clone)]
pub enum ParseError<'a> {
    /// Not done parsing yet
    Incomplete,

    /// All parsing options failed
    AltAllFailed,

    /// There was some issue decoding the data
    DecodeFailed,

    HexDecodeFailed,

    /// We encountered an unexpected token
    UnexpectedToken(UnexpectedToken<'a, 'static>),

    /// No more tokens
    EOF,
}

#[derive(Debug, Clone)]
pub enum ParseErrorOwned {
    Incomplete,
    AltAllFailed,
    DecodeFailed,
    HexDecodeFailed,
    UnexpectedToken(UnexpectedTokenOwned),
    EOF,
}

impl From<ParseError<'_>> for ParseErrorOwned {
    fn from(value: ParseError) -> Self {
        match value {
            ParseError::Incomplete => Self::Incomplete,
            ParseError::AltAllFailed => Self::AltAllFailed,
            ParseError::DecodeFailed => Self::DecodeFailed,
            ParseError::HexDecodeFailed => Self::HexDecodeFailed,
            ParseError::UnexpectedToken(unexpected_token) => {
                Self::UnexpectedToken(UnexpectedTokenOwned {
                    expected: unexpected_token.expected.to_owned(),
                    found: unexpected_token.found.to_owned(),
                })
            }
            ParseError::EOF => Self::EOF,
        }
    }
}

pub struct TokenWriter {
    pub buf: TokenBuffer,
}

pub enum TokenBuffer {
    ToString(StrTokenWriter),
    ToNote(NoteTokenWriter),
}

pub struct StrTokenWriter {
    pub delim: &'static str,
    pub tokens_written: usize,
    pub buf: Vec<u8>,
}

impl StrTokenWriter {
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

const DELIM: &str = ":";

pub struct NoteTokenWriter {
    pub delim: &'static str,
    pub buf: Vec<String>,
}

impl Default for NoteTokenWriter {
    fn default() -> Self {
        Self {
            delim: DELIM,
            buf: Default::default(),
        }
    }
}

impl Default for StrTokenWriter {
    fn default() -> Self {
        Self::new(DELIM)
    }
}

impl Default for TokenWriter {
    fn default() -> Self {
        Self::new(TokenBuffer::ToString(StrTokenWriter::default()))
    }
}

impl TokenWriter {
    pub fn new(buf: TokenBuffer) -> Self {
        Self { buf }
    }

    pub fn write_token(&mut self, token: &str) {
        match &mut self.buf {
            TokenBuffer::ToString(string_token_writer) => string_token_writer.write_token(token),
            TokenBuffer::ToNote(note_token_writer) => note_token_writer.write_token(token),
        }
    }

    pub fn str(&self) -> String {
        match &self.buf {
            TokenBuffer::ToString(string_token_writer) => string_token_writer.str().to_owned(),
            TokenBuffer::ToNote(note_token_writer) => note_token_writer.to_string(),
        }
    }
}

impl NoteTokenWriter {
    pub fn write_token(&mut self, token: &str) {
        self.buf.push(token.to_owned());
    }

    pub fn to_builder<'a>(
        &self,
        mut builder: nostrdb::NoteBuilder<'a>,
    ) -> nostrdb::NoteBuilder<'a> {
        if !self.buf.is_empty() {
            builder = builder.start_tag().tag_str("col");

            for token in &self.buf {
                builder = builder.tag_str(token);
            }
        }

        builder
    }
}

impl std::fmt::Display for NoteTokenWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.buf.join(self.delim))
    }
}

#[derive(Clone)]
pub struct TokenParser<'a> {
    tokens: &'a [&'a str],
    index: usize,
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

    /// Parse a list of alternative tokens, returning success if any match.
    pub fn parse_any_token(
        &mut self,
        expected: &[&'static str],
    ) -> Result<&'a str, ParseError<'a>> {
        for token in expected {
            let result = self.try_parse(|p| p.parse_token(token));
            if result.is_ok() {
                return result;
            }
        }

        Err(ParseError::AltAllFailed)
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

    /// Ensure that we have parsed all tokens. If not the parser backtracks
    /// and the parse does not succeed, returning [`ParseError::Incomplete`].
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

/// Parse a 32 byte hex string
pub fn parse_hex_id<'a>(parser: &mut TokenParser<'a>) -> Result<[u8; 32], ParseError<'a>> {
    use hex;

    let hexid = parser.pull_token()?;
    hex::decode(hexid)
        .map_err(|_| ParseError::HexDecodeFailed)?
        .as_slice()
        .try_into()
        .map_err(|_| ParseError::HexDecodeFailed)
}
