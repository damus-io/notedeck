# Tokenator

Tokenator is a simple, efficient library for parsing and serializing string tokens in Rust. It provides a lightweight solution for working with colon-delimited (or custom-delimited) string formats.

## Features

- Parse colon-delimited (or custom-delimited) string tokens
- Serialize data structures into token strings
- Robust error handling with descriptive error types
- Support for backtracking and alternative parsing routes
- Zero-copy parsing for improved performance
- Hex decoding utilities

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
tokenator = "0.1.0"
```

## Quick Start

```rust
use tokenator::{TokenParser, TokenWriter, TokenSerializable};

// Define a type that can be serialized to/from tokens
struct User {
    name: String,
    age: u32,
}

impl TokenSerializable for User {
    fn parse_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Self, tokenator::ParseError<'a>> {
        // Expect the token "user" first
        parser.parse_token("user")?;

        // Parse name and age
        let name = parser.pull_token()?.to_string();
        let age_str = parser.pull_token()?;
        let age = age_str.parse::<u32>().map_err(|_| tokenator::ParseError::DecodeFailed)?;

        Ok(Self { name, age })
    }

    fn serialize_tokens(&self, writer: &mut TokenWriter) {
        writer.write_token("user");
        writer.write_token(&self.name);
        writer.write_token(&self.age.to_string());
    }
}

fn main() {
    // Parsing example
    let tokens = ["user", "alice", "30"];
    let mut parser = TokenParser::new(&tokens);
    let user = User::parse_from_tokens(&mut parser).unwrap();
    assert_eq!(user.name, "alice");
    assert_eq!(user.age, 30);

    // Serializing example
    let user = User {
        name: "bob".to_string(),
        age: 25,
    };
    let mut writer = TokenWriter::default();
    user.serialize_tokens(&mut writer);
    assert_eq!(writer.str(), "user:bob:25");
}
```

## License

MIT
