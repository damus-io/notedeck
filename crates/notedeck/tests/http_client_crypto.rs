//! `HyperHttpClient` installs the process-level rustls crypto provider
//! itself, so a test or job thread that never ran the app's startup can't
//! panic on its first TLS handshake.
//!
//! This lives in its own test binary on purpose: the provider is process
//! global, and in a shared binary another test could have installed it
//! first, so the assert would pass whether or not the client does it.

use notedeck::network::HyperHttpClient;

/// Building a client is enough: no provider before, one after.
#[test]
fn building_a_client_installs_the_crypto_provider() {
    assert!(
        rustls::crypto::CryptoProvider::get_default().is_none(),
        "nothing else in this binary may install the provider first"
    );
    let _client = HyperHttpClient::new();
    assert!(rustls::crypto::CryptoProvider::get_default().is_some());
}
