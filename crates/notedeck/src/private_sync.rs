//! Cross-device "private relay" sync for GUI apps (headway, notebook).
//!
//! These apps back their document in the local nostrdb and surface it through a
//! local nostrdb subscription. To sync that document across the user's own
//! devices we need two directions over the account's private-sync relays — the
//! kind-10013 NIP-37 "Relay List for Private Content" (an encrypted relay list,
//! see [`crate::construct_private_relay_list_note`]):
//!
//! - **outbound** — fan each locally-ingested event out to the private relays so
//!   an edit on this device reaches the others ([`fan_out_event_frame`]);
//! - **inbound** — a scoped subscription that pulls the app's events back, both
//!   a NIP-77 full-history catch-up *and* a live REQ for realtime, so edits made
//!   on another device land in nostrdb and the local subscription surfaces them
//!   ([`PrivateRelaySync`]).
//!
//! dave already does the equivalent for its PNS session state; this is the
//! shared, domain-agnostic version for the plaintext-event apps. With no private
//! relay marked the relay set is empty and both directions are no-ops, so the
//! app stays purely local.

use enostr::{NormRelayUrl, Pubkey, RelayId};
use hashbrown::HashSet;
use nostrdb::Filter;

use crate::{
    AppContext, ExplicitPublishApi, FullHistoryConfig, ScopedSubIdentity, SubConfig, SubKey,
    SubOwnerKey,
};

/// Fan a single locally-ingested `["EVENT", {…}]` frame out to `relays` as a
/// bare-event publish. The outbox re-frames the bare event per relay.
///
/// Shared by the headway/notebook `store::Publisher` adapters: their `ingest`
/// path hands us the framed event, we forward the inner object. An empty relay
/// set or a malformed frame is a no-op — the local ingest already happened.
pub fn fan_out_event_frame(api: &mut ExplicitPublishApi, event_frame: &str, relays: &[RelayId]) {
    if relays.is_empty() {
        return;
    }
    if let Some(event) = serde_json::from_str::<serde_json::Value>(event_frame)
        .ok()
        .and_then(|frame| frame.get(1).cloned())
    {
        api.publish_event_json(event.to_string(), relays.to_vec());
    }
}

/// Declares (and tears down) the inbound private-sync subscription for one GUI
/// app, deduping the work so it only touches the outbox when the resolved
/// private relay set actually changes.
///
/// Hold one per app and call [`update`](Self::update) each frame with the app's
/// event filter; it returns the resolved private relays so the caller can reuse
/// them as outbound publish targets (see [`fan_out_event_frame`]).
pub struct PrivateRelaySync {
    /// Human-readable app name, for log lines.
    app: &'static str,
    /// Scoped-sub owner lifecycle, namespaced per app so two apps' private subs
    /// never collide on the shared outbox.
    owner: SubOwnerKey,
    /// Logical sub key under that owner.
    key: SubKey,
    /// Last resolved (selected account, private relay set), so we only
    /// re-declare (and log) on a change rather than every frame. The account is
    /// part of the key so switching accounts still re-declares even if the two
    /// accounts happen to share a private relay set.
    last: Option<(Pubkey, Vec<NormRelayUrl>)>,
}

impl PrivateRelaySync {
    /// Create a private-sync coordinator for `app` (e.g. `"headway"`,
    /// `"notebook"`). `app` seeds a stable, app-unique scoped-sub owner/key.
    pub fn new(app: &'static str) -> Self {
        Self {
            app,
            owner: SubOwnerKey::new(format!("{app}/private-sync")),
            key: SubKey::new("private-sync"),
            last: None,
        }
    }

    /// Bring the inbound subscription in line with the selected account's
    /// private relays, declaring a live + full-history scoped sub for `filter`
    /// against them (or dropping it when none are marked). Returns the resolved
    /// private relays for use as outbound publish targets.
    pub fn update(&mut self, ctx: &mut AppContext, filter: Filter) -> Vec<RelayId> {
        let relays = ctx.accounts.selected_account_private_relays();
        let urls: Vec<NormRelayUrl> = relays
            .iter()
            .filter_map(|relay| match relay {
                RelayId::Websocket(url) => Some(url.clone()),
                RelayId::Multicast => None,
            })
            .collect();

        // Nothing to do unless the account or its private relay set changed.
        // set_sub/drop_owner each re-resolve the account's read relays (a hot,
        // log-emitting path), so calling them every frame spams the logs and
        // wastes work — dedup before touching the outbox at all.
        let pubkey = *ctx.accounts.selected_account_pubkey();
        if self
            .last
            .as_ref()
            .is_some_and(|(pk, last)| *pk == pubkey && last.as_slice() == urls.as_slice())
        {
            return relays;
        }
        self.log_change(ctx, &urls);
        self.last = Some((pubkey, urls.clone()));

        let mut scoped = ctx.remote.scoped_subs(ctx.accounts);
        if urls.is_empty() {
            // No private relay marked: local-only. Drop any prior declaration.
            scoped.drop_owner(self.owner);
            return relays;
        }

        let config = SubConfig::live(vec![filter.clone()])
            .explicit_relays(urls.into_iter().collect::<HashSet<_>>())
            .full_history(FullHistoryConfig::new(vec![filter]))
            .build();
        let _ = scoped.set_sub(ScopedSubIdentity::account(self.owner, self.key), config);

        relays
    }

    /// Log the private relay set (and the live connection status of each) — the
    /// diagnostic for "is the private set even resolving?". The caller only
    /// invokes this on an actual change, so this never spams a line every frame.
    fn log_change(&self, ctx: &AppContext, urls: &[NormRelayUrl]) {
        if urls.is_empty() {
            tracing::info!(
                app = self.app,
                "private-sync: no private relay marked — local-only"
            );
            return;
        }

        let inspect = ctx.remote.relay_inspect();
        let infos = inspect.relay_infos();
        let statuses: Vec<String> = urls
            .iter()
            .map(|url| {
                let status = infos
                    .iter()
                    .find(|info| info.relay_url == url)
                    .map(|info| format!("{:?}", info.status))
                    .unwrap_or_else(|| "NotConnected".to_string());
                format!("{url} ({status})")
            })
            .collect();
        tracing::info!(
            app = self.app,
            relays = %statuses.join(", "),
            "private-sync: syncing against private relays"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EguiWakeup, ExplicitPublishApi};
    use enostr::{FullKeypair, NormRelayUrl, OutboxPool, OutboxSessionHandler};
    use nostrdb::NoteBuilder;

    /// Frame a signed note as the `["EVENT", {…}]` envelope `ingest` hands the
    /// publisher.
    fn event_frame() -> String {
        let kp = FullKeypair::generate();
        let note = NoteBuilder::new()
            .kind(1)
            .content("private-sync-test")
            .sign(&kp.secret_key.to_secret_bytes())
            .build()
            .expect("note");
        let event: serde_json::Value =
            serde_json::from_str(&note.json().expect("event json")).expect("event value");
        serde_json::json!(["EVENT", event]).to_string()
    }

    /// Drive `fan_out_event_frame` and return the relay set the outbox opened.
    fn relays_opened_for(frame: &str, relays: Vec<RelayId>) -> HashSet<NormRelayUrl> {
        let mut pool = OutboxPool::default();
        {
            let mut outbox =
                OutboxSessionHandler::new(&mut pool, EguiWakeup::new(egui::Context::default()));
            let mut api = ExplicitPublishApi::new(&mut outbox);
            fan_out_event_frame(&mut api, frame, &relays);
        }
        pool.websocket_statuses()
            .keys()
            .map(|url| (*url).clone())
            .collect()
    }

    /// A well-formed frame is unwrapped and published to each target relay.
    #[tokio::test]
    async fn fan_out_publishes_inner_event_to_targets() {
        let relay = NormRelayUrl::new("wss://private.example.com").expect("relay");
        let opened = relays_opened_for(&event_frame(), vec![RelayId::Websocket(relay.clone())]);
        assert_eq!(opened, HashSet::from_iter([relay]));
    }

    /// An empty relay set is a no-op — no relay connection is opened.
    #[test]
    fn fan_out_empty_relays_is_noop() {
        assert!(relays_opened_for(&event_frame(), vec![]).is_empty());
    }

    /// A malformed frame (no inner event object) opens no relay; the local ingest
    /// has already happened, so there's nothing to forward.
    #[test]
    fn fan_out_malformed_frame_is_noop() {
        let relay = RelayId::Websocket(NormRelayUrl::new("wss://private.example.com").expect("r"));
        assert!(relays_opened_for("not json", vec![relay.clone()]).is_empty());
        assert!(relays_opened_for("[\"EVENT\"]", vec![relay]).is_empty());
    }
}
