use enostr;

#[derive(Eq, PartialEq)]
pub enum Error {
    Nostr(enostr::Error),
}

impl From<enostr::Error> for Error {
    fn from(err: enostr::Error) -> Self {
        Error::Nostr(err)
    }
}
