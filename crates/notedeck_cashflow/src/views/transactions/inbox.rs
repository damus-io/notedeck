//! The triage inbox: transfer suggestions to accept or dismiss, then the
//! unknown transactions grouped by payee, biggest total first.
//!
//! A group's "Categorize all…" picks an account and writes a rule matching
//! the group's payee pattern; a row's "This one…" writes an override for
//! just that transaction. Accepting a suggestion writes a transfer link,
//! dismissing it a cleared one. Each write is queued on the outbox and
//! whatever it takes is hidden at once; the hides go when the next triage
//! arrives, which is the re-derive with the write in it, and come back if
//! the write fails.
//!
//! Each write also floats the Bookkeeping XP it earns ("+35 Bookkeeping
//! XP") up from the progress line, worked out on the rates the Adventure
//! folds with ([`cashflow::adventure::triage_reward`]). It is the one
//! place the game shows on the dashboard, and the vault's
//! `adventure.feedback_in_dashboard` setting turns it off.

use std::collections::HashSet;

use cashflow::adventure::{SkillId, XpTuning, triage_reward};
use cashflow::triage::{Action, Progress};
use egui::RichText;
use ledgerdb::TxnId;

use super::TransactionsState;
use super::model::{self, GroupView, SuggestionView};
use crate::ui::account_picker::{self, AccountPicker, Choices};
use crate::ui::hud::{FloatingXp, Grouped};
use crate::ui::palette::Palette;
use crate::views::LedgerCx;

/// How many groups the inbox lists at once: the biggest, which is where a
/// rule moves the most.
pub const GROUPS_SHOWN: usize = 40;

/// What the inbox keeps between frames.
#[derive(Default)]
pub(crate) struct InboxState {
    /// The books' generation the text below was written for.
    generation: Option<u64>,
    /// The groups, as drawn.
    groups: Vec<GroupView>,
    /// The transfer suggestions, as drawn.
    suggestions: Vec<SuggestionView>,
    /// The triage's own figures.
    progress: Progress,
    /// "N unknown left (−M today)", less what is hidden.
    progress_text: String,
    /// What is hidden waiting for its write to land.
    hidden: Hidden,
    /// Groups opened to list their rows, by index.
    expanded: HashSet<usize>,
    /// What the account picker is choosing for, when it is open.
    picking: Option<Target>,
    /// The account picker.
    picker: AccountPicker,
    /// The last write that failed, and why.
    error: Option<String>,
    /// The XP the writes earned, floating up from the progress line.
    floating: FloatingXp,
}

/// What hides until the next triage: queued writes' groups, rows and
/// suggestions.
#[derive(Default)]
struct Hidden {
    groups: HashSet<usize>,
    rows: HashSet<TxnId>,
    suggestions: HashSet<usize>,
    /// How many unknown transactions the hidden groups and rows hold.
    unknown: usize,
}

/// What a pick categorizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// Every row of the group at this index: a rule.
    Group(usize),
    /// One transaction of the group at this index: an override.
    Row {
        /// The group it is in.
        group: usize,
        /// The transaction.
        txn: TxnId,
    },
}

impl InboxState {
    /// Rewrite what is drawn when new books, and so a new triage, arrive.
    #[profiling::function]
    pub(crate) fn refresh(&mut self, ledger: LedgerCx<'_>) {
        if self.generation == Some(ledger.generation) {
            return;
        }
        self.generation = Some(ledger.generation);
        self.groups = model::group_views(ledger.books, ledger.triage);
        self.suggestions = model::suggestion_views(ledger.books, ledger.triage);
        self.progress = ledger.triage.progress;
        self.hidden = Hidden::default();
        self.picking = None;
        self.expanded.retain(|&i| i < self.groups.len());
        self.write_progress();
    }

    /// "N unknown left (−M today)", with what is hidden counted as done.
    fn write_progress(&mut self) {
        let hidden = self.hidden.unknown;
        let left = self.progress.left.saturating_sub(hidden);
        let today = self.progress.today + hidden;
        self.progress_text = if today == 0 {
            format!("{left} unknown left")
        } else {
            format!("{left} unknown left (−{today} today)")
        };
    }

    /// The progress line.
    pub(crate) fn progress_text(&self) -> &str {
        &self.progress_text
    }

    /// The XP texts floating, oldest first.
    pub(crate) fn floating(&self) -> impl Iterator<Item = &str> {
        self.floating.texts()
    }

    /// Queue `action` on `outbox`, floating the XP it earns when `xp`
    /// gives the rates: `rows` is how many transactions it categorizes.
    fn queue(
        &mut self,
        outbox: &mut Vec<Action>,
        action: Action,
        rows: usize,
        xp: Option<&XpTuning>,
    ) {
        if let Some(tuning) = xp {
            let rows = u32::try_from(rows).unwrap_or(u32::MAX);
            let earned = triage_reward(&action, rows, tuning);
            if earned > 0 {
                self.floating.push(format!(
                    "+{} {} XP",
                    Grouped(earned),
                    SkillId::Bookkeeping.name()
                ));
            }
        }
        outbox.push(action);
    }

    /// A queued write failed: show what it hid again, and why.
    pub(crate) fn write_failed(&mut self, err: String) {
        self.hidden = Hidden::default();
        self.error = Some(err);
        self.write_progress();
    }

    /// Hide group `index`, `rows` transactions, until the next triage.
    fn hide_group(&mut self, index: usize, rows: usize) {
        if self.hidden.groups.insert(index) {
            self.hidden.unknown += rows;
            self.write_progress();
        }
    }

    /// Hide transaction `txn` until the next triage.
    fn hide_row(&mut self, txn: TxnId) {
        if self.hidden.rows.insert(txn) {
            self.hidden.unknown += 1;
            self.write_progress();
        }
    }
}

/// A click this frame.
enum Click {
    /// Open the picker for this.
    Pick(Target),
    /// Open or close the group at this index.
    Toggle(usize),
    /// Accept the suggestion at this index.
    Accept(usize),
    /// Dismiss it.
    Dismiss(usize),
}

/// Draw the inbox, queueing on `outbox` the writes it asks for, and
/// floating the XP each earns at `xp`'s rates when it is given.
#[profiling::function]
pub(crate) fn ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    outbox: &mut Vec<Action>,
    xp: Option<&XpTuning>,
    state: &mut TransactionsState,
) {
    let TransactionsState {
        inbox, accounts, ..
    } = state;
    let mut click = None;
    let mut picked = None;
    egui::ScrollArea::vertical()
        .id_salt("cashflow_inbox")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let progress = ui.label(
                RichText::new(&inbox.progress_text)
                    .size(20.0)
                    .color(palette.text),
            );
            let from = progress.rect.right_bottom() + egui::vec2(12.0, 0.0);
            inbox.floating.show(ui, palette, from);
            if let Some(error) = &inbox.error {
                ui.label(RichText::new(error).color(palette.warning));
            }
            ui.add_space(8.0);

            let visible = |s: &&SuggestionView| !inbox.hidden.suggestions.contains(&s.index);
            if inbox.suggestions.iter().filter(visible).next().is_some() {
                section(ui, palette, "Transfers between your accounts");
                for suggestion in inbox.suggestions.iter().filter(visible) {
                    if let Some(c) = suggestion_ui(ui, palette, suggestion) {
                        click = Some(c);
                    }
                }
                ui.add_space(12.0);
            }

            if inbox.groups.is_empty() {
                ui.label(RichText::new("Nothing left to categorize.").color(palette.text_muted));
                return;
            }
            section(ui, palette, "Unknown, by payee");
            let shown = inbox
                .groups
                .iter()
                .filter(|g| !inbox.hidden.groups.contains(&g.index))
                .take(GROUPS_SHOWN);
            for group in shown {
                let open = inbox.expanded.contains(&group.index);
                let draw = GroupDraw {
                    group,
                    open,
                    hidden: &inbox.hidden.rows,
                    picking: inbox.picking,
                };
                let choices = Choices {
                    accounts: &accounts.names,
                    recent: &accounts.recent,
                    allow_new: true,
                };
                let out = group_ui(ui, palette, draw, &mut inbox.picker, choices);
                if out.click.is_some() {
                    click = out.click;
                }
                if out.picked.is_some() {
                    picked = out.picked;
                }
                if out.cancelled {
                    inbox.picking = None;
                }
            }
            let left = inbox.groups.len().saturating_sub(GROUPS_SHOWN);
            if left > 0 {
                ui.label(
                    RichText::new("Categorize these to see the rest.")
                        .small()
                        .color(palette.text_muted),
                );
            }
        });

    if let Some(account) = picked
        && let Some(target) = inbox.picking.take()
    {
        account_picker::remember(&mut accounts.recent, &account);
        inbox.error = None;
        categorize(inbox, outbox, xp, target, account);
    }
    match click {
        Some(Click::Pick(target)) => {
            inbox.picking = Some(target);
            inbox.picker.open();
        }
        Some(Click::Toggle(index)) => {
            if !inbox.expanded.remove(&index) {
                inbox.expanded.insert(index);
            }
        }
        Some(Click::Accept(index)) => answer(inbox, outbox, xp, index, true),
        Some(Click::Dismiss(index)) => answer(inbox, outbox, xp, index, false),
        None => {}
    }
}

/// Queue the write that accepts (or dismisses) suggestion `index`, and
/// hide it.
fn answer(
    inbox: &mut InboxState,
    outbox: &mut Vec<Action>,
    xp: Option<&XpTuning>,
    index: usize,
    accept: bool,
) {
    let Some(s) = inbox.suggestions.get(index) else {
        return;
    };
    let a = s.a.clone();
    let b = s.b.clone();
    let action = if accept {
        Action::Link { a, b }
    } else {
        Action::Dismiss { a, b }
    };
    inbox.queue(outbox, action, 2, xp);
    inbox.error = None;
    inbox.hidden.suggestions.insert(index);
}

/// Queue the write that sends `target` to `account`, and hide it.
fn categorize(
    inbox: &mut InboxState,
    outbox: &mut Vec<Action>,
    xp: Option<&XpTuning>,
    target: Target,
    account: String,
) {
    match target {
        Target::Group(index) => {
            let Some(group) = inbox.groups.get(index) else {
                return;
            };
            let Some(rule) = &group.rule else {
                return;
            };
            let action = Action::Rule {
                pattern: rule.pattern.clone(),
                source: rule.source.clone(),
                account,
            };
            let rows = group.rows.len() + group.more;
            inbox.queue(outbox, action, rows, xp);
            inbox.hide_group(index, rows);
        }
        Target::Row { group, txn } => {
            let row = inbox
                .groups
                .get(group)
                .and_then(|g| g.rows.iter().find(|r| r.txn == txn));
            let Some(record) = row.and_then(|r| r.record.clone()) else {
                return;
            };
            inbox.queue(outbox, Action::Override { record, account }, 1, xp);
            inbox.hide_row(txn);
        }
    }
}

/// A section's heading.
fn section(ui: &mut egui::Ui, palette: &Palette, title: &'static str) {
    ui.label(RichText::new(title).strong().color(palette.text));
    ui.add_space(4.0);
}

/// A card: the frame every suggestion and group is drawn in.
fn card(palette: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(palette.surface)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(8.0)
        .inner_margin(10.0)
}

/// One transfer suggestion, with its accept and dismiss buttons.
fn suggestion_ui(ui: &mut egui::Ui, palette: &Palette, s: &SuggestionView) -> Option<Click> {
    let mut click = None;
    card(palette).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(&s.out_text).size(13.0).color(palette.text));
        ui.label(RichText::new(&s.in_text).size(13.0).color(palette.text));
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(&s.days_text)
                    .small()
                    .color(palette.text_muted),
            );
            if ui.button("Link as transfer").clicked() {
                click = Some(Click::Accept(s.index));
            }
            if ui.button("Not a transfer").clicked() {
                click = Some(Click::Dismiss(s.index));
            }
        });
    });
    ui.add_space(6.0);
    click
}

/// What [`group_ui`] draws one group with.
struct GroupDraw<'a> {
    group: &'a GroupView,
    /// Whether its rows are listed.
    open: bool,
    /// Rows hidden waiting for an override.
    hidden: &'a HashSet<TxnId>,
    /// What the picker is open for, if anything.
    picking: Option<Target>,
}

/// What a frame of [`group_ui`] did.
#[derive(Default)]
struct GroupOut {
    click: Option<Click>,
    picked: Option<String>,
    cancelled: bool,
}

/// One payee group: its name, size and the rule it would take, its rows
/// when open, and the account picker under whichever of them is picking.
fn group_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    draw: GroupDraw<'_>,
    picker: &mut AccountPicker,
    choices: Choices<'_>,
) -> GroupOut {
    let group = draw.group;
    let mut out = GroupOut::default();
    let mut pick = |ui: &mut egui::Ui, out: &mut GroupOut| {
        let picked = account_picker::show(ui, palette, picker, choices);
        out.picked = picked.account;
        out.cancelled = picked.cancelled;
    };
    card(palette).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let arrow = if draw.open { "▾" } else { "▸" };
            let arrow = ui.add(
                egui::Button::new(RichText::new(arrow).color(palette.text_muted)).frame(false),
            );
            let title = RichText::new(&group.title)
                .size(15.0)
                .strong()
                .color(palette.text);
            let title = ui.add(egui::Button::new(title).frame(false));
            if arrow.clicked() || title.clicked() {
                out.click = Some(Click::Toggle(group.index));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if group.rule.is_some() && ui.button("Categorize all…").clicked() {
                    out.click = Some(Click::Pick(Target::Group(group.index)));
                }
            });
        });
        ui.label(
            RichText::new(&group.detail)
                .small()
                .color(palette.text_muted),
        );
        if let Some(rule) = &group.rule {
            ui.label(RichText::new(&rule.text).small().color(palette.text_muted));
        }
        if draw.picking == Some(Target::Group(group.index)) {
            pick(ui, &mut out);
        }
        if !draw.open {
            return;
        }
        ui.add_space(4.0);
        for row in group.rows.iter().filter(|r| !draw.hidden.contains(&r.txn)) {
            let target = Target::Row {
                group: group.index,
                txn: row.txn,
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(&row.text).size(13.0).color(palette.text));
                if row.record.is_some() && ui.small_button("This one…").clicked() {
                    out.click = Some(Click::Pick(target));
                }
            });
            if draw.picking == Some(target) {
                pick(ui, &mut out);
            }
        }
        if let Some(more) = &group.more_text {
            ui.label(RichText::new(more).small().color(palette.text_muted));
        }
    });
    ui.add_space(6.0);
    out
}
