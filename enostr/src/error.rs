use serde_json;

#[derive(Debug)]
pub enum Error {
    MessageEmpty,
    MessageDecodeFailed,
    InvalidSignature,
    Json(serde_json::Error),
    Hex(hex::FromHexError),
    Generic(String),
}

impl std::cmp::PartialEq for Error {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Error::MessageEmpty, Error::MessageEmpty) => true,
            (Error::MessageDecodeFailed, Error::MessageDecodeFailed) => true,
            (Error::InvalidSignature, Error::InvalidSignature) => true,
            // This is slightly wrong but whatevs
            (Error::Json(..), Error::Json(..)) => true,
            (Error::Generic(left), Error::Generic(right)) => left == right,
            _ => false,
        }
    }
}

impl std::cmp::Eq for Error {}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Generic(s)
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Error::Json(e)
    }
}

impl From<hex::FromHexError> for Error {
    fn from(e: hex::FromHexError) -> Self {
        Error::Hex(e)
    }
}
