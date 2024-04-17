use enostr::RelayPool;
use notedeck::{relay_pool_manager::RelayPoolManager, relay_view::RelayView};

use crate::egui_preview_setup::{EguiPreviewCase, EguiPreviewSetup};

pub struct RelayViewPreview {
    pool: RelayPool,
}

#[allow(unused_must_use)]
impl EguiPreviewCase for RelayViewPreview {
    fn new(_supr: EguiPreviewSetup) -> Self {
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

        RelayViewPreview { pool }
    }
}

impl eframe::App for RelayViewPreview {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        self.pool.try_recv();
        RelayView::new(ctx, RelayPoolManager::new(&mut self.pool)).panel();
    }
}
