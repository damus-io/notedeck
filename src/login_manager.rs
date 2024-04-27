use crate::key_parsing::perform_key_retrieval;
use crate::key_parsing::LoginError;
use egui::{TextBuffer, TextEdit};
use nostr_sdk::Keys;
use poll_promise::Promise;

/// Helper storage object for retrieving the plaintext key from the user and converting it into a
/// nostr-sdk Keys object if possible.
#[derive(Default)]
pub struct LoginManager {
    login_key: String,
    promise: Option<Promise<Result<Keys, LoginError>>>,
    error: Option<LoginError>,
    key_on_error: Option<String>,
}

impl<'a> LoginManager {
    pub fn new() -> Self {
        LoginManager {
            login_key: String::new(),
            promise: None,
            error: None,
            key_on_error: None,
        }
    }

    pub fn get_login_textedit(
        &'a mut self,
        textedit_closure: fn(&'a mut dyn TextBuffer) -> TextEdit<'a>,
    ) -> TextEdit<'a> {
        textedit_closure(&mut self.login_key)
    }

    pub fn apply_login(&'a mut self) {
        self.promise = Some(perform_key_retrieval(&self.login_key));
    }

    pub fn is_awaiting_network(&self) -> bool {
        self.promise.is_some()
    }

    pub fn check_for_error(&'a mut self) -> Option<&'a LoginError> {
        if let Some(error_key) = &self.key_on_error {
            if self.login_key != *error_key {
                self.error = None;
                self.key_on_error = None;
            }
        }

        self.error.as_ref()
    }

    pub fn check_for_successful_login(&mut self) -> Option<Keys> {
        if let Some(promise) = &mut self.promise {
            if promise.ready().is_some() {
                if let Some(promise) = self.promise.take() {
                    match promise.block_and_take() {
                        Ok(key) => {
                            return Some(key);
                        }
                        Err(e) => {
                            self.error = Some(e);
                            self.key_on_error = Some(self.login_key.clone());
                        }
                    };
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::PublicKey;
    use std::time::{Duration, Instant};

    #[test]
    fn test_retrieve_key() {
        let mut manager = LoginManager::new();
        let manager_ref = &mut manager;
        let expected_str = "3efdaebb1d8923ebd99c9e7ace3b4194ab45512e2be79c1b7d68d9243e0d2681";
        let expected_key = Keys::from_public_key(PublicKey::from_hex(expected_str).unwrap());

        let start_time = Instant::now();

        while start_time.elapsed() < Duration::from_millis(50u64) {
            let cur_time = start_time.elapsed();

            if cur_time < Duration::from_millis(10u64) {
                let key = "test";
                manager_ref.login_key = String::from(key);
                manager_ref.promise = Some(perform_key_retrieval(key));
            } else if cur_time < Duration::from_millis(30u64) {
                let key = "test2";
                manager_ref.login_key = String::from(key);
                manager_ref.promise = Some(perform_key_retrieval(key));
            } else {
                manager_ref.login_key = String::from(expected_str);
                manager_ref.promise = Some(perform_key_retrieval(expected_str));
            }

            if let Some(key) = manager_ref.check_for_successful_login() {
                assert_eq!(expected_key, key);
                return;
            }
        }

        panic!();
    }
}
