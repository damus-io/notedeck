//! The detail's Similar section: the transactions with the same payee, the
//! ones of another payee with the same amount, and how the same-payee ones
//! were categorized.
//!
//! What it lists comes from the books' [`cashflow::similar::Index`], built
//! off the UI thread with the rest of a load, and is turned into text with
//! the rest of the detail's ([`super::model::SimilarText`]). Under the
//! heading:
//!
//! - **The tally**: "12 → 🛒 Groceries (rule quiet-otter), 2 unknown", and
//!   a "Use 🛒 Groceries →" that pre-fills the Categorize section with that
//!   account: its override, or the rule's account when "All like this" is
//!   open. It plans the write and never makes it; Apply does.
//! - **The rows**, newest first: a click opens that transaction's detail as
//!   another entry on the nav stack, so back returns here.
//! - **"Show all N in the register →"**: the register, filtered to what
//!   every transaction with the payee contains.
//!
//! It draws its galleys ([`SimilarGalleys`]), laid out with the rest of
//! the detail's, and allocates nothing a frame.

use std::sync::Arc;

use egui::{Galley, Sense};

use super::model::{SimilarText, Tier};
use super::{Layouter, chip, text, tier_color};
use crate::ui::palette::Palette;

/// Height of one row.
const ROW_HEIGHT: f32 = 24.0;

/// Width of a row's date column.
const DATE_WIDTH: f32 = 84.0;

/// From this width, a row shows its date.
const DATES_FROM: f32 = 420.0;

/// The share of a row the account and its chip may take.
const ACCOUNT_SHARE: f32 = 0.45;

/// The gap between a row's columns.
const GAP: f32 = 8.0;

/// A chip's padding, each side.
const CHIP_PAD: f32 = 8.0;

/// The section's texts, laid out with the rest of the detail's.
pub(super) struct SimilarGalleys {
    heading: Arc<Galley>,
    /// "same payee 14 · same amount 2".
    summary: Option<Arc<Galley>>,
    tally: Option<Arc<Galley>>,
    /// "Use 🛒 Groceries →".
    use_account: Option<Arc<Galley>>,
    /// "Same payee (N)", over the same payee's rows.
    payee_heading: Option<Arc<Galley>>,
    /// "Same amount, other payees (N)", over the same amount's rows.
    amount_heading: Option<Arc<Galley>>,
    rows: Vec<RowGalleys>,
    /// How many of [`Self::rows`] are the same payee's.
    payee_rows: usize,
    /// "Show all N in the register →".
    show_all: Option<Arc<Galley>>,
    /// "Nothing like it in the loaded journal", with no rows.
    empty: Option<Arc<Galley>>,
    /// Whether the rows show their date: there is room.
    dates: bool,
}

/// One row's galleys.
struct RowGalleys {
    date: Arc<Galley>,
    payee: Arc<Galley>,
    account: Arc<Galley>,
    tier: Arc<Galley>,
    kind: Tier,
    /// "2024-10-07 SQ *COFFEE SHOP #9": what accesskit calls it.
    label: String,
}

impl SimilarGalleys {
    /// The section's galleys for `similar`, its rows `width` wide.
    pub(super) fn lay_out(l: &Layouter<'_>, similar: &SimilarText, width: f32) -> Self {
        let dates = width >= DATES_FROM;
        let account_width = width * ACCOUNT_SHARE;
        let payee_width = width - account_width - GAP - if dates { DATE_WIDTH } else { 0.0 };
        let rows = similar
            .rows
            .iter()
            .map(|r| {
                let tier = l.line(r.tier.label(), 11.0);
                let account = account_width - tier.size().x - 2.0 * CHIP_PAD - GAP;
                RowGalleys {
                    date: l.line(&r.date, 13.0),
                    payee: l.truncated(&r.payee, 13.0, payee_width),
                    account: l.truncated(&r.account, 13.0, account),
                    tier,
                    kind: r.tier,
                    label: format!("{} {}", r.date, r.payee),
                }
            })
            .collect();
        let amount_rows = similar.rows.len() - similar.payee_rows;
        SimilarGalleys {
            heading: l.bold("Similar transactions", 15.0),
            summary: (!similar.summary.is_empty()).then(|| l.line(&similar.summary, 12.0)),
            tally: similar.tally.as_deref().map(|t| l.wrapped(t, 13.0, width)),
            use_account: similar.use_account.as_ref().map(|u| l.line(&u.label, 13.0)),
            payee_heading: (similar.payee_rows > 0)
                .then(|| l.line(&format!("Same payee ({})", similar.payee_total), 13.0)),
            amount_heading: (amount_rows > 0).then(|| {
                l.line(
                    &format!("Same amount, other payees ({})", similar.amount_total),
                    13.0,
                )
            }),
            rows,
            payee_rows: similar.payee_rows,
            show_all: (similar.payee_total > 0).then(|| {
                l.line(
                    &format!("Show all {} in the register →", similar.payee_total + 1),
                    13.0,
                )
            }),
            empty: similar
                .rows
                .is_empty()
                .then(|| l.line("Nothing like it in the loaded journal", 13.0)),
            dates,
        }
    }
}

/// Something the section was asked to do this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// Open the row at this index of [`SimilarText::rows`].
    Open(usize),
    /// Pre-fill the Categorize section with the tally's account.
    Use,
    /// Show every transaction with the payee in the register.
    ShowAll,
}

/// The Similar section: its heading, the tally and its quick action, the
/// two lists, and the way to the register.
pub(super) fn section_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    g: &SimilarGalleys,
) -> Option<Event> {
    let mut event = None;
    ui.add_space(18.0);
    ui.horizontal(|ui| {
        text(ui, &g.heading, palette.text);
        if let Some(summary) = &g.summary {
            ui.add_space(6.0);
            text(ui, summary, palette.text_muted);
        }
    });
    ui.add_space(6.0);
    if let Some(empty) = &g.empty {
        text(ui, empty, palette.text_muted);
        return None;
    }
    if let Some(tally) = &g.tally {
        text(ui, tally, palette.text_muted);
    }
    if let Some(use_account) = &g.use_account
        && chip(ui, use_account, palette.accent, Sense::click())
            .on_hover_text("Pre-fill Categorize with it; nothing is written until Apply")
            .clicked()
    {
        event = Some(Event::Use);
    }
    for (at, row) in g.rows.iter().enumerate() {
        // The same amount's list starts where the same payee's ends, at
        // the top when there are none.
        let list = if at == g.payee_rows {
            g.amount_heading.as_ref()
        } else if at == 0 {
            g.payee_heading.as_ref()
        } else {
            None
        };
        if let Some(list) = list {
            ui.add_space(8.0);
            text(ui, list, palette.text);
        }
        if row_ui(ui, palette, row, g.dates) {
            event = Some(Event::Open(at));
        }
    }
    if let Some(show_all) = &g.show_all {
        ui.add_space(6.0);
        if ui
            .add(egui::Button::new(show_all.clone()).frame(false))
            .clicked()
        {
            event = Some(Event::ShowAll);
        }
    }
    event
}

/// One row: its date (with room), payee, and account with its tier's chip
/// at the right. True when it is clicked.
fn row_ui(ui: &mut egui::Ui, palette: &Palette, row: &RowGalleys, dates: bool) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
    let painter = ui.painter_at(rect);
    if resp.hovered() {
        painter.rect_filled(rect, 4.0, palette.surface);
    }
    let middle = |g: &Galley| rect.center().y - g.size().y / 2.0;
    let mut x = rect.left();
    if dates {
        painter.galley(
            egui::pos2(x, middle(&row.date)),
            row.date.clone(),
            palette.text_muted,
        );
        x += DATE_WIDTH;
    }
    painter.galley(
        egui::pos2(x, middle(&row.payee)),
        row.payee.clone(),
        palette.text,
    );

    let color = tier_color(palette, row.kind);
    let chip = egui::Rect::from_min_max(
        egui::pos2(
            rect.right() - row.tier.size().x - 2.0 * CHIP_PAD,
            rect.center().y - row.tier.size().y / 2.0 - 2.0,
        ),
        egui::pos2(
            rect.right(),
            rect.center().y + row.tier.size().y / 2.0 + 2.0,
        ),
    );
    painter.rect_stroke(
        chip,
        chip.height() / 2.0,
        egui::Stroke::new(1.0, color),
        egui::StrokeKind::Inside,
    );
    painter.galley(
        egui::pos2(chip.left() + CHIP_PAD, middle(&row.tier)),
        row.tier.clone(),
        color,
    );
    let account = if row.kind == Tier::Unknown {
        palette.warning
    } else {
        palette.text
    };
    painter.galley(
        egui::pos2(
            chip.left() - GAP - row.account.size().x,
            middle(&row.account),
        ),
        row.account.clone(),
        account,
    );
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        egui::Stroke::new(0.5, palette.border),
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &row.label));
    resp.clicked()
}
