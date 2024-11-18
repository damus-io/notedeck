use crate::key_parsing::perform_key_retrieval;
use crate::key_parsing::AcquireKeyError;
use egui::{TextBuffer, TextEdit};
use enostr::Keypair;
use poll_promise::Promise;

/// The state data for acquiring a nostr key
#[derive(Default)]
pub struct AcquireKeyState {
    desired_key: String,
    promise_query: Option<(String, Promise<Result<Keypair, AcquireKeyError>>)>,
    error: Option<AcquireKeyError>,
    key_on_error: Option<String>,
    should_create_new: bool,
}

impl<'a> AcquireKeyState {
    pub fn new() -> Self {
        AcquireKeyState::default()
    }

    /// Get the textedit for the UI without exposing the key variable
    pub fn get_acquire_textedit(
        &'a mut self,
        textedit_closure: fn(&'a mut dyn TextBuffer) -> TextEdit<'a>,
    ) -> TextEdit<'a> {
        textedit_closure(&mut self.desired_key)
    }

    /// User pressed the 'acquire' button
    pub fn apply_acquire(&'a mut self) {
        let new_promise = match &self.promise_query {
            Some((query, _)) => {
                if query != &self.desired_key {
                    Some(perform_key_retrieval(&self.desired_key))
                } else {
                    None
                }
            }
            None => Some(perform_key_retrieval(&self.desired_key)),
        };

        if let Some(new_promise) = new_promise {
            self.promise_query = Some((self.desired_key.clone(), new_promise));
        }
    }

    /// Whether to indicate to the user that there is a network operation occuring
    pub fn is_awaiting_network(&self) -> bool {
        self.promise_query.is_some()
    }

    /// Whether to indicate to the user that a login error occured
    pub fn check_for_error(&'a mut self) -> Option<&'a AcquireKeyError> {
        if let Some(error_key) = &self.key_on_error {
            if self.desired_key != *error_key {
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
                            self.key_on_error = Some(self.desired_key.clone());
                        }
                    };
                }
            }
        }
        None
    }

    pub fn should_create_new(&mut self) {
        self.should_create_new = true;
    }

    pub fn check_for_create_new(&self) -> bool {
        self.should_create_new
    }
}

#[cfg(test)]
mod tests {
    use enostr::Pubkey;

    use super::*;
    use std::time::{Duration, Instant};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_retrieve_key() {
        let mut manager = AcquireKeyState::new();
        let expected_str = "3efdaebb1d8923ebd99c9e7ace3b4194ab45512e2be79c1b7d68d9243e0d2681";
        let expected_key = Keypair::only_pubkey(Pubkey::from_hex(expected_str).unwrap());

        let start_time = Instant::now();

        while start_time.elapsed() < Duration::from_millis(50u64) {
            let cur_time = start_time.elapsed();

            if cur_time < Duration::from_millis(10u64) {
                let _ = manager.get_acquire_textedit(|text| {
                    text.clear();
                    text.insert_text("test", 0);
                    egui::TextEdit::singleline(text)
                });
                manager.apply_acquire();
            } else if cur_time < Duration::from_millis(30u64) {
                let _ = manager.get_acquire_textedit(|text| {
                    text.clear();
                    text.insert_text("test2", 0);
                    egui::TextEdit::singleline(text)
                });
                manager.apply_acquire();
            } else {
                let _ = manager.get_acquire_textedit(|text| {
                    text.clear();
                    text.insert_text(
                        "3efdaebb1d8923ebd99c9e7ace3b4194ab45512e2be79c1b7d68d9243e0d2681",
                        0,
                    );
                    egui::TextEdit::singleline(text)
                });
                manager.apply_acquire();
            }

            if let Some(key) = manager.check_for_successful_login() {
                assert_eq!(expected_key, key);
                return;
            }
        }

        panic!("Test failed to get expected key.");
    }
}
