use egui_nav::{NavAction, NavResponse};
use enostr::Pubkey;
use nostrdb::Transaction;
use notedeck::{AppAction, AppContext, ReplacementType, Router};

use crate::{
    cache::{
        ConversationCache, ConversationId, ConversationIdentifierUnowned, ParticipantSetUnowned,
    },
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

pub fn process_messages_ui_response(
    resp: MessagesUiResponse,
    ctx: &mut AppContext,
    cache: &mut ConversationCache,
    router: &mut Router<Route>,
    is_narrow: bool,
) -> Option<AppAction> {
    let mut action = None;
    if let Some(convo_resp) = resp.conversation_panel_response {
        action = handle_messages_action(convo_resp, ctx, cache, router, is_narrow);
    }

    let Some(nav) = resp.nav_response else {
        return action;
    };

    action.or(process_nav_resp(nav, ctx, cache, router, is_narrow))
}

fn process_nav_resp(
    nav: NavResponse<Option<MessagesAction>>,
    ctx: &mut AppContext,
    cache: &mut ConversationCache,
    router: &mut Router<Route>,
    is_narrow: bool,
) -> Option<AppAction> {
    let mut app_action = None;
    if let Some(action) = nav.response.or(nav.title_response) {
        app_action = handle_messages_action(action, ctx, cache, router, is_narrow);
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

fn handle_messages_action(
    action: MessagesAction,
    ctx: &mut AppContext<'_>,
    cache: &mut ConversationCache,
    router: &mut Router<Route>,
    is_narrow: bool,
) -> Option<AppAction> {
    let mut app_action = None;
    match action {
        MessagesAction::SendMessage {
            conversation_id,
            content,
        } => send_conversation_message(conversation_id, content, cache, ctx),
        MessagesAction::Open(conversation_id) => {
            open_coversation_action(conversation_id, ctx, cache, router, is_narrow);
        }
        MessagesAction::Create { recipient } => {
            let selected = ctx.accounts.selected_account_pubkey();
            let participants = vec![recipient.bytes(), selected.bytes()];
            let id = cache
                .registry
                .get_or_insert(ConversationIdentifierUnowned::Nip17(
                    ParticipantSetUnowned::new(participants),
                ));

            cache.initialize_conversation(id, vec![recipient, *selected]);

            let txn = Transaction::new(ctx.ndb).expect("txn");
            cache.open_conversation(
                ctx.ndb,
                &txn,
                id,
                ctx.note_cache,
                ctx.unknown_ids,
                ctx.accounts.selected_account_pubkey(),
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

fn open_coversation_action(
    id: ConversationId,
    ctx: &mut AppContext<'_>,
    cache: &mut ConversationCache,
    router: &mut Router<Route>,
    is_narrow: bool,
) {
    let txn = Transaction::new(ctx.ndb).expect("txn");
    cache.open_conversation(
        ctx.ndb,
        &txn,
        id,
        ctx.note_cache,
        ctx.unknown_ids,
        ctx.accounts.selected_account_pubkey(),
    );

    if is_narrow {
        router.route_to(Route::Conversation);
    }
}
