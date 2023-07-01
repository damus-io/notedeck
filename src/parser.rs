use log::{debug, info};

#[derive(Debug, PartialEq, Eq)]
pub struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Bound {
    Start,
    End,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    BadUtf8Encoding,
    OutOfBounds(Bound),
}

type Result<T> = std::result::Result<T, Error>;

pub fn is_oob<T>(r: Result<T>) -> bool {
    match r {
        Err(Error::OutOfBounds(_)) => true,
        Err(_) => false,
        Ok(_) => false,
    }
}

impl<'a> Parser<'a> {
    pub fn from_bytes(data: &'a [u8]) -> Parser<'a> {
        Parser { data: data, pos: 0 }
    }

    pub fn from_str(string: &'a str) -> Parser<'a> {
        Parser {
            data: string.as_bytes(),
            pos: 0,
        }
    }

    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos;
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn data(&self) -> &[u8] {
        self.data
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn pull_byte(&mut self) -> Result<u8> {
        if self.pos + 1 > self.len() {
            return Err(Error::OutOfBounds(Bound::End));
        }

        let c = self.data[self.pos];
        self.pos += 1;
        return Ok(c);
    }

    pub fn skip<F: Fn(char) -> bool>(&mut self, should_skip: F) -> Result<()> {
        let len = self.len();
        while self.pos < len {
            let prev = self.pos();
            if should_skip(self.pull_char()?) {
                continue;
            } else {
                self.set_pos(prev);
                return Ok(());
            }
        }

        return Err(Error::OutOfBounds(Bound::End));
    }

    pub fn skip_whitespace(&mut self) -> Result<()> {
        self.skip(|c| c.is_ascii_whitespace())
    }

    pub fn peek_char(&mut self) -> Result<char> {
        let peek = true;
        self.pull_or_peek_char(peek)
    }

    pub fn pull_char(&mut self) -> Result<char> {
        let peek = false;
        self.pull_or_peek_char(peek)
    }

    pub fn seek_prev_byte(&mut self) -> Result<()> {
        if self.pos == 0 {
            return Err(Error::OutOfBounds(Bound::Start));
        }
        self.pos -= 1;

        Ok(())
    }

    fn peek_prev_char(&self) -> Result<char> {
        let mut i = 1;
        let mut codepoint = 0u32;
        let mut bs: [u32; 4] = [0; 4];

        if self.pos == 0 {
            return Err(Error::OutOfBounds(Bound::Start));
        }

        while i <= 4 && ((self.pos as i32) - (i as i32) >= 0) {
            let byte = self.data[self.pos - i] as u32;
            let masked = byte & 0b11000000;
            if masked == 0b10000000 {
                // continuation byte
                bs[i - 1] = byte & 0b00111111;
                i += 1;
            } else if masked == 0b11000000 {
                // start byte
                match i {
                    4 => {
                        codepoint = ((bs[3] & 0x07) << 18)
                            | ((bs[2] & 0x3F) << 12)
                            | ((bs[1] & 0x3F) << 6)
                            | (bs[0] & 0x3F)
                    }
                    3 => {
                        codepoint = ((bs[2] & 0x0F) << 12) | ((bs[1] & 0x3F) << 6) | (bs[0] & 0x3F)
                    }
                    2 => codepoint = ((bs[1] & 0x0F) << 6) | (bs[0] & 0x3F),
                    _ => return Err(Error::BadUtf8Encoding),
                }
                return parser_codepoint_char(codepoint);
            } else {
                return parser_codepoint_char(byte);
            }
        }

        // If we reached here, we reached the start of the string without finding a non-continuation byte.
        Err(Error::BadUtf8Encoding)
    }

    pub fn seek_prev_char(&mut self) -> Result<()> {
        self.seek_prev_byte()?;
        while self.pos > 0 && (self.data[self.pos] & 0b11000000) == 0b10000000 {
            self.pos -= 1;
        }

        Ok(())
    }

    fn pull_or_peek_char(&mut self, peek: bool) -> Result<char> {
        let mut codepoint: u32 = 0;

        let start = self.pos;
        let b0 = self.pull_byte()? as u32;

        if b0 & 0x80 != 0 {
            if (b0 & 0b11100000) == 0b11000000 {
                // Two-byte sequence
                let b1 = self.pull_byte()? as u32;
                codepoint = ((b0 & 0b00011111) << 6) | (b1 & 0b00111111);
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
            None => Err(Error::BadUtf8Encoding),
        }
    }

    pub fn parse_until_char(&mut self, needle: char) -> Result<()> {
        self.parse_until(|c| c == needle)
    }

    pub fn parse_until<F: Fn(char) -> bool>(&mut self, matches: F) -> Result<()> {
        let len = self.len();
        while self.pos < len {
            let prev = self.pos;
            if matches(self.pull_char()?) {
                self.pos = prev;
                return Ok(());
            }
        }

        Err(Error::OutOfBounds(Bound::End))
    }
}

fn parser_codepoint_char(codepoint: u32) -> Result<char> {
    match std::char::from_u32(codepoint) {
        Some(c) => Ok(c),
        None => Err(Error::BadUtf8Encoding),
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
        let mut parser = Parser::from_str(s);
        let mut res = parser.parse_until_char('#');
        assert_eq!(res, Ok(()));
        assert_eq!(parser.pos, 1);
        res = parser.parse_until_char('t');
        assert_eq!(res, Ok(()));
        assert_eq!(parser.pos, 6);
        Ok(())
    }

    #[test]
    fn test_peek_prev_char() {
        let s = ".👽.";
        let mut parser = Parser::from_str(s);
        let r1 = parser.parse_until_char('👽');
        assert_eq!(r1, Ok(()));
        let r2 = parser.pull_char();
        assert_eq!(r2, Ok('👽'));
        let r3 = parser.peek_prev_char();
        assert_eq!(r3, Ok('👽'));
        assert_eq!(parser.pos(), 5);
    }

    #[test]
    fn test_utf8_parsing() -> Result<()> {
        let s = "hey there #👽.";
        let mut parser = Parser::from_str(s);
        let _ = parser.parse_until_char('👽');
        assert_eq!(parser.peek_char(), Ok('👽'));
        assert_eq!(parser.pos, 11);
        let res = parser.parse_until(|c| c.is_ascii_whitespace() || c.is_ascii_punctuation());
        assert_eq!(res, Ok(()));
        assert_eq!(parser.peek_char(), Ok('.'));
        Ok(())
    }
}
