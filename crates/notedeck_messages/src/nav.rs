use egui_nav::{NavAction, NavResponse};
use enostr::Pubkey;
use hashbrown::HashSet;
use notedeck::{AppAction, AppContext, ReplacementType, Router};

use crate::{
    cache::{
        ConversationCache, ConversationId, ConversationIdentifierUnowned, ParticipantSetUnowned,
    },
    loader::MessagesLoader,
    nip17::send_conversation_message,
};

#[derive(Clone, Debug)]
pub enum Route {
    ConvoList,
    CreateConvo,
    Conversation,
}

#[derive(Debug)]
pub enum MessagesAction {
    SendMessage {
        conversation_id: ConversationId,
        content: String,
    },
    Open(ConversationId),
    Creating,
    Back,
    Create {
        recipient: Pubkey,
    },
    ToggleChrome,
}

pub struct MessagesUiResponse {
    pub nav_response: Option<NavResponse<Option<MessagesAction>>>,
    pub conversation_panel_response: Option<MessagesAction>,
}

/// Apply UI responses and navigation actions to the messages router.
pub fn process_messages_ui_response(
    resp: MessagesUiResponse,
    ctx: &mut AppContext,
    cache: &mut ConversationCache,
    router: &mut Router<Route>,
    is_narrow: bool,
    loader: &MessagesLoader,
    inflight_messages: &mut HashSet<ConversationId>,
) -> Option<AppAction> {
    let mut action = None;
    if let Some(convo_resp) = resp.conversation_panel_response {
        action = handle_messages_action(
            convo_resp,
            ctx,
            cache,
            router,
            is_narrow,
            loader,
            inflight_messages,
        );
    }

    let Some(nav) = resp.nav_response else {
        return action;
    };

    action.or(process_nav_resp(
        nav,
        ctx,
        cache,
        router,
        is_narrow,
        loader,
        inflight_messages,
    ))
}

/// Handle navigation responses emitted by the messages UI.
fn process_nav_resp(
    nav: NavResponse<Option<MessagesAction>>,
    ctx: &mut AppContext,
    cache: &mut ConversationCache,
    router: &mut Router<Route>,
    is_narrow: bool,
    loader: &MessagesLoader,
    inflight_messages: &mut HashSet<ConversationId>,
) -> Option<AppAction> {
    let mut app_action = None;
    if let Some(action) = nav.response.or(nav.title_response) {
        app_action = handle_messages_action(
            action,
            ctx,
            cache,
            router,
            is_narrow,
            loader,
            inflight_messages,
        );
    }

    let Some(action) = nav.action else {
        return app_action;
    };

    match action {
        NavAction::Returning(_) => {}
        NavAction::Resetting => {}
        NavAction::Dragging => {}
        NavAction::Returned(_) => {
            router.pop();
            if is_narrow {
                cache.active = None;
            }
        }
        NavAction::Navigating => {}
        NavAction::Navigated => {
            router.navigating = false;
            if router.is_replacing() {
                router.complete_replacement();
            }
        }
    }

    app_action
}

/// Execute a messages action and return an optional app action.
fn handle_messages_action(
    action: MessagesAction,
    ctx: &mut AppContext<'_>,
    cache: &mut ConversationCache,
    router: &mut Router<Route>,
    is_narrow: bool,
    loader: &MessagesLoader,
    inflight_messages: &mut HashSet<ConversationId>,
) -> Option<AppAction> {
    let mut app_action = None;
    match action {
        MessagesAction::SendMessage {
            conversation_id,
            content,
        } => send_conversation_message(conversation_id, content, cache, ctx),
        MessagesAction::Open(conversation_id) => open_coversation_action(
            conversation_id,
            ctx,
            cache,
            router,
            is_narrow,
            loader,
            inflight_messages,
        ),
        MessagesAction::Create { recipient } => {
            let selected = ctx.accounts.selected_account_pubkey();
            let participants = vec![recipient.bytes(), selected.bytes()];
            let id = cache
                .registry
                .get_or_insert(ConversationIdentifierUnowned::Nip17(
                    ParticipantSetUnowned::new(participants),
                ));

            cache.initialize_conversation(id, vec![recipient, *selected]);
            cache.active = Some(id);
            request_conversation_messages(
                cache,
                ctx.accounts.selected_account_pubkey(),
                id,
                loader,
                inflight_messages,
            );

            if is_narrow {
                router.route_to_replaced(Route::Conversation, ReplacementType::Single);
            } else {
                router.go_back();
            }
        }
        MessagesAction::Creating => {
            router.route_to(Route::CreateConvo);
        }
        MessagesAction::Back => {
            router.go_back();
        }
        MessagesAction::ToggleChrome => app_action = Some(AppAction::ToggleChrome),
    }

    app_action
}

/// Activate a conversation and request its message history.
fn open_coversation_action(
    id: ConversationId,
    ctx: &mut AppContext<'_>,
    cache: &mut ConversationCache,
    router: &mut Router<Route>,
    is_narrow: bool,
    loader: &MessagesLoader,
    inflight_messages: &mut HashSet<ConversationId>,
) {
    cache.active = Some(id);
    request_conversation_messages(
        cache,
        ctx.accounts.selected_account_pubkey(),
        id,
        loader,
        inflight_messages,
    );

    if is_narrow {
        router.route_to(Route::Conversation);
    }
}

/// Schedule a background load for a conversation if it is not already in flight.
fn request_conversation_messages(
    cache: &ConversationCache,
    me: &Pubkey,
    conversation_id: ConversationId,
    loader: &MessagesLoader,
    inflight_messages: &mut HashSet<ConversationId>,
) {
    if inflight_messages.contains(&conversation_id) {
        return;
    }

    let Some(conversation) = cache.get(conversation_id) else {
        return;
    };

    inflight_messages.insert(conversation_id);
    loader.load_conversation_messages(
        conversation_id,
        conversation.metadata.participants.clone(),
        *me,
    );
}
