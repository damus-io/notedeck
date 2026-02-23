use std::fmt;

/// A token from the s-expression tokenizer.
/// String references are zero-copy slices into the input.
#[derive(Debug, Clone, PartialEq)]
pub enum Token<'a> {
    Open,
    Close,
    Symbol(&'a str),
    Str(&'a str),
    Number(&'a str),
}

#[derive(Debug)]
pub struct TokenError {
    pub msg: String,
    pub pos: usize,
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "token error at position {}: {}", self.pos, self.msg)
    }
}

impl std::error::Error for TokenError {}

fn is_symbol_start(c: u8) -> bool {
    c.is_ascii_lowercase()
}

fn is_symbol_char(c: u8) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'_'
}

fn scan_symbol(input: &[u8], start: usize) -> Result<usize, TokenError> {
    if start >= input.len() || !is_symbol_start(input[start]) {
        return Err(TokenError {
            msg: "symbol must start with a-z".into(),
            pos: start,
        });
    }
    let mut end = start + 1;
    while end < input.len() {
        let c = input[end];
        if c.is_ascii_whitespace() || c == b')' || c == b'(' {
            break;
        }
        if !is_symbol_char(c) {
            return Err(TokenError {
                msg: format!("invalid symbol character '{}'", c as char),
                pos: end,
            });
        }
        end += 1;
    }
    Ok(end)
}

fn scan_number(input: &[u8], start: usize) -> Result<usize, TokenError> {
    if start >= input.len() {
        return Err(TokenError {
            msg: "unexpected end of input in number".into(),
            pos: start,
        });
    }
    let first = input[start];
    if !first.is_ascii_digit() && first != b'-' {
        return Err(TokenError {
            msg: "number must start with 0-9 or -".into(),
            pos: start,
        });
    }
    let mut end = start + 1;
    while end < input.len() {
        let c = input[end];
        if c.is_ascii_whitespace() || c == b')' || c == b'(' {
            break;
        }
        if !c.is_ascii_digit() && c != b'.' {
            return Err(TokenError {
                msg: format!("invalid number character '{}'", c as char),
                pos: end,
            });
        }
        end += 1;
    }
    Ok(end)
}

fn scan_string(input: &[u8], start: usize) -> Result<(usize, usize), TokenError> {
    // start should point at the opening quote
    if start >= input.len() || input[start] != b'"' {
        return Err(TokenError {
            msg: "string must start with '\"'".into(),
            pos: start,
        });
    }
    let content_start = start + 1;
    let mut i = content_start;
    while i < input.len() {
        if input[i] == b'\\' {
            i += 2; // skip escaped char
            continue;
        }
        if input[i] == b'"' {
            return Ok((content_start, i)); // i points at closing quote
        }
        i += 1;
    }
    Err(TokenError {
        msg: "unterminated string".into(),
        pos: start,
    })
}

/// Tokenize an s-expression input string into a sequence of tokens.
/// Token string/symbol/number values are zero-copy references into the input.
pub fn tokenize(input: &str) -> Result<Vec<Token<'_>>, TokenError> {
    let mut tokens = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        let c = bytes[i];

        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }

        match c {
            b'(' => {
                tokens.push(Token::Open);
                i += 1;
            }
            b')' => {
                tokens.push(Token::Close);
                i += 1;
            }
            b'"' => {
                let (content_start, content_end) = scan_string(bytes, i)?;
                tokens.push(Token::Str(&input[content_start..content_end]));
                i = content_end + 1; // skip closing quote
            }
            b'a'..=b'z' => {
                let end = scan_symbol(bytes, i)?;
                tokens.push(Token::Symbol(&input[i..end]));
                i = end;
            }
            b'0'..=b'9' | b'-' => {
                let end = scan_number(bytes, i)?;
                tokens.push(Token::Number(&input[i..end]));
                i = end;
            }
            _ => {
                return Err(TokenError {
                    msg: format!("unexpected character '{}'", c as char),
                    pos: i,
                });
            }
        }
    }

    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_simple() {
        let tokens = tokenize("(room (name \"hello\"))").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Open,
                Token::Symbol("room"),
                Token::Open,
                Token::Symbol("name"),
                Token::Str("hello"),
                Token::Close,
                Token::Close,
            ]
        );
    }

    #[test]
    fn test_tokenize_number() {
        let tokens = tokenize("(width 10)").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Open,
                Token::Symbol("width"),
                Token::Number("10"),
                Token::Close,
            ]
        );
    }

    #[test]
    fn test_tokenize_symbol_with_dash() {
        let tokens = tokenize("(id welcome-desk)").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Open,
                Token::Symbol("id"),
                Token::Symbol("welcome-desk"),
                Token::Close,
            ]
        );
    }

    #[test]
    fn test_tokenize_negative_number() {
        let tokens = tokenize("(height -5)").unwrap();
        assert_eq!(
            tokens,
            vec![
                Token::Open,
                Token::Symbol("height"),
                Token::Number("-5"),
                Token::Close,
            ]
        );
    }
}
