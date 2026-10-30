//! The detail's Categorize section: send this one transaction somewhere (an
//! override), split it, write a rule for every transaction like it, or
//! clear the override or split deciding it. A transaction linked as a
//! transfer can't be categorized, so for one the section offers its unlink
//! instead.
//!
//! Every write is planned before it is made, the way the inbox plans its
//! rules ([`crate::plan_job`]): picking an account, or legs that add up,
//! asks for a [`cashflow::plan::plan`] of the [`Action`], and the section
//! says what it would do ("This one: expenses:unknown → expenses:travel",
//! "Would move 3 transactions to …") or why it can't be written, with the
//! CLI's own words. Apply is enabled only by a plan that came back clean,
//! and queues the action on the outbox, which writes it through
//! [`cashflow::triage::apply`]. "Saving…" shows until the books come back
//! derived with it, and a write that fails says why here.
//!
//! A split's legs are checked with [`cashflow::split::legs`] as they are
//! typed, the same rules the CLI's `split` reads its `account=amount`s
//! with, against the raw ledger's counter leg.
//!
//! Closed, it draws its galleys ([`CategorizeGalleys`]), laid out with the
//! rest of the detail's, and allocates nothing a frame; the editors it
//! opens are egui widgets over the strings they edit.

use std::sync::Arc;

use cashflow::Books;
use cashflow::plan::{Categorized, Plan};
use cashflow::split::{self, LegDraft};
use cashflow::triage::Action;
use cashflow::unknown;
use egui::{Galley, RichText};
use ledgerdb::{Edits, Ledger, TxnId};

use super::model::{DetailText, Tier};
use super::{Layouter, heading, text};
use crate::plan_job::{PlanJob, PlanKey, PlanResult};
use crate::ui::account_picker::{self, AccountPicker, Choices};
use crate::ui::palette::Palette;
use crate::views::transactions::Accounts;

/// How wide a split leg's amount box is.
const AMOUNT_BOX: f32 = 80.0;

/// What the section has open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Open {
    /// Nothing: its buttons alone.
    #[default]
    Closed,
    /// An override's account picker.
    Recategorize,
    /// The split editor.
    Split,
    /// A rule for every transaction like this one.
    Rule,
    /// Clearing its override or split.
    Uncategorize,
    /// Clearing its transfer link.
    Unlink,
}

impl Open {
    /// Its Apply button's text.
    fn apply_label(self) -> &'static str {
        match self {
            Open::Closed => "Apply",
            Open::Recategorize => "Apply override",
            Open::Split => "Apply split",
            Open::Rule => "Add rule",
            Open::Uncategorize => "Apply uncategorize",
            Open::Unlink => "Apply unlink",
        }
    }
}

/// What the account picker is choosing for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PickFor {
    /// The override's account.
    Override,
    /// The split leg at this index.
    Leg(usize),
    /// The rule's account.
    Rule,
}

/// What the section keeps between frames, for the record open; a new
/// record starts it afresh.
#[derive(Default)]
pub(crate) struct CategorizeState {
    open: Open,
    /// What the picker is open for, if it is.
    picking: Option<PickFor>,
    picker: AccountPicker,
    split: Vec<LegRow>,
    rule: RuleDraft,
    /// The write planned or being planned, waiting for its Apply.
    pending: Option<Pending>,
    /// Why what is open can't be planned as it stands: legs that don't add
    /// up, an empty pattern.
    problem: Option<String>,
    /// The books' generation an applied write was queued on: "Saving…"
    /// until newer books arrive.
    saving: Option<u64>,
    /// Why the last write failed.
    error: Option<String>,
}

/// One leg of the split editor, as typed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LegRow {
    /// Its account; empty until one is picked.
    account: String,
    /// Its amount; blank takes what the other legs leave.
    amount: String,
}

/// The rule "all like this" would write, as edited.
#[derive(Default)]
struct RuleDraft {
    /// What the payee contains.
    pattern: String,
    /// This record's source.
    source: String,
    /// "source: card": whether the rule is limited to [`Self::source`].
    limit: bool,
    /// Where it sends them, once picked.
    account: Option<String>,
}

/// A write waiting for its Apply, and what its plan says.
struct Pending {
    action: Action,
    /// What the plan says it does, or why it can't be written.
    text: String,
    /// Whether [`Self::text`] is the plan of the books in hand: a plan
    /// made again for newer books un-lands it.
    landed: bool,
    /// Whether the plan came back clean.
    ok: bool,
}

impl Pending {
    fn new(action: Action) -> Self {
        Pending {
            action,
            text: String::new(),
            landed: false,
            ok: false,
        }
    }

    /// Write what `plan` says of the write on `record`.
    fn land(&mut self, plan: &PlanResult, record: &str) {
        self.landed = true;
        self.ok = plan.is_ok();
        self.text = match plan {
            Ok(plan) => describe(&self.action, plan, record),
            Err(err) => err.to_string(),
        };
    }
}

/// What `plan` says `action`, a write on `record`, would do.
fn describe(action: &Action, plan: &Plan, record: &str) -> String {
    if let Action::Rule { account, .. } = action {
        let n = plan.moves.len();
        let rows = if n == 1 {
            "transaction"
        } else {
            "transactions"
        };
        let mut text = format!("Would move {n} {rows} to {account}.");
        if plan.unchanged > 0 {
            text.push_str(&format!(" {} already decided stay put.", plan.unchanged));
        }
        return text;
    }
    let mine = plan
        .moves
        .iter()
        .find(|m| m.record.as_deref() == Some(record));
    let mut text = match mine {
        Some(m) => format!(
            "This one: {} → {}",
            legs_text(&m.before),
            legs_text(&m.after)
        ),
        None => "It changes nothing here".to_owned(),
    };
    if plan.moves.len() > 1 {
        text.push_str(", and its other side too");
    }
    text
}

/// Where `c`'s legs go: one account, or a split's joined.
fn legs_text(c: &Categorized) -> String {
    match c.legs.as_slice() {
        [] => "as imported".to_owned(),
        [one] => one.account.clone(),
        legs => {
            let accounts: Vec<&str> = legs.iter().map(|l| l.account.as_str()).collect();
            accounts.join(" + ")
        }
    }
}

/// The section's texts that show while it is closed, laid out with the
/// rest of the detail's.
pub(super) struct CategorizeGalleys {
    heading: Arc<Galley>,
    recategorize: Arc<Galley>,
    split: Arc<Galley>,
    rule: Arc<Galley>,
    /// Shown when an override or split decides it.
    uncategorize: Option<Arc<Galley>>,
    /// "Linked as a transfer with bank:4", for a linked record.
    linked: Option<Arc<Galley>>,
    unlink: Arc<Galley>,
}

impl CategorizeGalleys {
    /// The section's galleys for `text`, wrapped at `width`.
    pub(super) fn lay_out(l: &Layouter<'_>, text: &DetailText, width: f32) -> Self {
        let clears = matches!(text.tier, Tier::Override | Tier::Split);
        CategorizeGalleys {
            heading: l.bold("Categorize", 15.0),
            recategorize: l.line("Recategorize…", 14.0),
            split: l.line("Split…", 14.0),
            rule: l.line("All like this…", 14.0),
            uncategorize: clears.then(|| l.line("Uncategorize", 14.0)),
            linked: text
                .categorize
                .linked
                .as_deref()
                .map(|other| l.wrapped(&format!("Linked as a transfer with {other}"), 13.0, width)),
            unlink: l.line("Unlink", 14.0),
        }
    }
}

/// Something the section was asked to do this frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// Open this, or close it when it is open.
    Open(Open),
    /// Close whatever is open.
    Cancel,
    /// The picker chose an account.
    Picked(String),
    /// The picker was dismissed.
    PickerCancelled,
    /// Open the picker for this.
    Pick(PickFor),
    /// Add a split leg.
    AddLeg,
    /// Remove the split leg at this index.
    RemoveLeg(usize),
    /// A split leg's amount was edited.
    LegEdited,
    /// The rule's pattern was edited; `done` when the box was left, which
    /// plans it.
    PatternEdited {
        /// Whether the box lost focus.
        done: bool,
    },
    /// Limit the rule to this record's source, or stop limiting it.
    ToggleSource,
    /// Queue the planned write.
    Apply,
}

/// What [`act`] writes with.
pub(crate) struct Cx<'a> {
    /// The books on screen.
    pub(crate) books: &'a Books,
    /// The raw ledger they were derived from, which a split is checked
    /// against.
    pub(crate) raw: Option<&'a Ledger>,
    /// The edits they were derived with.
    pub(crate) edits: &'a Edits,
    /// Their generation.
    pub(crate) generation: u64,
    /// Where the plans are made.
    pub(crate) plans: &'a mut PlanJob,
    /// Where an applied write is queued.
    pub(crate) outbox: &'a mut Vec<Action>,
    /// The picker's accounts, whose recent picks a pick joins.
    pub(crate) accounts: &'a mut Accounts,
}

impl CategorizeState {
    /// Before a frame of the books at `generation` draws: stop saying
    /// "Saving…" once newer books arrived, take the plan if it landed, and
    /// drop the plan when nothing waits for one.
    pub(crate) fn prepare(&mut self, plans: &mut PlanJob, generation: u64, record: &str) {
        if self.saving.is_some_and(|g| g != generation) {
            self.saving = None;
        }
        let Some(pending) = self.pending.as_mut() else {
            plans.cancel(PlanKey::Detail);
            return;
        };
        match plans.result(PlanKey::Detail) {
            Some(plan) if !pending.landed => pending.land(plan, record),
            Some(_) => {}
            // Being made again, for newer books: not Apply-able meanwhile.
            None => {
                pending.landed = false;
                pending.ok = false;
            }
        }
    }

    /// A write this section queued failed with `err`.
    pub(crate) fn write_failed(&mut self, err: &str) {
        if self.saving.take().is_some() {
            self.error = Some(err.to_owned());
        }
    }

    /// Ask for a plan of `action`, which Apply waits on.
    fn plan(&mut self, plans: &mut PlanJob, action: Action) {
        self.problem = None;
        plans.request(PlanKey::Detail, action.clone());
        self.pending = Some(Pending::new(action));
    }

    /// Close whatever is open.
    fn close(&mut self) {
        self.open = Open::Closed;
        self.picking = None;
        self.pending = None;
        self.problem = None;
    }
}

/// Do what `event` asked of the section for the record `text` describes.
pub(crate) fn act(state: &mut CategorizeState, event: Event, text: &DetailText, mut cx: Cx<'_>) {
    let record = text.record.as_str();
    match event {
        Event::Open(open) if open == state.open => state.close(),
        Event::Open(open) => {
            state.close();
            state.error = None;
            state.open = open;
            match open {
                Open::Closed => {}
                Open::Recategorize => pick(state, PickFor::Override),
                Open::Split => {
                    state.split = if text.categorize.legs.is_empty() {
                        vec![LegRow::default(), LegRow::default()]
                    } else {
                        text.categorize
                            .legs
                            .iter()
                            .map(|l| LegRow {
                                account: l.account.clone(),
                                amount: l.amount.clone().unwrap_or_default(),
                            })
                            .collect()
                    };
                    plan_split(state, record, &mut cx);
                    if state.pending.is_none() {
                        // A fresh split asks for its first account.
                        state.problem = None;
                        pick(state, PickFor::Leg(0));
                    }
                }
                Open::Rule => {
                    state.rule = RuleDraft {
                        pattern: like_this(cx.books, &text.payee),
                        source: text.categorize.source.clone(),
                        limit: true,
                        account: None,
                    };
                    pick(state, PickFor::Rule);
                }
                Open::Uncategorize => state.plan(
                    cx.plans,
                    Action::Uncategorize {
                        record: record.to_owned(),
                    },
                ),
                Open::Unlink => {
                    let Some(other) = &text.categorize.linked else {
                        state.close();
                        return;
                    };
                    state.plan(
                        cx.plans,
                        Action::Unlink {
                            a: record.to_owned(),
                            b: other.clone(),
                        },
                    );
                }
            }
        }
        Event::Cancel => state.close(),
        Event::Pick(what) => pick(state, what),
        Event::PickerCancelled => {
            state.picking = None;
            if state.open == Open::Recategorize && state.pending.is_none() {
                state.close();
            }
        }
        Event::Picked(account) => {
            cx.accounts.remember(&account);
            match state.picking.take() {
                Some(PickFor::Override) => {
                    state.plan(
                        cx.plans,
                        Action::Override {
                            record: record.to_owned(),
                            account,
                        },
                    );
                    // It stays open over the plan, so another pick
                    // replans.
                    state.picking = Some(PickFor::Override);
                }
                Some(PickFor::Leg(at)) => {
                    if let Some(leg) = state.split.get_mut(at) {
                        leg.account = account;
                    }
                    plan_split(state, record, &mut cx);
                    // On to the next leg still without an account.
                    if let Some(next) = state.split.iter().position(|l| l.account.is_empty()) {
                        state.problem = None;
                        pick(state, PickFor::Leg(next));
                    }
                }
                Some(PickFor::Rule) => {
                    state.rule.account = Some(account);
                    plan_rule(state, cx.plans);
                }
                None => {}
            }
        }
        Event::AddLeg => {
            state.split.push(LegRow::default());
            plan_split(state, record, &mut cx);
        }
        Event::RemoveLeg(at) => {
            if at < state.split.len() {
                state.split.remove(at);
            }
            state.picking = None;
            plan_split(state, record, &mut cx);
        }
        Event::LegEdited => plan_split(state, record, &mut cx),
        Event::PatternEdited { done } => {
            // A pattern being typed plans once the box is left, so a word
            // typed doesn't queue a derive a key.
            state.pending = None;
            if done {
                plan_rule(state, cx.plans);
            }
        }
        Event::ToggleSource => {
            state.rule.limit = !state.rule.limit;
            plan_rule(state, cx.plans);
        }
        Event::Apply => {
            let Some(pending) = state.pending.take() else {
                return;
            };
            if !(pending.landed && pending.ok) {
                state.pending = Some(pending);
                return;
            }
            cx.outbox.push(pending.action);
            state.close();
            state.saving = Some(cx.generation);
        }
    }
}

/// Open the picker for `what`.
fn pick(state: &mut CategorizeState, what: PickFor) {
    state.picking = Some(what);
    state.picker.open();
}

/// Check the split editor's legs against the record's counter leg, and
/// plan the split they make, or say why they don't make one.
fn plan_split(state: &mut CategorizeState, record: &str, cx: &mut Cx<'_>) {
    match split_legs(&state.split, record, cx) {
        Ok(legs) => {
            let action = Action::Split {
                record: record.to_owned(),
                legs,
            };
            state.plan(cx.plans, action);
        }
        Err(problem) => {
            state.pending = None;
            state.problem = Some(problem);
        }
    }
}

/// The legs `rows` make of `record`'s counter leg, or what is wrong with
/// them, worded for the editor.
fn split_legs(rows: &[LegRow], record: &str, cx: &Cx<'_>) -> Result<Vec<ledgerdb::Leg>, String> {
    if rows.iter().any(|r| r.account.trim().is_empty()) {
        return Err("Pick an account for each leg".to_owned());
    }
    if rows.len() < 2 {
        return Err("A split needs two legs or more".to_owned());
    }
    let raw = cx
        .raw
        .ok_or_else(|| "The journal is still loading".to_owned())?;
    let txn: TxnId = raw
        .find(record)
        .ok_or_else(|| format!("{record} isn't in the loaded journal"))?;
    let drafts: Vec<LegDraft> = rows
        .iter()
        .map(|r| {
            let amount = r.amount.trim();
            LegDraft {
                account: r.account.trim().to_owned(),
                amount: (!amount.is_empty()).then(|| amount.to_owned()),
            }
        })
        .collect();
    split::legs(raw, cx.edits, txn, record, &drafts).map_err(|e| e.to_string())
}

/// Plan the rule as drafted, once it has a pattern and an account.
fn plan_rule(state: &mut CategorizeState, plans: &mut PlanJob) {
    state.pending = None;
    let pattern = state.rule.pattern.trim();
    if pattern.is_empty() {
        state.problem = Some("Say what the payee contains".to_owned());
        return;
    }
    state.problem = None;
    let Some(account) = state.rule.account.clone() else {
        return;
    };
    let action = Action::Rule {
        pattern: pattern.to_owned(),
        source: state.rule.limit.then(|| state.rule.source.clone()),
        account,
    };
    state.plan(plans, action);
}

/// What an "all like this" rule matches `payee` by: the substring every
/// transaction with the same normalized payee shares
/// ([`unknown::common_pattern`]), else the normalized payee itself. Walks
/// the journal once, when the rule is opened.
fn like_this(books: &Books, payee: &str) -> String {
    let key = unknown::normalize(payee);
    let ledger = books.ledger();
    let journal = ledger.journal();
    let alike: Vec<&str> = (0..ledger.txns().len())
        .map(|i| journal.strings.get(journal.txn(TxnId(i as u32)).payee))
        .filter(|p| unknown::normalize(p) == key)
        .collect();
    unknown::common_pattern(alike.iter().copied()).unwrap_or(key)
}

/// What [`section_ui`] draws with.
pub(super) struct Section<'a> {
    pub(super) state: &'a mut CategorizeState,
    pub(super) choices: Choices<'a>,
}

/// The Categorize section: its heading and buttons, what is open under
/// them, and the plan of the write it would make.
pub(super) fn section_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    g: &CategorizeGalleys,
    section: Section<'_>,
) -> Option<Event> {
    let Section { state, choices } = section;
    heading(ui, palette, &g.heading);
    let mut event = None;
    let mut on = |e: Option<Event>| {
        if e.is_some() {
            event = e;
        }
    };
    if let Some(linked) = &g.linked {
        text(ui, linked, palette.text_muted);
        let unlink = egui::Button::new(g.unlink.clone()).selected(state.open == Open::Unlink);
        if ui.add(unlink).clicked() {
            on(Some(Event::Open(Open::Unlink)));
        }
    } else {
        ui.horizontal_wrapped(|ui| {
            let buttons = [
                (Some(&g.recategorize), Open::Recategorize),
                (Some(&g.split), Open::Split),
                (Some(&g.rule), Open::Rule),
                (g.uncategorize.as_ref(), Open::Uncategorize),
            ];
            for (galley, open) in buttons {
                let Some(galley) = galley else {
                    continue;
                };
                let button = egui::Button::new(galley.clone()).selected(state.open == open);
                if ui.add(button).clicked() {
                    on(Some(Event::Open(open)));
                }
            }
        });
    }
    match state.open {
        Open::Closed => {}
        Open::Recategorize => {
            if state.picking == Some(PickFor::Override) {
                on(picker_ui(ui, palette, state, choices));
            }
        }
        Open::Split => on(split_ui(ui, palette, state, choices)),
        Open::Rule => on(rule_ui(ui, palette, state, choices)),
        Open::Uncategorize | Open::Unlink => {}
    }
    if state.open != Open::Closed {
        on(plan_ui(ui, palette, state));
    }
    if state.saving.is_some() {
        ui.label(RichText::new("Saving…").color(palette.text_muted));
    }
    if let Some(error) = &state.error {
        ui.label(RichText::new(error.as_str()).color(palette.warning));
    }
    event
}

/// The account picker, open for [`CategorizeState::picking`].
fn picker_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    state: &mut CategorizeState,
    choices: Choices<'_>,
) -> Option<Event> {
    let picked = account_picker::show(ui, palette, &mut state.picker, choices);
    if let Some(account) = picked.account {
        return Some(Event::Picked(account));
    }
    picked.cancelled.then_some(Event::PickerCancelled)
}

/// The split editor: a row per leg, its account and amount, and the
/// picker under the leg picking.
fn split_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    state: &mut CategorizeState,
    choices: Choices<'_>,
) -> Option<Event> {
    let mut event = None;
    ui.label(
        RichText::new("Leave one amount blank to take the rest")
            .small()
            .color(palette.text_muted),
    );
    let removable = state.split.len() > 2;
    for at in 0..state.split.len() {
        ui.horizontal(|ui| {
            let leg = &mut state.split[at];
            let account = if leg.account.is_empty() {
                "Pick an account…"
            } else {
                leg.account.as_str()
            };
            let width = ui.available_width() - AMOUNT_BOX - 36.0;
            let button = egui::Button::new(account).truncate();
            if ui.add_sized([width.max(60.0), 22.0], button).clicked() {
                event = Some(Event::Pick(PickFor::Leg(at)));
            }
            let amount = ui.add(
                egui::TextEdit::singleline(&mut leg.amount)
                    .hint_text("rest")
                    .desired_width(AMOUNT_BOX),
            );
            if amount.changed() {
                event = Some(Event::LegEdited);
            }
            if removable && ui.small_button("✕").clicked() {
                event = Some(Event::RemoveLeg(at));
            }
        });
        if state.picking == Some(PickFor::Leg(at))
            && let Some(e) = picker_ui(ui, palette, state, choices)
        {
            event = Some(e);
        }
    }
    if ui.small_button("+ Add leg").clicked() {
        event = Some(Event::AddLeg);
    }
    event
}

/// The rule editor: what the payee contains, the source chip, where they
/// go.
fn rule_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    state: &mut CategorizeState,
    choices: Choices<'_>,
) -> Option<Event> {
    let mut event = None;
    ui.label(
        RichText::new("A rule, placed last: anything already decided stays put")
            .small()
            .color(palette.text_muted),
    );
    let label = ui.label(RichText::new("Payee contains").color(palette.text));
    let pattern = ui
        .add(egui::TextEdit::singleline(&mut state.rule.pattern).desired_width(f32::INFINITY))
        .labelled_by(label.id);
    if pattern.changed() || pattern.lost_focus() {
        event = Some(Event::PatternEdited {
            done: pattern.lost_focus(),
        });
    }
    ui.horizontal_wrapped(|ui| {
        let source = if state.rule.limit {
            RichText::new(format!("source: {}", state.rule.source)).color(palette.text)
        } else {
            RichText::new("any source").color(palette.text_muted)
        };
        if ui
            .add(egui::Button::selectable(state.rule.limit, source))
            .on_hover_text("Limit the rule to this transaction's source")
            .clicked()
        {
            event = Some(Event::ToggleSource);
        }
        let account = state
            .rule
            .account
            .as_deref()
            .unwrap_or("Pick where they go…");
        if ui.button(account).clicked() {
            event = Some(Event::Pick(PickFor::Rule));
        }
    });
    if state.picking == Some(PickFor::Rule)
        && let Some(e) = picker_ui(ui, palette, state, choices)
    {
        event = Some(e);
    }
    event
}

/// What the write would do, or why it can't be made, and Apply and Cancel.
fn plan_ui(ui: &mut egui::Ui, palette: &Palette, state: &CategorizeState) -> Option<Event> {
    let mut event = None;
    ui.add_space(4.0);
    match (&state.pending, &state.problem) {
        (_, Some(problem)) => {
            ui.label(
                RichText::new(problem.as_str())
                    .size(13.0)
                    .color(palette.warning),
            );
        }
        (Some(pending), None) if !pending.landed => {
            ui.label(
                RichText::new("Checking what it does…")
                    .size(13.0)
                    .color(palette.text_muted),
            );
        }
        (Some(pending), None) => {
            let color = if pending.ok {
                palette.text
            } else {
                palette.warning
            };
            ui.label(RichText::new(pending.text.as_str()).size(13.0).color(color));
            if pending.ok {
                ui.label(RichText::new("applies").small().color(palette.accent));
            }
        }
        (None, None) => {}
    }
    let ok = state.pending.as_ref().is_some_and(|p| p.landed && p.ok);
    ui.horizontal(|ui| {
        let apply = egui::Button::new(state.open.apply_label());
        if ui.add_enabled(ok, apply).clicked() {
            event = Some(Event::Apply);
        }
        if ui.button("Cancel").clicked() {
            event = Some(Event::Cancel);
        }
    });
    event
}
