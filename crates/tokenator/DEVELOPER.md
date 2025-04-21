# Tokenator Developer Documentation

This document provides detailed information for developers who want to use the Tokenator library in their projects or contribute to its development.

## Core Concepts

Tokenator works with two primary concepts:

1. **Token Parsing**: Converting a sequence of string tokens into structured data
2. **Token Serialization**: Converting structured data into a sequence of string tokens

The library is designed to be simple, efficient, and flexible for working with delimited string formats.

## API Reference

### TokenParser

`TokenParser` is responsible for parsing tokens from a slice of string references.

```rust
pub struct TokenParser<'a> {
    tokens: &'a [&'a str],
    index: usize,
}
```

Key methods:

- `new(tokens: &'a [&'a str]) -> Self`: Creates a new parser from a slice of string tokens
- `pull_token() -> Result<&'a str, ParseError<'a>>`: Gets the next token and advances the index
- `peek_token() -> Result<&'a str, ParseError<'a>>`: Looks at the next token without advancing the index
- `parse_token(expected: &'static str) -> Result<&'a str, ParseError<'a>>`: Checks if the next token matches the expected value
- `alt<R>(parser: &mut TokenParser<'a>, routes: &[fn(&mut TokenParser<'a>) -> Result<R, ParseError<'a>>]) -> Result<R, ParseError<'a>>`: Tries each parser in `routes` until one succeeds
- `parse_all<R>(&mut self, parse_fn: impl FnOnce(&mut Self) -> Result<R, ParseError<'a>>) -> Result<R, ParseError<'a>>`: Ensures all tokens are consumed after parsing
- `try_parse<R>(&mut self, parse_fn: impl FnOnce(&mut Self) -> Result<R, ParseError<'a>>) -> Result<R, ParseError<'a>>`: Attempts to parse and backtracks on failure
- `is_eof() -> bool`: Checks if there are any tokens left to parse

### TokenWriter

`TokenWriter` is responsible for serializing tokens into a string with the specified delimiter.

```rust
pub struct TokenWriter {
    delim: &'static str,
    tokens_written: usize,
    buf: Vec<u8>,
}
```

Key methods:

- `new(delim: &'static str) -> Self`: Creates a new writer with the specified delimiter
- `default() -> Self`: Creates a new writer with ":" as the delimiter
- `write_token(token: &str)`: Appends a token to the buffer
- `str() -> &str`: Gets the current buffer as a string
- `buffer() -> &[u8]`: Gets the current buffer as a byte slice

### TokenSerializable

`TokenSerializable` is a trait that types can implement to be serialized to and parsed from tokens.

```rust
pub trait TokenSerializable: Sized {
    fn parse_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Self, ParseError<'a>>;
    fn serialize_tokens(&self, writer: &mut TokenWriter);
}
```

### Error Handling

The library provides detailed error types:

- `ParseError<'a>`: Represents errors that can occur during parsing
  - `Incomplete`: Not done parsing yet
  - `AltAllFailed`: All parsing options failed
  - `DecodeFailed`: General decoding failure
  - `HexDecodeFailed`: Hex decoding failure
  - `UnexpectedToken`: Encountered an unexpected token
  - `EOF`: No more tokens

## Advanced Usage

### Backtracking and Alternative Parsing

One of the powerful features of Tokenator is its support for backtracking and alternative parsing paths:

```rust
// Try multiple parsing strategies
let result = TokenParser::alt(&mut parser, &[
    |p| parse_strategy_a(p),
    |p| parse_strategy_b(p),
    |p| parse_strategy_c(p),
]);

// Attempt to parse but backtrack on failure
let result = parser.try_parse(|p| {
    let token = p.parse_token("specific_token")?;
    // More parsing...
    Ok(result)
});
```

### Parsing Hex Data

The library includes utilities for parsing hexadecimal data:

```rust
use tokenator::parse_hex_id;

// Parse a 32-byte hex string from the next token
let hash: [u8; 32] = parse_hex_id(&mut parser)?;
```

### Custom Delimiters

You can use custom delimiters when serializing tokens:

```rust
// Create a writer with a custom delimiter
let mut writer = TokenWriter::new("|");
writer.write_token("user");
writer.write_token("alice");
// Result: "user|alice"
```

## Best Practices

1. **Implement TokenSerializable for your types**: This ensures consistency between parsing and serialization logic.

2. **Use try_parse for speculative parsing**: When trying different parsing strategies, wrap them in `try_parse` to ensure proper backtracking.

3. **Handle all error cases**: The detailed error types provided by Tokenator help identify and handle specific parsing issues.

4. **Consider memory efficiency**: The parser works with string references to avoid unnecessary copying.

5. **Validate input**: Always validate input tokens before attempting to parse them into your data structures.

## Integration Examples

### Custom Protocol Parser

```rust
use tokenator::{TokenParser, TokenWriter, TokenSerializable, ParseError};

enum Command {
    Get { key: String },
    Set { key: String, value: String },
    Delete { key: String },
}

impl TokenSerializable for Command {
    fn parse_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Self, ParseError<'a>> {
        let cmd = parser.pull_token()?;
        
        match cmd {
            "GET" => {
                let key = parser.pull_token()?.to_string();
                Ok(Command::Get { key })
            },
            "SET" => {
                let key = parser.pull_token()?.to_string();
                let value = parser.pull_token()?.to_string();
                Ok(Command::Set { key, value })
            },
            "DEL" => {
                let key = parser.pull_token()?.to_string();
                Ok(Command::Delete { key })
            },
            _ => Err(ParseError::UnexpectedToken(tokenator::UnexpectedToken {
                expected: "GET, SET, or DEL",
                found: cmd,
            })),
        }
    }

    fn serialize_tokens(&self, writer: &mut TokenWriter) {
        match self {
            Command::Get { key } => {
                writer.write_token("GET");
                writer.write_token(key);
            },
            Command::Set { key, value } => {
                writer.write_token("SET");
                writer.write_token(key);
                writer.write_token(value);
            },
            Command::Delete { key } => {
                writer.write_token("DEL");
                writer.write_token(key);
            },
        }
    }
}
```

## Contributing

Contributions to Tokenator are welcome! Here are some areas that could be improved:

- Additional parsing utilities
- Performance optimizations
- More comprehensive test coverage
- Example implementations for common use cases
- Documentation improvements

When submitting a pull request, please ensure:

1. All tests pass
2. New functionality includes appropriate tests
3. Documentation is updated to reflect changes
4. Code follows the existing style conventions
