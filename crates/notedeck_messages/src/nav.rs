use egui_nav::{NavAction, NavResponse};
use enostr::Pubkey;
use hashbrown::HashSet;
use notedeck::{AppAction, AppContext, NoteAction, ReplacementType, Router};

use crate::{
    cache::{
        ConversationCache, ConversationId, ConversationIdentifierUnowned, ParticipantSetUnowned,
    },
    loader::MessagesLoader,
    open_conversation_with_prefetch, ConversationLoadKey,
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
    Profile(Pubkey),
}

pub struct MessagesUiResponse {
    pub nav_response: Option<NavResponse<Option<MessagesAction>>>,
    pub conversation_panel_response: Option<MessagesAction>,
}

pub(crate) struct MessageSendRequest {
    pub(crate) conversation_id: ConversationId,
    pub(crate) content: String,
}

#[derive(Default)]
pub(crate) struct ProcessedMessagesUiResponse {
    pub(crate) app_action: Option<AppAction>,
    pub(crate) send_message: Option<MessageSendRequest>,
}

impl ProcessedMessagesUiResponse {
    fn or(mut self, next: Self) -> Self {
        if self.app_action.is_none() {
            self.app_action = next.app_action;
        }
        if self.send_message.is_none() {
            self.send_message = next.send_message;
        }
        self
    }
}

/// Returns whether route transitions should animate for the current app context.
fn nav_transitions_enabled(ctx: &mut AppContext<'_>) -> bool {
    ctx.settings.get_settings_mut().animate_nav_transitions
}

/// Pushes a route immediately when transitions are disabled.
fn route_to(router: &mut Router<Route>, route: Route, animate: bool) {
    if animate {
        router.route_to(route);
        return;
    }

    router.navigating = false;
    router.returning = false;
    router.routes.push(route);
}

/// Applies a replacement route immediately when transitions are disabled.
fn route_to_replaced(
    router: &mut Router<Route>,
    route: Route,
    replacement_type: ReplacementType,
    animate: bool,
) {
    if animate {
        router.route_to_replaced(route, replacement_type);
        return;
    }

    router.navigating = false;
    router.returning = false;
    match replacement_type {
        ReplacementType::Single => {
            router.routes.push(route);
            let len = router.routes.len();
            if len >= 2 {
                router.routes.remove(len - 2);
            }
        }
        ReplacementType::All => {
            router.routes.clear();
            router.routes.push(route);
        }
    }
}

/// Pops the current route immediately when transitions are disabled.
fn go_back(router: &mut Router<Route>, animate: bool) {
    if animate {
        let _ = router.go_back();
        return;
    }

    if router.routes.len() > 1 {
        router.navigating = false;
        router.returning = false;
        router.routes.pop();
    }
}

/// Apply UI responses and navigation actions to the messages router.
pub(crate) fn process_messages_ui_response(
    resp: MessagesUiResponse,
    ctx: &mut AppContext,
    cache: &mut ConversationCache,
    router: &mut Router<Route>,
    is_narrow: bool,
    loader: &MessagesLoader,
    inflight_messages: &mut HashSet<ConversationLoadKey>,
) -> ProcessedMessagesUiResponse {
    let mut action = ProcessedMessagesUiResponse::default();
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
    inflight_messages: &mut HashSet<ConversationLoadKey>,
) -> ProcessedMessagesUiResponse {
    let mut processed = ProcessedMessagesUiResponse::default();
    if let Some(action) = nav.response.or(nav.title_response) {
        processed = handle_messages_action(
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
        return processed;
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

    processed
}

/// Apply a messages action and return side effects that must be run by the app.
fn handle_messages_action(
    action: MessagesAction,
    ctx: &mut AppContext<'_>,
    cache: &mut ConversationCache,
    router: &mut Router<Route>,
    is_narrow: bool,
    loader: &MessagesLoader,
    inflight_messages: &mut HashSet<ConversationLoadKey>,
) -> ProcessedMessagesUiResponse {
    let mut processed = ProcessedMessagesUiResponse::default();
    let animate_nav = nav_transitions_enabled(ctx);
    match action {
        MessagesAction::SendMessage {
            conversation_id,
            content,
        } => {
            processed.send_message = Some(MessageSendRequest {
                conversation_id,
                content,
            })
        }
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
            open_conversation_with_prefetch(&mut ctx.remote, ctx.accounts, cache, id);
            request_conversation_messages(
                cache,
                ctx.accounts.selected_account_pubkey(),
                id,
                loader,
                inflight_messages,
            );

            if is_narrow {
                route_to_replaced(
                    router,
                    Route::Conversation,
                    ReplacementType::Single,
                    animate_nav,
                );
            } else {
                go_back(router, animate_nav);
            }
        }
        MessagesAction::Creating => {
            route_to(router, Route::CreateConvo, animate_nav);
        }
        MessagesAction::Back => {
            go_back(router, animate_nav);
        }
        MessagesAction::ToggleChrome => processed.app_action = Some(AppAction::ToggleChrome),
        MessagesAction::Profile(pubkey) => {
            processed.app_action = Some(AppAction::Note(NoteAction::Profile(pubkey)));
        }
    }

    processed
}

/// Activate a conversation and request its message history.
fn open_coversation_action(
    id: ConversationId,
    ctx: &mut AppContext<'_>,
    cache: &mut ConversationCache,
    router: &mut Router<Route>,
    is_narrow: bool,
    loader: &MessagesLoader,
    inflight_messages: &mut HashSet<ConversationLoadKey>,
) {
    let animate_nav = nav_transitions_enabled(ctx);
    open_conversation_with_prefetch(&mut ctx.remote, ctx.accounts, cache, id);
    request_conversation_messages(
        cache,
        ctx.accounts.selected_account_pubkey(),
        id,
        loader,
        inflight_messages,
    );

    if is_narrow {
        route_to(router, Route::Conversation, animate_nav);
    }
}

/// Schedule a background load for a conversation if it is not already in flight.
fn request_conversation_messages(
    cache: &ConversationCache,
    me: &Pubkey,
    conversation_id: ConversationId,
    loader: &MessagesLoader,
    inflight_messages: &mut HashSet<ConversationLoadKey>,
) {
    let load_key = ConversationLoadKey::new(*me, conversation_id);
    if inflight_messages.contains(&load_key) {
        return;
    }

    let Some(conversation) = cache.get(conversation_id) else {
        return;
    };

    inflight_messages.insert(load_key);
    loader.load_conversation_messages(
        conversation_id,
        conversation.metadata.participants.clone(),
        *me,
    );
}
