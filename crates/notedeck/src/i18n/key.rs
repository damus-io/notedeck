use std::fmt;

/// An owned key used to lookup i18n translations. Mostly used for errors
#[derive(Eq, PartialEq, Clone, Debug)]
pub struct IntlKeyBuf(String);

/// A key used to lookup i18n translations
#[derive(Eq, PartialEq, Clone, Copy, Debug)]
pub struct IntlKey<'a>(&'a str);

impl fmt::Display for IntlKey<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Use `self.number` to refer to each positional data point.
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for IntlKeyBuf {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Use `self.number` to refer to each positional data point.
        write!(f, "{}", &self.0)
    }
}

impl IntlKeyBuf {
    pub fn new(string: impl Into<String>) -> Self {
        IntlKeyBuf(string.into())
    }

    pub fn borrow<'a>(&'a self) -> IntlKey<'a> {
        IntlKey::new(&self.0)
    }
}

impl<'a> IntlKey<'a> {
    pub fn new(string: &'a str) -> IntlKey<'a> {
        IntlKey(string)
    }

    pub fn to_owned(&self) -> IntlKeyBuf {
        IntlKeyBuf::new(self.0)
    }

    pub fn as_str(&self) -> &'a str {
        self.0
    }
}
