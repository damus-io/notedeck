use log::info;

#[derive(Debug, PartialEq)]
struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
}

#[derive(Debug, PartialEq)]
enum ParseError {
    NotFound,
    BadUtf8Encoding,
    EOF,
}

type Result<T> = std::result::Result<T, ParseError>;

impl<'a> Parser<'a> {
    fn new(data: &'a [u8]) -> Parser {
        Parser { data: data, pos: 0 }
    }

    fn pull_byte(&mut self) -> Result<u8> {
        if self.pos + 1 > self.data.len() {
            return Err(ParseError::EOF);
        }

        let c = self.data[self.pos];
        self.pos += 1;
        return Ok(c);
    }

    pub fn peek_char(&mut self) -> Result<char> {
        let peek = true;
        self.pull_or_peek_char(peek)
    }

    pub fn pull_char(&mut self) -> Result<char> {
        let peek = false;
        self.pull_or_peek_char(peek)
    }

    fn pull_or_peek_char(&mut self, peek: bool) -> Result<char> {
        let mut codepoint: u32 = 0;

        let start = self.pos;
        let b0 = self.pull_byte()? as u32;

        if b0 & 0x80 != 0 {
            if (b0 & 0xE0) == 0xC0 {
                // Two-byte sequence
                let b1 = self.pull_byte()? as u32;
                codepoint = ((b0 & 0x1F) << 6) | (b1 & 0x3F);
            } else if (b0 & 0xF0) == 0xE0 {
                // Three-byte sequence
                let b1 = self.pull_byte()? as u32;
                let b2 = self.pull_byte()? as u32;
                codepoint = ((b0 & 0x0F) << 12) | ((b1 & 0x3F) << 6) | (b2 & 0x3F);
            } else if (b0 & 0xF8) == 0xF0 {
                // Four-byte sequence
                let b1 = self.pull_byte()? as u32;
                let b2 = self.pull_byte()? as u32;
                let b3 = self.pull_byte()? as u32;
                codepoint =
                    ((b0 & 0x07) << 18) | ((b1 & 0x3F) << 12) | ((b2 & 0x3F) << 6) | (b3 & 0x3F);
            }
        } else {
            // Single-byte ASCII character
            codepoint = b0;
        }

        if peek {
            self.pos = start;
        }

        match std::char::from_u32(codepoint) {
            Some(c) => Ok(c),
            None => Err(ParseError::BadUtf8Encoding),
        }
    }

    fn current(&mut self) -> Result<char> {
        let last_pos = self.pos;
        let c = self.pull_char();
        if c.is_ok() {
            self.pos = last_pos;
        }
        return c;
    }

    fn parse_until_char(&mut self, needle: char) -> Result<()> {
        self.parse_until(|c| c == needle)?;
        Ok(())
    }

    fn parse_until<F: Fn(char) -> bool>(&mut self, matches: F) -> Result<()> {
        let len = self.data.len();
        while self.pos < len {
            let prev_pos = self.pos;
            if matches(self.pull_char()?) {
                self.pos = prev_pos;
                return Ok(());
            }
        }

        Err(ParseError::NotFound)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_parser() -> Result<()> {
        //             v alien  v
        // 00000000: 20f0 9f91 bd23 6861 7368 7461 670a       _....#hashtag.
        let s = " #hashtag ";
        let mut parser = Parser::new(s.as_bytes());
        let mut res = parser.parse_until_char('#');
        assert_eq!(res, Ok(()));
        assert_eq!(parser.pos, 1);
        res = parser.parse_until_char('t');
        assert_eq!(res, Ok(()));
        assert_eq!(parser.pos, 6);
        Ok(())
    }

    #[test]
    fn test_utf8_parsing() -> Result<()> {
        let s = "hey there #👽.";
        let mut parser = Parser::new(s.as_bytes());
        let _ = parser.parse_until_char('👽');
        assert_eq!(parser.current(), Ok('👽'));
        assert_eq!(parser.pos, 11);
        let res = parser.parse_until(|c| c.is_ascii_whitespace() || c.is_ascii_punctuation());
        assert_eq!(res, Ok(()));
        assert_eq!(parser.current(), Ok('.'));
        Ok(())
    }
}
