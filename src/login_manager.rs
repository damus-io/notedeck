use crate::key_parsing::LoginError;
use nostr_sdk::Keys;
use poll_promise::Promise;

/// Helper storage object for retrieving the plaintext key from the user and converting it into a
/// nostr-sdk Keys object if possible.
pub struct LoginManager {
    pub login_key: String,
    pub promise: Option<Promise<Result<Keys, LoginError>>>,
    pub error: Option<LoginError>,
    pub key_on_error: Option<String>
}

impl LoginManager {
    pub fn new() -> Self {
        LoginManager {
            login_key: String::new(),
            promise: None,
            error: None,
            key_on_error: None
        }
    }
}
