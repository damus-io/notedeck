use std::collections::HashMap;
use std::str::FromStr;

use crate::Error;
use ehttp::{Request, Response};
use nostr_sdk::{prelude::Keys, PublicKey, SecretKey};
use poll_promise::Promise;
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq)]
pub enum LoginError {
    InvalidKey,
    Nip05Failed(String),
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            LoginError::InvalidKey => write!(f, "The inputted key is invalid."),
            LoginError::Nip05Failed(e) => write!(f, "Failed to get pubkey from Nip05 address: {e}"),
        }
    }
}

impl std::error::Error for LoginError {}

#[derive(Deserialize, Serialize)]
pub struct Nip05Result {
    pub names: HashMap<String, String>,
    pub relays: Option<HashMap<String, Vec<String>>>,
}

fn parse_nip05_response(response: Response) -> Result<Nip05Result, Error> {
    serde_json::from_slice::<Nip05Result>(&response.bytes)
        .map_err(|e| Error::Generic(e.to_string()))
}

fn get_pubkey_from_result(result: Nip05Result, user: String) -> Result<PublicKey, Error> {
    match result.names.get(&user).to_owned() {
        Some(pubkey_str) => PublicKey::from_str(pubkey_str).map_err(|e| {
            Error::Generic("Could not parse pubkey: ".to_string() + e.to_string().as_str())
        }),
        None => Err(Error::Generic("Could not find user in json.".to_string())),
    }
}

fn get_nip05_pubkey(id: &str) -> Promise<Result<PublicKey, Error>> {
    let (sender, promise) = Promise::new();
    let mut parts = id.split('@');

    let user = match parts.next() {
        Some(user) => user,
        None => {
            sender.send(Err(Error::Generic(
                "Address does not contain username.".to_string(),
            )));
            return promise;
        }
    };
    let host = match parts.next() {
        Some(host) => host,
        None => {
            sender.send(Err(Error::Generic(
                "Nip05 address does not contain host.".to_string(),
            )));
            return promise;
        }
    };

    if parts.next().is_some() {
        sender.send(Err(Error::Generic(
            "Nip05 address contains extraneous parts.".to_string(),
        )));
        return promise;
    }

    let url = format!("https://{host}/.well-known/nostr.json?name={user}");
    let request = Request::get(url);

    let cloned_user = user.to_string();
    ehttp::fetch(request, move |response: Result<Response, String>| {
        let result = match response {
            Ok(resp) => parse_nip05_response(resp)
                .and_then(move |result| get_pubkey_from_result(result, cloned_user)),
            Err(e) => Err(Error::Generic(e.to_string())),
        };
        sender.send(result);
    });

    promise
}

fn retrieving_nip05_pubkey(key: &str) -> bool {
    key.contains('@')
}

fn nip05_promise_wrapper(id: &str) -> Promise<Result<Keys, LoginError>> {
    let (sender, promise) = Promise::new();
    let original_promise = get_nip05_pubkey(id);

    std::thread::spawn(move || {
        let result = original_promise.block_and_take();
        let transformed_result = match result {
            Ok(public_key) => Ok(Keys::from_public_key(public_key)),
            Err(e) => Err(LoginError::Nip05Failed(e.to_string())),
        };
        sender.send(transformed_result);
    });

    promise
}

/// Attempts to turn a string slice key from the user into a Nostr-Sdk Keys object.
/// The `key` can be in any of the following formats:
/// - Public Bech32 key (prefix "npub"): "npub1xyz..."
/// - Private Bech32 key (prefix "nsec"): "nsec1xyz..."
/// - Public hex key: "02a1..."
/// - Private hex key: "5dab..."
/// - NIP-05 address: "example@nostr.com"
///
/// For NIP-05 addresses, retrieval of the public key is an asynchronous operation that returns a `Promise`, so it
/// will not be immediately ready.
/// All other key formats are processed synchronously even though they are still behind a Promise, they will be
/// available immediately.
///
/// Returns a `Promise` that resolves to `Result<Keys, LoginError>`. `LoginError` is returned in case of invalid format,
/// unsupported key types, or network errors during NIP-05 address resolution.
///
pub fn perform_key_retrieval(key: &str) -> Promise<Result<Keys, LoginError>> {
    let tmp_key: &str = if let Some(stripped) = key.strip_prefix('@') {
        stripped
    } else {
        key
    };

    if retrieving_nip05_pubkey(tmp_key) {
        nip05_promise_wrapper(tmp_key)
    } else {
        let result: Result<Keys, LoginError> = if let Ok(pubkey) = PublicKey::from_str(tmp_key) {
            Ok(Keys::from_public_key(pubkey))
        } else if let Ok(secret_key) = SecretKey::from_str(tmp_key) {
            Ok(Keys::new(secret_key))
        } else {
            Err(LoginError::InvalidKey)
        };
        Promise::from_ready(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::promise_assert;

    #[test]
    fn test_pubkey() {
        let pubkey_str = "npub1xtscya34g58tk0z605fvr788k263gsu6cy9x0mhnm87echrgufzsevkk5s";
        let expected_pubkey = PublicKey::from_str(pubkey_str).expect("Should not have errored.");
        let login_key_result = perform_key_retrieval(pubkey_str);

        promise_assert!(
            assert_eq,
            Ok(Keys::from_public_key(expected_pubkey)),
            &login_key_result
        );
    }

    #[test]
    fn test_hex_pubkey() {
        let pubkey_str = "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245";
        let expected_pubkey = PublicKey::from_str(pubkey_str).expect("Should not have errored.");
        let login_key_result = perform_key_retrieval(pubkey_str);

        promise_assert!(
            assert_eq,
            Ok(Keys::from_public_key(expected_pubkey)),
            &login_key_result
        );
    }

    #[test]
    fn test_privkey() {
        let privkey_str = "nsec1g8wt3hlwjpa4827xylr3r0lccufxltyekhraexes8lqmpp2hensq5aujhs";
        let expected_privkey = SecretKey::from_str(privkey_str).expect("Should not have errored.");
        let login_key_result = perform_key_retrieval(privkey_str);

        promise_assert!(
            assert_eq,
            Ok(Keys::new(expected_privkey)),
            &login_key_result
        );
    }

    #[test]
    fn test_hex_privkey() {
        let privkey_str = "41dcb8dfee907b53abc627c711bff8c7126fac99b5c7dc9b303fc1b08557cce0";
        let expected_privkey = SecretKey::from_str(privkey_str).expect("Should not have errored.");
        let login_key_result = perform_key_retrieval(privkey_str);

        promise_assert!(
            assert_eq,
            Ok(Keys::new(expected_privkey)),
            &login_key_result
        );
    }

    #[test]
    fn test_nip05() {
        let nip05_str = "damus@damus.io";
        let expected_pubkey =
            PublicKey::from_str("npub18m76awca3y37hkvuneavuw6pjj4525fw90necxmadrvjg0sdy6qsngq955")
                .expect("Should not have errored.");
        let login_key_result = perform_key_retrieval(nip05_str);

        promise_assert!(
            assert_eq,
            Ok(Keys::from_public_key(expected_pubkey)),
            &login_key_result
        );
    }

    #[test]
    fn test_nip05_pubkey() {
        let nip05_str = "damus@damus.io";
        let expected_pubkey =
            PublicKey::from_str("npub18m76awca3y37hkvuneavuw6pjj4525fw90necxmadrvjg0sdy6qsngq955")
                .expect("Should not have errored.");
        let login_key_result = get_nip05_pubkey(nip05_str);

        let res = login_key_result.block_and_take().expect("Should not error");
        assert_eq!(expected_pubkey, res);
    }
}
