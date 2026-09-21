use std::fmt;

/// A normalized key used to look up an i18n translation
#[derive(Eq, PartialEq, Clone, Debug)]
pub struct IntlKeyBuf(String);

impl fmt::Display for IntlKeyBuf {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Use `self.number` to refer to each positional data point.
        write!(f, "{}", self.0)
    }
}

impl IntlKeyBuf {
    pub fn new(string: impl Into<String>) -> Self {
        IntlKeyBuf(string.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}
