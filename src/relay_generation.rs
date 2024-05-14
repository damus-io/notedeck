use crate::relay_pool_manager::create_wakeup;
use enostr::{Pubkey, RelayPool};
use tracing::error;

pub enum RelayGenerator {
    GossipModel,
    Nip65,
    Constant,
}

impl RelayGenerator {
    pub fn generate_relays_for(&self, key: &Pubkey, ctx: &egui::Context) -> RelayPool {
        match self {
            Self::GossipModel => generate_relays_gossip(key, ctx),
            Self::Nip65 => generate_relays_nip65(key, ctx),
            Self::Constant => generate_constant_relays(ctx),
        }
    }
}

fn generate_relays_gossip(key: &Pubkey, ctx: &egui::Context) -> RelayPool {
    let _ = ctx;
    let _ = key;
    todo!()
}

fn generate_relays_nip65(key: &Pubkey, ctx: &egui::Context) -> RelayPool {
    let _ = ctx;
    let _ = key;
    todo!()
}

fn generate_constant_relays(ctx: &egui::Context) -> RelayPool {
    let mut pool = RelayPool::new();
    let wakeup = create_wakeup(ctx);

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
