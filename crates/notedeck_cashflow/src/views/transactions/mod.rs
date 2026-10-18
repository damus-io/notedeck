//! The Transactions view: the register, and the triage inbox for what
//! nothing has categorized yet.
//!
//! - The **register** ([`register`]) lists every transaction over the shared
//!   [`crate::Period`], filtered by payee, account subtree, source and
//!   unknown-only, virtualized so the whole ledger scrolls. Hovering a
//!   row's account says what decided it.
//! - The **inbox** ([`inbox`]) burns the unknowns down: transfer
//!   suggestions to link or dismiss, then the unknown transactions grouped
//!   by payee, biggest first, each one rule ("categorize all") or one
//!   override ("this one") away from categorized.
//!
//! Both read the [`cashflow::triage::Triage`] that came with the books. The
//! inbox's writes go to [`super::ViewCx::outbox`]; until the re-derive lands
//! with them, what they took is hidden here so the inbox moves on at once.

pub mod inbox;
pub mod model;
pub mod register;

use egui::RichText;

use super::ViewCx;
use crate::View;

/// Which half of the view is showing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    /// Every transaction.
    #[default]
    Register,
    /// What is left to categorize.
    Inbox,
}

/// What the Transactions view keeps between frames.
#[derive(Default)]
pub(crate) struct TransactionsState {
    /// The half showing.
    pub(crate) tab: Tab,
    /// The register's filters, matches and rows on screen.
    pub(crate) register: register::RegisterState,
    /// The inbox's groups, suggestions and optimistic hides.
    pub(crate) inbox: inbox::InboxState,
    /// What both pickers choose from, kept per load.
    accounts: Accounts,
}

/// The accounts the pickers offer, refreshed when the books change.
#[derive(Default)]
pub(crate) struct Accounts {
    /// The books' generation they were listed from.
    generation: Option<u64>,
    /// Every account in the books, sorted.
    names: Vec<String>,
    /// Recent picks, newest first.
    recent: Vec<String>,
}

impl TransactionsState {
    /// A vault write failed: tell the inbox, which shows it.
    pub(crate) fn write_failed(&mut self, err: String) {
        self.inbox.write_failed(err);
    }
}

/// Draw the Transactions view.
#[profiling::function]
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    let palette = cx.palette;
    let narrow = notedeck::ui::is_narrow(ui.ctx());
    let state = &mut cx.states.transactions;
    if state.tab == Tab::Register {
        crate::ui::period_picker::heading(ui, palette, View::Transactions, cx.period, narrow);
    } else {
        ui.label(
            RichText::new(View::Transactions.label())
                .heading()
                .color(palette.text),
        );
    }
    ui.add_space(8.0);

    let Some(ledger) = cx.ledger else {
        ui.label(RichText::new("No journal loaded yet.").color(palette.text_muted));
        return;
    };
    if state.accounts.generation != Some(ledger.generation) {
        state.accounts.generation = Some(ledger.generation);
        state.accounts.names = model::account_names(ledger.books);
    }
    state.inbox.refresh(ledger);

    ui.horizontal(|ui| {
        for (tab, label) in [(Tab::Register, "Register"), (Tab::Inbox, "Inbox")] {
            let selected = state.tab == tab;
            let color = if selected {
                palette.text
            } else {
                palette.text_muted
            };
            let text = RichText::new(label).color(color);
            if ui.add(egui::Button::selectable(selected, text)).clicked() {
                state.tab = tab;
            }
        }
        // The inbox heads itself with it.
        if state.tab == Tab::Register {
            ui.label(RichText::new(state.inbox.progress_text()).color(palette.text_muted));
        }
    });
    ui.add_space(8.0);

    match state.tab {
        Tab::Register => {
            let range = cx.period.resolve(cx.today).range;
            register::ui(ui, palette, ledger, cx.labels, range, state);
        }
        Tab::Inbox => {
            let xp = cx.xp_feedback.then_some(cx.tuning);
            inbox::ui(ui, palette, cx.outbox, xp, state)
        }
    }
}
