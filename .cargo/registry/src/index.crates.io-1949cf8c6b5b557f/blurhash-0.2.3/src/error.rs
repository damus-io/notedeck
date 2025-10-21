use std::fmt;

#[derive(Debug)]
pub enum Error {
    HashTooShort,
    LengthMismatch { expected: usize, actual: usize },
    InvalidAscii,
    InvalidBase83(u8),
    ComponentsOutOfRange,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let message = match self {
            Error::HashTooShort => "blurhash must be at least 6 characters long".to_string(),
            Error::LengthMismatch { expected, actual } => format!(
                "blurhash length mismatch: length is {} but it should be {}",
                actual, expected
            ),
            Error::InvalidBase83(byte) => format!("Invalid base83 character: {:?}", *byte as char),
            Error::InvalidAscii => "blurhash must be valid ASCII".into(),
            Error::ComponentsOutOfRange => "blurhash must have between 1 and 9 components".into(),
        };
        write!(f, "{}", message)
    }
}

impl std::error::Error for Error {}
