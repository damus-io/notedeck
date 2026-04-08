use crate::{EguiWakeup, RemoteApi, ScopedSubsState, SubKey, SubScope};
use enostr::{OutboxPool, OutboxSessionHandler, OutboxSubId, Pubkey};

pub(crate) fn remote_for_test<'a>(
    pool: &'a mut OutboxPool,
    scoped_sub_state: &'a mut ScopedSubsState,
) -> RemoteApi<'a> {
    RemoteApi::new(
        OutboxSessionHandler::new(pool, EguiWakeup::new(egui::Context::default())),
        scoped_sub_state,
    )
}

pub(crate) fn live_id_with_selected_for_test(
    scoped_sub_state: &mut ScopedSubsState,
    selected_account_pubkey: Pubkey,
    key: SubKey,
    scope: SubScope,
) -> Option<OutboxSubId> {
    scoped_sub_state
        .runtime_mut()
        .live_id_with_selected(selected_account_pubkey, key, scope)
}
