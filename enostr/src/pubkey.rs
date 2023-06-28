use serde_derive::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Eq, PartialEq, Clone, Hash)]
pub struct Pubkey(String);

impl AsRef<str> for Pubkey {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<String> for Pubkey {
    fn from(s: String) -> Self {
        Pubkey(s)
    }
}

impl From<&str> for Pubkey {
    fn from(s: &str) -> Self {
        Pubkey(s.to_owned())
    }
}

impl From<Pubkey> for String {
    fn from(pk: Pubkey) -> Self {
        pk.0
    }
}
