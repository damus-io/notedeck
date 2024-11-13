use enostr::RelayPool;
pub use enostr::RelayStatus;

/// The interface to a RelayPool for UI components.
/// Represents all user-facing operations that can be performed for a user's relays
pub struct RelayPoolManager<'a> {
    pub pool: &'a mut RelayPool,
}

pub struct RelayInfo<'a> {
    pub relay_url: &'a str,
    pub status: &'a RelayStatus,
}

impl<'a> RelayPoolManager<'a> {
    pub fn new(pool: &'a mut RelayPool) -> Self {
        RelayPoolManager { pool }
    }

    pub fn get_relay_infos(&self) -> Vec<RelayInfo> {
        self.pool
            .relays
            .iter()
            .map(|relay| RelayInfo {
                relay_url: &relay.relay.url,
                status: &relay.relay.status,
            })
            .collect()
    }

    /// index of the Vec<RelayInfo> from get_relay_infos
    pub fn remove_relay(&mut self, index: usize) {
        if index < self.pool.relays.len() {
            self.pool.relays.remove(index);
        }
    }

    /// removes all specified relay indicies shown in get_relay_infos
    pub fn remove_relays(&mut self, mut indices: Vec<usize>) {
        indices.sort_unstable_by(|a, b| b.cmp(a));
        indices.iter().for_each(|index| self.remove_relay(*index));
    }

    // This wants to add the relay to one of the pool collections
    // (bootstrapping, local, advertised, or forced) and then call
    // configure_relays ...
    /*
    pub fn add_relay(&mut self, ctx: &egui::Context, relay_url: String) {
        let _ = self.pool.add_url(relay_url, create_wakeup(ctx));
    }
    */
}

pub fn create_wakeup(ctx: &egui::Context) -> impl Fn() + Send + Sync + Clone + 'static {
    let ctx = ctx.clone();
    move || {
        ctx.request_repaint();
    }
}
