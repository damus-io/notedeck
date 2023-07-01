use crate::parser;

#[derive(Eq, PartialEq, Debug)]
pub enum Error {
    Nostr(enostr::Error),
    Parse(parser::Error),
    Generic(String),
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Generic(s)
    }
}

impl From<parser::Error> for Error {
    fn from(s: parser::Error) -> Self {
        Error::Parse(s)
    }
}

impl From<enostr::Error> for Error {
    fn from(err: enostr::Error) -> Self {
        Error::Nostr(err)
    }
}
