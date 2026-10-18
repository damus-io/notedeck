//! The Categories view: where money goes and where it comes from, ranked by
//! account down the chart of accounts.
//!
//! Two columns, Expenses and Income, side by side (stacked on a phone). Each
//! ranks the accounts one level below its current parent over the shared
//! [`Period`], with an "Other" row for the tail so the shares sum to 100%.
//! Clicking an account drills into it; a breadcrumb walks back up. A leaf
//! account lists its payees instead.
//!
//! Each column's [`Breakdown`] is computed by [`model::breakdown`] only when
//! the books, the period or the column's level change, and kept with its
//! text already formatted, so a frame draws it without querying the ledger
//! or allocating its own strings (CLAUDE.md rule 18).

pub mod model;

use egui::{Color32, FontId, RichText, Sense, Stroke, StrokeKind, text::LayoutJob};
use ledgerdb::Date;

use super::{LedgerCx, ViewCx};
use crate::{Period, View, ui::palette::Palette};
use model::{Breakdown, Flow, Level, Row, RowKind};

/// How many accounts (or payees) a column ranks before the rest go to
/// "Other".
const LIMIT: usize = 8;

/// Height of one bar row: a line of text over a thin track.
const ROW_HEIGHT: f32 = 40.0;

/// Height of a row's track.
const TRACK_HEIGHT: f32 = 6.0;

/// Gap between the two columns in the wide layout.
const COLUMN_GAP: f32 = 32.0;

/// The Categories view's state: each column's level and cached breakdown.
pub(crate) struct CategoriesState {
    /// Expenses, then income.
    columns: [ColumnState; 2],
    /// What the cached breakdowns were computed from; a frame with a
    /// different key recomputes them.
    key: Option<Key>,
}

impl Default for CategoriesState {
    fn default() -> Self {
        Self {
            columns: Flow::ALL.map(ColumnState::new),
            key: None,
        }
    }
}

/// What every column's breakdown depends on besides its own level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    generation: u64,
    period: Period,
    today: Date,
}

/// One column: which level it shows, and that level's breakdown.
struct ColumnState {
    /// Expenses or income.
    flow: Flow,
    /// The full name of the account drilled into; `None` is the column's
    /// root. Kept by name, not id, so a reload of the books keeps the level.
    parent: Option<String>,
    /// The path from the root to the parent, for the breadcrumb.
    crumbs: Vec<Crumb>,
    /// The level's rows, or `None` when the books have no root account for
    /// this column.
    breakdown: Option<Breakdown>,
    /// A note under the bars about postings left out, formatted once.
    note: Option<String>,
    /// Whether the level changed since [`Self::breakdown`] was computed.
    stale: bool,
}

/// One step of a column's breadcrumb.
struct Crumb {
    /// The account's full name, which clicking the crumb makes the parent.
    name: String,
    /// What the crumb shows: the column's heading at the root, an account's
    /// last segment below it.
    label: String,
}

/// A click this frame that changes a column's level.
enum Nav {
    /// Drill into this account.
    Into(String),
    /// Go back up to the crumb at this index.
    Up(usize),
}

impl ColumnState {
    fn new(flow: Flow) -> Self {
        Self {
            flow,
            parent: None,
            crumbs: Vec::new(),
            breakdown: None,
            note: None,
            stale: true,
        }
    }

    /// Recompute the breakdown and breadcrumb from `ledger` over `period`.
    #[profiling::function]
    fn refresh(&mut self, ledger: LedgerCx<'_>, period: Period, today: Date) {
        self.stale = false;
        self.crumbs.clear();
        self.breakdown = None;
        self.note = None;

        let books = ledger.books;
        let Some(home) = books.home() else { return };
        let journal = books.ledger().journal();
        let accounts = &journal.accounts;
        let Some(root) = accounts.lookup(self.flow.root_name()) else {
            return;
        };
        // A level the reloaded books no longer have falls back to the root.
        let parent = self
            .parent
            .as_deref()
            .and_then(|name| accounts.lookup(name))
            .filter(|&p| p == root || accounts.is_ancestor(root, p))
            .unwrap_or(root);

        let path = std::iter::once(parent).chain(accounts.ancestors(parent));
        for account in path.take_while(|&a| a == root || accounts.is_ancestor(root, a)) {
            let label = if account == root {
                self.flow.label().to_owned()
            } else {
                accounts.leaf_name(account).to_owned()
            };
            self.crumbs.push(Crumb {
                name: accounts.full_name(account).to_owned(),
                label,
            });
        }
        self.crumbs.reverse();

        let breakdown = model::breakdown(Level {
            books,
            home,
            parent,
            flow: self.flow,
            period: period.resolve(today),
            limit: LIMIT,
        });
        if breakdown.unconverted > 0 {
            self.note = Some(format!(
                "{} postings had no rate into {} and are left out.",
                breakdown.unconverted,
                journal.commodities.name(home)
            ));
        }
        self.breakdown = Some(breakdown);
    }

    /// Apply a click from this frame.
    fn navigate(&mut self, nav: Nav) {
        let parent = match nav {
            Nav::Into(name) => Some(name),
            Nav::Up(0) => None,
            Nav::Up(at) => self.crumbs.get(at).map(|c| c.name.clone()),
        };
        if parent != self.parent {
            self.parent = parent;
            self.stale = true;
        }
    }
}

/// Draw the Categories view.
#[profiling::function]
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut egui::Ui) {
    let palette = cx.palette;
    let narrow = notedeck::ui::is_narrow(ui.ctx());
    let heading = RichText::new(View::Categories.label())
        .heading()
        .color(palette.text);
    if narrow {
        // The chips are wider than a phone: they scroll on a row of their
        // own rather than widening the page.
        ui.label(heading);
        ui.add_space(4.0);
        egui::ScrollArea::horizontal()
            .id_salt("cashflow_period_picker")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                crate::ui::period_picker::show(ui, palette, cx.period)
            });
    } else {
        ui.horizontal(|ui| {
            ui.label(heading);
            ui.add_space(16.0);
            crate::ui::period_picker::show(ui, palette, cx.period);
        });
    }
    ui.add_space(12.0);

    let Some(ledger) = cx.ledger else {
        muted(ui, palette, "No journal loaded yet.");
        return;
    };
    if ledger.books.home().is_none() {
        muted(
            ui,
            palette,
            "The journal has no postings in the home commodity, so nothing can be totalled.",
        );
        return;
    }

    let period = *cx.period;
    let state = &mut cx.states.categories;
    let key = Key {
        generation: ledger.generation,
        period,
        today: cx.today,
    };
    let refresh_all = state.key != Some(key);
    state.key = Some(key);
    for column in &mut state.columns {
        if refresh_all || column.stale {
            column.refresh(ledger, period, cx.today);
        }
    }

    egui::ScrollArea::vertical()
        .id_salt("cashflow_categories")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if narrow {
                for column in &mut state.columns {
                    column_ui(ui, palette, column, period);
                    ui.add_space(24.0);
                }
                return;
            }
            ui.spacing_mut().item_spacing.x = COLUMN_GAP;
            let [spending, earning] = &mut state.columns;
            ui.columns_const(|[left, right]| {
                column_ui(left, palette, spending, period);
                column_ui(right, palette, earning, period);
            });
        });
}

/// One column: its heading and total, its breadcrumb, and its bars. Applies
/// a drill or a crumb click at the end of the frame it happened in.
fn column_ui(ui: &mut egui::Ui, palette: &Palette, column: &mut ColumnState, period: Period) {
    let flow = column.flow;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(flow.label())
                .size(18.0)
                .strong()
                .color(palette.text),
        );
        if let Some(breakdown) = &column.breakdown {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(&breakdown.total_text)
                        .size(18.0)
                        .color(flow_color(palette, flow)),
                );
            });
        }
    });

    let mut nav = breadcrumb(ui, palette, &column.crumbs);
    ui.add_space(6.0);

    let Some(breakdown) = &column.breakdown else {
        muted(ui, palette, "The journal has no accounts here.");
        return;
    };
    if breakdown.rows.is_empty() {
        muted(ui, palette, "Nothing in this period.");
    }

    let max = breakdown
        .rows
        .iter()
        .map(|r| r.amount.unsigned_abs())
        .max()
        .unwrap_or(0);
    for row in &breakdown.rows {
        let resp = bar_row(ui, palette, flow, row, max);
        let resp = resp.on_hover_ui(|ui| tooltip(ui, palette, row, period));
        if let RowKind::Account { .. } = row.kind
            && resp.clicked()
        {
            nav = Some(Nav::Into(row.detail.clone()));
        }
    }

    if let Some(note) = &column.note {
        ui.add_space(6.0);
        ui.label(RichText::new(note).small().color(palette.warning));
    }

    if let Some(nav) = nav {
        column.navigate(nav);
    }
}

/// The path from the column's root to its level, each step a link back up.
/// Draws nothing at the root.
fn breadcrumb(ui: &mut egui::Ui, palette: &Palette, crumbs: &[Crumb]) -> Option<Nav> {
    if crumbs.len() < 2 {
        return None;
    }
    let mut nav = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let last = crumbs.len() - 1;
        for (at, crumb) in crumbs.iter().enumerate() {
            if at == last {
                ui.label(RichText::new(&crumb.label).color(palette.text));
                break;
            }
            let link =
                egui::Button::new(RichText::new(&crumb.label).color(palette.accent)).frame(false);
            if ui.add(link).clicked() {
                nav = Some(Nav::Up(at));
            }
            ui.label(RichText::new("›").color(palette.text_muted));
        }
    });
    nav
}

/// One bar: the label and its amount and share on a line, over a track
/// filled in proportion to the column's largest row. Clickable when the row
/// is an account to drill into.
fn bar_row(
    ui: &mut egui::Ui,
    palette: &Palette,
    flow: Flow,
    row: &Row,
    max: u128,
) -> egui::Response {
    let drills = matches!(row.kind, RowKind::Account { .. });
    let sense = if drills {
        Sense::click()
    } else {
        Sense::hover()
    };
    let size = egui::vec2(ui.available_width(), ROW_HEIGHT);
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &row.label));
    if !ui.is_rect_visible(rect) {
        return resp;
    }

    if resp.hovered() {
        // The highlight reaches past the row, so the bars line up with the
        // column's heading.
        let highlight = rect.expand2(egui::vec2(6.0, 2.0));
        ui.painter().rect_filled(highlight, 6.0, palette.surface);
    }
    let painter = ui.painter_at(rect);
    let inner = rect.shrink2(egui::vec2(0.0, 4.0));
    let font = FontId::proportional(14.0);

    let figures = painter.layout_no_wrap(
        row.share_text.clone(),
        FontId::proportional(13.0),
        palette.text_muted,
    );
    let share_pos = inner.right_top();
    painter.galley(
        share_pos - egui::vec2(figures.size().x, 0.0),
        figures.clone(),
        palette.text_muted,
    );
    let amount = painter.layout_no_wrap(row.amount_text.clone(), font.clone(), palette.text);
    let amount_right = share_pos.x - figures.size().x - 10.0;
    painter.galley(
        egui::pos2(amount_right - amount.size().x, inner.top()),
        amount.clone(),
        palette.text,
    );

    // The label takes what the figures leave, cut short with an ellipsis.
    let mut job = LayoutJob::simple_singleline(row.label.clone(), font, palette.text);
    job.wrap = egui::text::TextWrapping::truncate_at_width(
        (amount_right - amount.size().x - 12.0 - inner.left()).max(24.0),
    );
    let label = painter.layout_job(job);
    painter.galley(inner.left_top(), label, palette.text);

    let track = egui::Rect::from_min_size(
        egui::pos2(inner.left(), inner.bottom() - TRACK_HEIGHT),
        egui::vec2(inner.width(), TRACK_HEIGHT),
    );
    let radius = TRACK_HEIGHT / 2.0;
    painter.rect_filled(track, radius, palette.border);
    let fraction = if max == 0 {
        0.0
    } else {
        (row.amount.unsigned_abs() as f64 / max as f64) as f32
    };
    if fraction > 0.0 {
        let mut fill = track;
        fill.set_width((track.width() * fraction).max(TRACK_HEIGHT));
        painter.rect_filled(fill, radius, row_color(palette, flow, row));
    }
    if drills && resp.hovered() {
        painter.rect_stroke(
            track,
            radius,
            Stroke::new(1.0, palette.text_muted),
            StrokeKind::Outside,
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// What hovering a bar shows: the row's name, amount, share and change.
fn tooltip(ui: &mut egui::Ui, palette: &Palette, row: &Row, period: Period) {
    ui.label(RichText::new(&row.label).strong().color(palette.text));
    ui.label(RichText::new(&row.detail).small().color(palette.text_muted));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(&row.amount_text).color(palette.text));
        ui.label(RichText::new(&row.share_text).color(palette.text_muted));
    });
    if let (Some(delta), Some(versus)) = (&row.delta_text, period.previous_label()) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(delta).color(palette.text));
            ui.label(RichText::new(versus).color(palette.text_muted));
        });
    }
    if let RowKind::Account { leaf, .. } = row.kind {
        let hint = if leaf {
            "Click for its payees"
        } else {
            "Click to drill in"
        };
        ui.label(RichText::new(hint).small().color(palette.text_muted));
    }
}

/// A column's color: money out or money in.
fn flow_color(palette: &Palette, flow: Flow) -> Color32 {
    match flow {
        Flow::Spending => palette.expense,
        Flow::Earning => palette.income,
    }
}

/// A bar's fill: the column's color for accounts and payees, dimmer for the
/// parent's own postings, muted for "Other".
fn row_color(palette: &Palette, flow: Flow, row: &Row) -> Color32 {
    match row.kind {
        RowKind::Account { .. } | RowKind::Payee => flow_color(palette, flow),
        RowKind::Direct => flow_color(palette, flow).gamma_multiply(0.6),
        RowKind::Other => palette.text_muted,
    }
}

/// A line of muted text: an empty state or a hint.
fn muted(ui: &mut egui::Ui, palette: &Palette, text: &'static str) {
    ui.label(RichText::new(text).color(palette.text_muted));
}
