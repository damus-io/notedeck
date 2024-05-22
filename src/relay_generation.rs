use enostr::{Pubkey, RelayPool};
use tracing::error;

pub enum RelayGenerator {
    GossipModel,
    Nip65,
    Constant,
}

impl RelayGenerator {
    pub fn generate_relays_for(
        &self,
        key: &Pubkey,
        wakeup: impl Fn() + Send + Sync + Clone + 'static,
    ) -> RelayPool {
        match self {
            Self::GossipModel => generate_relays_gossip(key, wakeup),
            Self::Nip65 => generate_relays_nip65(key, wakeup),
            Self::Constant => generate_constant_relays(wakeup),
        }
    }
}

fn generate_relays_gossip(
    key: &Pubkey,
    wakeup: impl Fn() + Send + Sync + Clone + 'static,
) -> RelayPool {
    let _ = wakeup;
    let _ = key;
    todo!()
}

fn generate_relays_nip65(
    key: &Pubkey,
    wakeup: impl Fn() + Send + Sync + Clone + 'static,
) -> RelayPool {
    let _ = wakeup;
    let _ = key;
    todo!()
}

fn generate_constant_relays(wakeup: impl Fn() + Send + Sync + Clone + 'static) -> RelayPool {
    let mut pool = RelayPool::new();

    if let Err(e) = pool.add_url("ws://localhost:8080".to_string(), wakeup.clone()) {
        error!("{:?}", e)
    }
    if let Err(e) = pool.add_url("wss://relay.damus.io".to_string(), wakeup.clone()) {
        error!("{:?}", e)
    }
    if let Err(e) = pool.add_url("wss://pyramid.fiatjaf.com".to_string(), wakeup.clone()) {
        error!("{:?}", e)
    }
    if let Err(e) = pool.add_url("wss://nos.lol".to_string(), wakeup.clone()) {
        error!("{:?}", e)
    }
    if let Err(e) = pool.add_url("wss://nostr.wine".to_string(), wakeup.clone()) {
        error!("{:?}", e)
    }
    if let Err(e) = pool.add_url("wss://purplepag.es".to_string(), wakeup) {
        error!("{:?}", e)
    }

    pool
}
