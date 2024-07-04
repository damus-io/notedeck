use crate::key_parsing::perform_key_retrieval;
use crate::key_parsing::LoginError;
use egui::{TextBuffer, TextEdit};
use enostr::Keypair;
use poll_promise::Promise;

/// The UI view interface to log in to a nostr account.
#[derive(Default)]
pub struct LoginManager {
    login_key: String,
    promise_query: Option<(String, Promise<Result<Keypair, LoginError>>)>,
    error: Option<LoginError>,
    key_on_error: Option<String>,
}

impl<'a> LoginManager {
    pub fn new() -> Self {
        LoginManager {
            login_key: String::new(),
            promise_query: None,
            error: None,
            key_on_error: None,
        }
    }

    /// Get the textedit for the login UI without exposing the key variable
    pub fn get_login_textedit(
        &'a mut self,
        textedit_closure: fn(&'a mut dyn TextBuffer) -> TextEdit<'a>,
    ) -> TextEdit<'a> {
        textedit_closure(&mut self.login_key)
    }

    /// User pressed the 'login' button
    pub fn apply_login(&'a mut self) {
        let new_promise = match &self.promise_query {
            Some((query, _)) => {
                if query != &self.login_key {
                    Some(perform_key_retrieval(&self.login_key))
                } else {
                    None
                }
            }
            None => Some(perform_key_retrieval(&self.login_key)),
        };

        if let Some(new_promise) = new_promise {
            self.promise_query = Some((self.login_key.clone(), new_promise));
        }
    }

    /// Whether to indicate to the user that there is a network operation occuring
    pub fn is_awaiting_network(&self) -> bool {
        self.promise_query.is_some()
    }

    /// Whether to indicate to the user that a login error occured
    pub fn check_for_error(&'a mut self) -> Option<&'a LoginError> {
        if let Some(error_key) = &self.key_on_error {
            if self.login_key != *error_key {
                self.error = None;
                self.key_on_error = None;
            }
        }

        self.error.as_ref()
    }

    /// Whether to indicate to the user that a successful login occured
    pub fn check_for_successful_login(&mut self) -> Option<Keypair> {
        if let Some((_, promise)) = &mut self.promise_query {
            if promise.ready().is_some() {
                if let Some((_, promise)) = self.promise_query.take() {
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

    pub fn clear(&mut self) {
        *self = Default::default();
    }
}

#[cfg(test)]
mod tests {
    use enostr::Pubkey;

    use super::*;
    use std::time::{Duration, Instant};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_retrieve_key() {
        let mut manager = LoginManager::new();
        let expected_str = "3efdaebb1d8923ebd99c9e7ace3b4194ab45512e2be79c1b7d68d9243e0d2681";
        let expected_key = Keypair::only_pubkey(Pubkey::from_hex(expected_str).unwrap());

        let start_time = Instant::now();

        while start_time.elapsed() < Duration::from_millis(50u64) {
            let cur_time = start_time.elapsed();

            if cur_time < Duration::from_millis(10u64) {
                let _ = manager.get_login_textedit(|text| {
                    text.clear();
                    text.insert_text("test", 0);
                    egui::TextEdit::singleline(text)
                });
                manager.apply_login();
            } else if cur_time < Duration::from_millis(30u64) {
                let _ = manager.get_login_textedit(|text| {
                    text.clear();
                    text.insert_text("test2", 0);
                    egui::TextEdit::singleline(text)
                });
                manager.apply_login();
            } else {
                let _ = manager.get_login_textedit(|text| {
                    text.clear();
                    text.insert_text(
                        "3efdaebb1d8923ebd99c9e7ace3b4194ab45512e2be79c1b7d68d9243e0d2681",
                        0,
                    );
                    egui::TextEdit::singleline(text)
                });
                manager.apply_login();
            }

            if let Some(key) = manager.check_for_successful_login() {
                assert_eq!(expected_key, key);
                return;
            }
        }

        panic!("Test failed to get expected key.");
    }
}
