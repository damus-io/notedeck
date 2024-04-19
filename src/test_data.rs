use enostr::RelayPool;

#[allow(unused_must_use)]
pub fn sample_pool() -> RelayPool {
    let mut pool = RelayPool::new();
    let wakeup = move || {};

    pool.add_url("wss://relay.damus.io".to_string(), wakeup);
    pool.add_url("wss://eden.nostr.land".to_string(), wakeup);
    pool.add_url("wss://nostr.wine".to_string(), wakeup);
    pool.add_url("wss://nos.lol".to_string(), wakeup);
    pool.add_url("wss://test_relay_url_long_00000000000000000000000000000000000000000000000000000000000000000000000000000000000".to_string(), wakeup);

    for _ in 0..20 {
        pool.add_url("tmp".to_string(), wakeup);
    }

    pool
}
