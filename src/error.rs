use shatter::parser;

#[derive(Debug)]
pub enum Error {
    Nostr(enostr::Error),
    Shatter(parser::Error),
    Image(image::error::ImageError),
    Generic(String),
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::Generic(s)
    }
}

impl From<parser::Error> for Error {
    fn from(s: parser::Error) -> Self {
        Error::Shatter(s)
    }
}

impl From<image::error::ImageError> for Error {
    fn from(err: image::error::ImageError) -> Self {
        Error::Image(err)
    }
}

impl From<enostr::Error> for Error {
    fn from(err: enostr::Error) -> Self {
        Error::Nostr(err)
    }
}
