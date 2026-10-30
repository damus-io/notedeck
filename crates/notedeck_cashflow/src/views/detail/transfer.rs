//! The detail's Transfer section: the other side of the transfer this
//! transaction is, or could be, one side of.
//!
//! It is in one of four states ([`TransferText`]):
//!
//! - **Suggested**: [`cashflow::transfers::suggest`] pairs it with a record
//!   on another of the owner's accounts. The section shows that leg (its
//!   account, amount, payee and date, and how many days apart it is), opens
//!   its detail, and links the two or dismisses the pair, with the same
//!   [`Action::Link`] and [`Action::Dismiss`] the triage inbox writes. The
//!   link is planned ([`crate::plan_job`]) as soon as the suggestion shows,
//!   so "Link as transfer" is enabled once the plan comes back clean.
//! - **Linked**: a live link names it. The section opens the other side,
//!   and plans an [`Action::Unlink`] before its Apply.
//! - **Categorized**: an override or split decides it, and a categorized
//!   record can't be linked, so Link is disabled and "Uncategorize first"
//!   opens the Categorize section's uncategorize.
//! - **Unmatched**: one muted line.
//!
//! Every text that shows while nothing is pending is laid out with the rest
//! of the detail's ([`TransferGalleys`]), so a frame that holds still lays
//! out and allocates nothing of its own.

use std::sync::Arc;

use cashflow::transfers::WINDOW_DAYS;
use cashflow::triage::Action;
use egui::{Galley, RichText};

use super::model::{OtherLeg, TransferText};
use super::{Layouter, chip, heading, text};
use crate::plan_job::{PlanJob, PlanKey, PlanResult};
use crate::ui::palette::Palette;

/// Room kept at the right of the other leg's payee for the day gap.
const GAP_WIDTH: f32 = 100.0;

/// What the section keeps between frames, for the record open; a new
/// record starts it afresh.
#[derive(Default)]
pub(crate) struct TransferState {
    /// The link or unlink planned or being planned, waiting for its Apply.
    pending: Option<Pending>,
    /// The books' generation a write was queued on: "Saving…" until newer
    /// books arrive.
    saving: Option<u64>,
    /// Why the last write failed.
    error: Option<String>,
}

/// A write waiting for its Apply, and whether its plan allows it.
struct Pending {
    action: Action,
    /// Whether the plan of the books in hand has landed.
    landed: bool,
    /// Why the plan refuses it; `None` once it lands clean.
    refused: Option<String>,
}

impl Pending {
    fn new(action: Action) -> Self {
        Pending {
            action,
            landed: false,
            refused: None,
        }
    }

    /// Whether it can be applied: its plan landed, and came back clean.
    fn ready(&self) -> bool {
        self.landed && self.refused.is_none()
    }

    /// Whether it still fits what the section shows: a link while the pair
    /// is suggested, an unlink while the record is linked.
    fn fits(&self, transfer: &TransferText) -> bool {
        match (&self.action, transfer) {
            (Action::Link { a, b }, TransferText::Suggested(leg)) => *a == leg.a && *b == leg.b,
            (Action::Unlink { b, .. }, TransferText::Linked { other }) => b == other,
            _ => false,
        }
    }
}

/// The section's texts, laid out with the rest of the detail's.
pub(super) struct TransferGalleys {
    heading: Arc<Galley>,
    /// Under a plan being made.
    checking: Arc<Galley>,
    /// Beside a plan that came back clean.
    applies: Arc<Galley>,
    shown: Shown,
}

/// The galleys of the state the section is in.
enum Shown {
    /// No section.
    None,
    Unmatched(Arc<Galley>),
    Suggested(SuggestedGalleys),
    Linked {
        /// "Linked with bank:4", which opens it.
        linked: Arc<Galley>,
        unlink: Arc<Galley>,
        /// What the unlink does, once its plan lands clean.
        preview: Arc<Galley>,
    },
    Categorized {
        /// Why it can't be linked.
        why: Arc<Galley>,
        link: Arc<Galley>,
        uncategorize: Arc<Galley>,
    },
}

/// A suggested pair's galleys.
struct SuggestedGalleys {
    chip: Arc<Galley>,
    account: Arc<Galley>,
    amount: Arc<Galley>,
    payee: Arc<Galley>,
    gap: Arc<Galley>,
    open: Arc<Galley>,
    preview: Arc<Galley>,
    link: Arc<Galley>,
    dismiss: Arc<Galley>,
}

impl TransferGalleys {
    /// The section's galleys for `transfer`, wrapped at `width`.
    pub(super) fn lay_out(l: &Layouter<'_>, transfer: &TransferText, width: f32) -> Self {
        let shown = match transfer {
            TransferText::NotApplicable => Shown::None,
            TransferText::Unmatched => Shown::Unmatched(l.wrapped(
                &format!("No matching leg in your other accounts within {WINDOW_DAYS} days"),
                13.0,
                width,
            )),
            TransferText::Suggested(leg) => {
                Shown::Suggested(SuggestedGalleys::lay_out(l, leg, width))
            }
            TransferText::Linked { other } => Shown::Linked {
                linked: l.line(&format!("Linked with {other} →"), 14.0),
                unlink: l.line("Unlink", 14.0),
                preview: l.wrapped(
                    "Unlinking puts both legs back on their counter accounts.",
                    13.0,
                    width,
                ),
            },
            TransferText::Categorized => Shown::Categorized {
                why: l.wrapped(
                    "An override or split decides it, and a categorized \
                     transaction can't be linked as a transfer.",
                    13.0,
                    width,
                ),
                link: l.line("Link as transfer", 14.0),
                uncategorize: l.line("Uncategorize first", 14.0),
            },
        };
        TransferGalleys {
            heading: l.bold("Transfer", 15.0),
            checking: l.line("Checking it applies…", 13.0),
            applies: l.line("applies", 12.0),
            shown,
        }
    }
}

impl SuggestedGalleys {
    /// `leg`'s galleys, wrapped at `width`.
    fn lay_out(l: &Layouter<'_>, leg: &OtherLeg, width: f32) -> Self {
        SuggestedGalleys {
            chip: l.line("suggested", 12.0),
            account: l.line(&leg.account, 14.0),
            amount: l.line(&leg.amount, 14.0),
            // The gap sits at its right.
            payee: l.wrapped(&leg.payee, 13.0, width - GAP_WIDTH),
            gap: l.line(&leg.gap, 13.0),
            open: l.line(&format!("open {} →", leg.record), 13.0),
            preview: l.wrapped(
                "Linking nets both legs: no income, no expense.",
                13.0,
                width,
            ),
            link: l.line("Link as transfer", 14.0),
            dismiss: l.line("Not a transfer", 14.0),
        }
    }
}

/// Something the section was asked to do this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// Open the other side's detail.
    OpenOther,
    /// Queue the planned link or unlink.
    Apply,
    /// Dismiss the suggested pair.
    Dismiss,
    /// Plan the unlink of the linked pair.
    Unlink,
    /// Drop the unlink waiting for its Apply.
    Cancel,
    /// Open the Categorize section's uncategorize.
    Uncategorize,
}

impl TransferState {
    /// Before a frame of the books at `generation` draws: stop saying
    /// "Saving…" once newer books arrived, drop a write that no longer fits
    /// `transfer`, plan the link of a suggested pair, and take the plan if
    /// it landed. A frame that holds still asks for nothing.
    pub(crate) fn prepare(
        &mut self,
        plans: &mut PlanJob,
        generation: u64,
        transfer: &TransferText,
    ) {
        if self.saving.is_some_and(|g| g != generation) {
            self.saving = None;
        }
        if self.pending.as_ref().is_some_and(|p| !p.fits(transfer)) {
            self.pending = None;
        }
        if self.pending.is_none()
            && self.saving.is_none()
            && let TransferText::Suggested(leg) = transfer
        {
            self.plan(
                plans,
                Action::Link {
                    a: leg.a.clone(),
                    b: leg.b.clone(),
                },
            );
        }
        let Some(pending) = self.pending.as_mut() else {
            plans.cancel(PlanKey::Transfer);
            return;
        };
        match plans.result(PlanKey::Transfer) {
            Some(plan) if !pending.landed => land(pending, plan),
            Some(_) => {}
            // Being made again, for newer books: not Apply-able meanwhile.
            None => pending.landed = false,
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
        plans.request(PlanKey::Transfer, action.clone());
        self.pending = Some(Pending::new(action));
    }

    /// Queue `action`, and say "Saving…" until the books come back with it.
    fn queue(&mut self, outbox: &mut Vec<Action>, action: Action, generation: u64) {
        outbox.push(action);
        self.pending = None;
        self.error = None;
        self.saving = Some(generation);
    }
}

/// Take what `plan` says of `pending`.
fn land(pending: &mut Pending, plan: &PlanResult) {
    pending.landed = true;
    pending.refused = plan.as_ref().err().map(ToString::to_string);
}

/// What [`act`] writes with.
pub(crate) struct Cx<'a> {
    /// Where the plans are made.
    pub(crate) plans: &'a mut PlanJob,
    /// Where an applied write is queued.
    pub(crate) outbox: &'a mut Vec<Action>,
    /// The books' generation.
    pub(crate) generation: u64,
}

/// Do what `event` asked of the section, which shows `transfer` for
/// `record`. Opening the other side and uncategorizing are the detail's to
/// do.
pub(crate) fn act(
    state: &mut TransferState,
    event: Event,
    record: &str,
    transfer: &TransferText,
    cx: Cx<'_>,
) {
    match event {
        Event::OpenOther | Event::Uncategorize => {}
        Event::Apply => {
            let Some(pending) = state.pending.take_if(|p| p.ready()) else {
                return;
            };
            state.queue(cx.outbox, pending.action, cx.generation);
        }
        Event::Dismiss => {
            let TransferText::Suggested(leg) = transfer else {
                return;
            };
            let action = Action::Dismiss {
                a: leg.a.clone(),
                b: leg.b.clone(),
            };
            state.queue(cx.outbox, action, cx.generation);
        }
        Event::Unlink => {
            let TransferText::Linked { other } = transfer else {
                return;
            };
            state.error = None;
            let action = Action::Unlink {
                a: record.to_owned(),
                b: other.clone(),
            };
            state.plan(cx.plans, action);
        }
        Event::Cancel => state.pending = None,
    }
}

/// Whether the section has anything to draw.
pub(super) fn shows(g: &TransferGalleys) -> bool {
    !matches!(g.shown, Shown::None)
}

/// The Transfer section.
pub(super) fn section_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    g: &TransferGalleys,
    state: &TransferState,
) -> Option<Event> {
    if !shows(g) {
        return None;
    }
    let mut event = None;
    let mut on = |e: Option<Event>| {
        if e.is_some() {
            event = e;
        }
    };
    let ready = state.pending.as_ref().is_some_and(Pending::ready);
    if let Shown::Suggested(s) = &g.shown {
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            text(ui, &g.heading, palette.text);
            ui.add_space(6.0);
            chip(ui, &s.chip, palette.accent, egui::Sense::hover());
        });
        ui.add_space(6.0);
    } else {
        heading(ui, palette, &g.heading);
    }
    match &g.shown {
        Shown::None => {}
        Shown::Unmatched(line) => {
            text(ui, line, palette.text_muted);
        }
        Shown::Suggested(s) => on(suggested_ui(ui, palette, g, s, state, ready)),
        Shown::Linked {
            linked,
            unlink,
            preview,
        } => {
            if ui
                .add(egui::Button::new(linked.clone()).frame(false))
                .on_hover_text("Open the other side")
                .clicked()
            {
                on(Some(Event::OpenOther));
            }
            let unlinking = state.pending.is_some();
            let button = egui::Button::new(unlink.clone()).selected(unlinking);
            if ui.add(button).clicked() {
                on(Some(if unlinking {
                    Event::Cancel
                } else {
                    Event::Unlink
                }));
            }
            if unlinking {
                on(unlink_ui(ui, palette, g, state, preview, ready));
            }
        }
        Shown::Categorized {
            why,
            link,
            uncategorize,
        } => {
            text(ui, why, palette.text_muted);
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled(false, egui::Button::new(link.clone()));
                if ui.add(egui::Button::new(uncategorize.clone())).clicked() {
                    on(Some(Event::Uncategorize));
                }
            });
        }
    }
    if state.saving.is_some() {
        ui.label(RichText::new("Saving…").color(palette.text_muted));
    }
    if let Some(error) = &state.error {
        ui.label(RichText::new(error.as_str()).color(palette.warning));
    }
    event
}

/// A suggested pair: the other leg, its open link, the link's plan, and
/// Link and Not a transfer.
fn suggested_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    g: &TransferGalleys,
    s: &SuggestedGalleys,
    state: &TransferState,
    ready: bool,
) -> Option<Event> {
    let mut event = None;
    ui.horizontal(|ui| {
        text(ui, &s.account, palette.text);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            text(ui, &s.amount, palette.text);
        });
    });
    ui.horizontal(|ui| {
        text(ui, &s.payee, palette.text_muted);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            text(ui, &s.gap, palette.text_muted);
        });
    });
    if ui
        .add(egui::Button::new(s.open.clone()).frame(false))
        .on_hover_text("Open the other side")
        .clicked()
    {
        event = Some(Event::OpenOther);
    }
    if state.saving.is_none() {
        ui.add_space(4.0);
        plan_line(ui, palette, g, state, &s.preview);
    }
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(ready, egui::Button::new(s.link.clone()))
            .clicked()
        {
            event = Some(Event::Apply);
        }
        if ui
            .add_enabled(state.saving.is_none(), egui::Button::new(s.dismiss.clone()))
            .clicked()
        {
            event = Some(Event::Dismiss);
        }
    });
    event
}

/// The unlink's plan, and Apply unlink.
fn unlink_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    g: &TransferGalleys,
    state: &TransferState,
    preview: &Arc<Galley>,
    ready: bool,
) -> Option<Event> {
    ui.add_space(4.0);
    plan_line(ui, palette, g, state, preview);
    let mut event = None;
    ui.horizontal(|ui| {
        if ui
            .add_enabled(ready, egui::Button::new("Apply unlink"))
            .clicked()
        {
            event = Some(Event::Apply);
        }
        if ui.button("Cancel").clicked() {
            event = Some(Event::Cancel);
        }
    });
    event
}

/// Where the pending write's plan is: `preview`, what it does, once it
/// lands clean; why it can't be made; or that it is being checked.
fn plan_line(
    ui: &mut egui::Ui,
    palette: &Palette,
    g: &TransferGalleys,
    state: &TransferState,
    preview: &Arc<Galley>,
) {
    let Some(pending) = &state.pending else {
        return;
    };
    if !pending.landed {
        text(ui, &g.checking, palette.text_muted);
        return;
    }
    if let Some(refused) = &pending.refused {
        ui.label(
            RichText::new(refused.as_str())
                .size(13.0)
                .color(palette.warning),
        );
        return;
    }
    text(ui, preview, palette.text);
    text(ui, &g.applies, palette.accent);
}
