use std::collections::HashMap;
use std::str::FromStr;

use crate::Error;
use enostr::{Keypair, Pubkey, SecretKey};
use poll_promise::Promise;
use reqwest::{Request, Response};
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

async fn parse_nip05_response(response: Response) -> Result<Nip05Result, Error> {
    match response.bytes().await {
        Ok(bytes) => {
            serde_json::from_slice::<Nip05Result>(&bytes).map_err(|e| Error::Generic(e.to_string()))
        }
        Err(e) => Err(Error::Generic(e.to_string())),
    }
}

fn get_pubkey_from_result(result: Nip05Result, user: String) -> Result<Pubkey, Error> {
    match result.names.get(&user).to_owned() {
        Some(pubkey_str) => Pubkey::from_hex(pubkey_str).map_err(|e| {
            Error::Generic("Could not parse pubkey: ".to_string() + e.to_string().as_str())
        }),
        None => Err(Error::Generic("Could not find user in json.".to_string())),
    }
}

async fn get_nip05_pubkey(id: &str) -> Result<Pubkey, Error> {
    let mut parts = id.split('@');

    let user = match parts.next() {
        Some(user) => user,
        None => {
            return Err(Error::Generic(
                "Address does not contain username.".to_string(),
            ));
        }
    };
    let host = match parts.next() {
        Some(host) => host,
        None => {
            return Err(Error::Generic(
                "Nip05 address does not contain host.".to_string(),
            ));
        }
    };

    if parts.next().is_some() {
        return Err(Error::Generic(
            "Nip05 address contains extraneous parts.".to_string(),
        ));
    }

    let url = format!("https://{host}/.well-known/nostr.json?name={user}");
    let request = Request::new(reqwest::Method::GET, url.parse().unwrap());
    let cloned_user = user.to_string();

    let client = reqwest::Client::new();
    match client.execute(request).await {
        Ok(resp) => match parse_nip05_response(resp).await {
            Ok(result) => match get_pubkey_from_result(result, cloned_user) {
                Ok(pubkey) => Ok(pubkey),
                Err(e) => Err(Error::Generic(e.to_string())),
            },
            Err(e) => Err(Error::Generic(e.to_string())),
        },
        Err(e) => Err(Error::Generic(e.to_string())),
    }
}

fn retrieving_nip05_pubkey(key: &str) -> bool {
    key.contains('@')
}

pub fn perform_key_retrieval(key: &str) -> Promise<Result<Keypair, LoginError>> {
    let key_string = String::from(key);
    Promise::spawn_async(async move { get_login_key(&key_string).await })
}

/// Attempts to turn a string slice key from the user into a Nostr-Sdk Keypair object.
/// The `key` can be in any of the following formats:
/// - Public Bech32 key (prefix "npub"): "npub1xyz..."
/// - Private Bech32 key (prefix "nsec"): "nsec1xyz..."
/// - Public hex key: "02a1..."
/// - Private hex key: "5dab..."
/// - NIP-05 address: "example@nostr.com"
///
pub async fn get_login_key(key: &str) -> Result<Keypair, LoginError> {
    let tmp_key: &str = if let Some(stripped) = key.strip_prefix('@') {
        stripped
    } else {
        key
    };

    if retrieving_nip05_pubkey(tmp_key) {
        match get_nip05_pubkey(tmp_key).await {
            Ok(pubkey) => Ok(Keypair::only_pubkey(pubkey)),
            Err(e) => Err(LoginError::Nip05Failed(e.to_string())),
        }
    } else if let Ok(pubkey) = Pubkey::try_from_bech32_string(tmp_key, true) {
        Ok(Keypair::only_pubkey(pubkey))
    } else if let Ok(pubkey) = Pubkey::try_from_hex_str_with_verify(tmp_key) {
        Ok(Keypair::only_pubkey(pubkey))
    } else if let Ok(secret_key) = SecretKey::from_str(tmp_key) {
        Ok(Keypair::new(secret_key))
    } else {
        Err(LoginError::InvalidKey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::promise_assert;

    #[tokio::test]
    async fn test_pubkey_async() {
        let pubkey_str = "npub1xtscya34g58tk0z605fvr788k263gsu6cy9x0mhnm87echrgufzsevkk5s";
        let expected_pubkey =
            Pubkey::try_from_bech32_string(pubkey_str, false).expect("Should not have errored.");
        let login_key_result = get_login_key(pubkey_str).await;

        assert_eq!(Ok(Keypair::only_pubkey(expected_pubkey)), login_key_result);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_pubkey() {
        let pubkey_str = "npub1xtscya34g58tk0z605fvr788k263gsu6cy9x0mhnm87echrgufzsevkk5s";
        let expected_pubkey =
            Pubkey::try_from_bech32_string(pubkey_str, false).expect("Should not have errored.");
        let login_key_result = perform_key_retrieval(pubkey_str);

        promise_assert!(
            assert_eq,
            Ok(Keypair::only_pubkey(expected_pubkey)),
            &login_key_result
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_hex_pubkey() {
        let pubkey_str = "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245";
        let expected_pubkey = Pubkey::from_hex(pubkey_str).expect("Should not have errored.");
        let login_key_result = perform_key_retrieval(pubkey_str);

        promise_assert!(
            assert_eq,
            Ok(Keypair::only_pubkey(expected_pubkey)),
            &login_key_result
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_privkey() {
        let privkey_str = "nsec1g8wt3hlwjpa4827xylr3r0lccufxltyekhraexes8lqmpp2hensq5aujhs";
        let expected_privkey = SecretKey::from_str(privkey_str).expect("Should not have errored.");
        let login_key_result = perform_key_retrieval(privkey_str);

        promise_assert!(
            assert_eq,
            Ok(Keypair::new(expected_privkey)),
            &login_key_result
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_hex_privkey() {
        let privkey_str = "41dcb8dfee907b53abc627c711bff8c7126fac99b5c7dc9b303fc1b08557cce0";
        let expected_privkey = SecretKey::from_str(privkey_str).expect("Should not have errored.");
        let login_key_result = perform_key_retrieval(privkey_str);

        promise_assert!(
            assert_eq,
            Ok(Keypair::new(expected_privkey)),
            &login_key_result
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_nip05() {
        let nip05_str = "damus@damus.io";
        let expected_pubkey = Pubkey::try_from_bech32_string(
            "npub18m76awca3y37hkvuneavuw6pjj4525fw90necxmadrvjg0sdy6qsngq955",
            false,
        )
        .expect("Should not have errored.");
        let login_key_result = perform_key_retrieval(nip05_str);

        promise_assert!(
            assert_eq,
            Ok(Keypair::only_pubkey(expected_pubkey)),
            &login_key_result
        );
    }
}
