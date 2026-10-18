//! The register: every transaction the filters match, newest first,
//! virtualized with `ScrollArea::show_rows` so a ledger of 300k scrolls as
//! fast as one of thirty.
//!
//! The matches are a list of ids worked out only when the books, the
//! period or a filter change ([`model::matching`]). A row's text is worked
//! out and laid out when it scrolls into view and kept while it stays
//! there ([`Window`]), so a frame that doesn't scroll formats and lays out
//! nothing: it paints the galleys it has.

use std::sync::Arc;

use cashflow::chart::Labels;
use egui::{Align2, FontId, Galley, RichText, Sense};
use ledgerdb::{DateRange, TxnId};

use super::model::{self, RegisterFilter, RowText};
use super::{Accounts, TransactionsState};
use crate::ui::account_picker::{self, AccountPicker, Choices};
use crate::ui::palette::Palette;
use crate::views::LedgerCx;

/// Height of one register row.
pub const ROW_HEIGHT: f32 = 26.0;

/// Width of the date column.
const DATE_WIDTH: f32 = 96.0;

/// Width of each amount column.
const AMOUNT_WIDTH: f32 = 120.0;

/// What the register keeps between frames.
#[derive(Default)]
pub(crate) struct RegisterState {
    /// The filters as the person has them now.
    pub(crate) filter: RegisterFilter,
    /// What [`Self::ids`] were matched with; a frame where it differs from
    /// the filters, the books or the period matches again.
    matched: Option<Matched>,
    /// The matching transactions, newest first.
    ids: Vec<TxnId>,
    /// "N transactions".
    count_text: String,
    /// The text of the rows on screen.
    window: Window,
    /// The segment sources the source filter offers, per load.
    sources: Vec<String>,
    /// The account filter's picker, when it is open.
    picker: Option<AccountPicker>,
}

/// What a match list was worked out from.
#[derive(Clone, PartialEq)]
struct Matched {
    generation: u64,
    range: DateRange,
    filter: RegisterFilter,
}

/// The rows on screen, laid out: rows `first..first + rows.len()` of the
/// match list, at `pixels_per_point`.
#[derive(Default)]
struct Window {
    first: usize,
    pixels_per_point: f32,
    rows: Vec<Row>,
}

/// One register row, laid out once in [`PLACEHOLDER`] so painting it picks
/// the color.
///
/// [`PLACEHOLDER`]: egui::Color32::PLACEHOLDER
struct Row {
    date: Arc<Galley>,
    payee: Arc<Galley>,
    account: Arc<Galley>,
    amount: Arc<Galley>,
    home: Arc<Galley>,
    /// The hover over the account: its full name and what decided it.
    why: String,
    unknown: bool,
    incoming: bool,
}

impl Row {
    /// `text` laid out with `painter`'s fonts.
    fn lay_out(painter: &egui::Painter, text: RowText) -> Self {
        let font = FontId::proportional(14.0);
        let galley =
            |text: String| painter.layout_no_wrap(text, font.clone(), egui::Color32::PLACEHOLDER);
        Row {
            date: galley(text.date),
            payee: galley(text.payee),
            account: galley(text.account),
            amount: galley(text.amount),
            home: galley(text.home),
            why: text.why,
            unknown: text.unknown,
            incoming: text.incoming,
        }
    }
}

impl RegisterState {
    /// Match again if the books, the period or the filters moved.
    #[profiling::function]
    fn refresh(&mut self, ledger: LedgerCx<'_>, range: DateRange) {
        let fresh = self.matched.as_ref().is_some_and(|m| {
            m.generation == ledger.generation && m.range == range && m.filter == self.filter
        });
        if fresh {
            return;
        }
        if self
            .matched
            .as_ref()
            .is_none_or(|m| m.generation != ledger.generation)
        {
            self.sources = model::sources(ledger.books);
        }
        self.ids = model::matching(ledger.books, range, &self.filter);
        let n = self.ids.len();
        self.count_text = format!("{n} transaction{}", if n == 1 { "" } else { "s" });
        self.window = Window::default();
        self.matched = Some(Matched {
            generation: ledger.generation,
            range,
            filter: self.filter.clone(),
        });
    }

    /// Have match rows `rows` laid out with `painter`'s fonts.
    fn show_window(
        &mut self,
        painter: &egui::Painter,
        ledger: LedgerCx<'_>,
        labels: &Labels,
        rows: std::ops::Range<usize>,
    ) {
        let w = &self.window;
        let ppp = painter.pixels_per_point();
        if w.first == rows.start && w.rows.len() == rows.len() && w.pixels_per_point == ppp {
            return;
        }
        self.window = Window {
            first: rows.start,
            pixels_per_point: ppp,
            rows: self.ids[rows]
                .iter()
                .map(|&txn| {
                    let text = model::row_text(ledger.books, ledger.triage, labels, txn);
                    Row::lay_out(painter, text)
                })
                .collect(),
        };
    }
}

/// Draw the register.
#[profiling::function]
pub(crate) fn ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    ledger: LedgerCx<'_>,
    labels: &Labels,
    range: DateRange,
    state: &mut TransactionsState,
) {
    let TransactionsState {
        register, accounts, ..
    } = state;
    filters_ui(ui, palette, register, accounts);
    register.refresh(ledger, range);
    ui.add_space(4.0);
    ui.label(RichText::new(&register.count_text).color(palette.text_muted));
    ui.add_space(4.0);

    let narrow = notedeck::ui::is_narrow(ui.ctx());
    let columns = Columns::new(ui.available_width(), narrow);
    header_ui(ui, palette, columns);
    let total = register.ids.len();
    egui::ScrollArea::vertical()
        .id_salt("cashflow_register")
        .auto_shrink([false, false])
        .show_rows(ui, ROW_HEIGHT, total, |ui, rows| {
            register.show_window(ui.painter(), ledger, labels, rows.clone());
            for row in &register.window.rows {
                row_ui(ui, palette, columns, row);
            }
        });
}

/// The filter bar: payee search, account subtree, source, unknown-only.
fn filters_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    register: &mut RegisterState,
    accounts: &mut Accounts,
) {
    let filter = &mut register.filter;
    ui.horizontal_wrapped(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut filter.payee)
                .hint_text("Payee")
                .desired_width(180.0),
        );
        let account = filter.account.as_deref().unwrap_or("Any account");
        let open = register.picker.is_some();
        if ui
            .add(egui::Button::selectable(
                open,
                RichText::new(account).color(palette.text),
            ))
            .clicked()
        {
            register.picker = match register.picker {
                Some(_) => None,
                None => {
                    let mut picker = AccountPicker::default();
                    picker.open();
                    Some(picker)
                }
            };
        }
        if filter.account.is_some() && ui.small_button("✕").clicked() {
            filter.account = None;
        }
        let chip = |ui: &mut egui::Ui, selected: bool, label: &str| {
            let color = if selected {
                palette.text
            } else {
                palette.text_muted
            };
            ui.add(egui::Button::selectable(
                selected,
                RichText::new(label).color(color),
            ))
            .clicked()
        };
        if chip(ui, filter.source.is_none(), "Any source") {
            filter.source = None;
        }
        for s in &register.sources {
            let selected = filter.source.as_deref() == Some(s.as_str());
            if chip(ui, selected, s) && !selected {
                filter.source = Some(s.clone());
            }
        }
        ui.checkbox(&mut filter.unknown_only, "Unknown only");
    });
    let Some(picker) = register.picker.as_mut() else {
        return;
    };
    let choices = Choices {
        accounts: &accounts.names,
        recent: &accounts.recent,
        allow_new: false,
    };
    let picked = account_picker::show(ui, palette, picker, choices);
    if let Some(account) = picked.account {
        account_picker::remember(&mut accounts.recent, &account);
        filter.account = Some(account);
        register.picker = None;
    } else if picked.cancelled {
        register.picker = None;
    }
}

/// Where each column of a row starts, and how wide the stretchy ones are.
#[derive(Clone, Copy)]
struct Columns {
    payee: f32,
    account: f32,
    amount: f32,
    home: Option<f32>,
    end: f32,
}

impl Columns {
    /// The columns of a register `width` wide. A phone drops the home
    /// commodity's column.
    fn new(width: f32, narrow: bool) -> Self {
        let date = if narrow { 82.0 } else { DATE_WIDTH };
        let amounts = if narrow {
            AMOUNT_WIDTH
        } else {
            2.0 * AMOUNT_WIDTH
        };
        let rest = (width - date - amounts).max(120.0);
        let payee = date;
        let account = payee + rest * 0.55;
        let amount = account + rest * 0.45;
        Columns {
            payee,
            account,
            amount,
            home: (!narrow).then_some(amount + AMOUNT_WIDTH),
            end: amount + amounts,
        }
    }
}

/// The column headings.
fn header_ui(ui: &mut egui::Ui, palette: &Palette, columns: Columns) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), Sense::hover());
    let painter = ui.painter_at(rect);
    let font = FontId::proportional(12.0);
    let y = rect.center().y;
    let at = |x: f32| egui::pos2(rect.left() + x, y);
    let color = palette.text_muted;
    painter.text(at(0.0), Align2::LEFT_CENTER, "Date", font.clone(), color);
    painter.text(
        at(columns.payee),
        Align2::LEFT_CENTER,
        "Payee",
        font.clone(),
        color,
    );
    painter.text(
        at(columns.account),
        Align2::LEFT_CENTER,
        "Account",
        font.clone(),
        color,
    );
    let amount_end = columns.home.unwrap_or(columns.end);
    painter.text(
        at(amount_end - 8.0),
        Align2::RIGHT_CENTER,
        "Amount",
        font.clone(),
        color,
    );
    if columns.home.is_some() {
        painter.text(
            at(columns.end - 8.0),
            Align2::RIGHT_CENTER,
            "Home",
            font,
            color,
        );
    }
    painter.hline(
        rect.x_range(),
        rect.bottom(),
        egui::Stroke::new(1.0, palette.border),
    );
}

/// One row of the register. Hovering its account says what decided it.
fn row_ui(ui: &mut egui::Ui, palette: &Palette, columns: Columns, row: &Row) {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), Sense::hover());
    let painter = ui.painter_at(rect);
    let y = rect.center().y;
    // A galley's top-left so it sits vertically centred, starting at `x`
    // (or, right-aligned, ending there).
    let left = |x: f32, g: &Galley| egui::pos2(rect.left() + x, y - g.size().y / 2.0);
    let right = |x: f32, g: &Galley| egui::pos2(rect.left() + x - g.size().x, y - g.size().y / 2.0);
    let cell = |from: f32, to: f32| {
        egui::Rect::from_x_y_ranges(rect.left() + from..=rect.left() + to - 8.0, rect.y_range())
    };
    painter.galley(left(0.0, &row.date), row.date.clone(), palette.text_muted);
    painter
        .with_clip_rect(cell(columns.payee, columns.account))
        .galley(
            left(columns.payee, &row.payee),
            row.payee.clone(),
            palette.text,
        );
    let account_color = if row.unknown {
        palette.warning
    } else {
        palette.text
    };
    let account_cell = cell(columns.account, columns.amount);
    painter.with_clip_rect(account_cell).galley(
        left(columns.account, &row.account),
        row.account.clone(),
        account_color,
    );
    let amount_color = if row.incoming {
        palette.income
    } else {
        palette.text
    };
    let amount_end = columns.home.unwrap_or(columns.end) - 8.0;
    painter.galley(
        right(amount_end, &row.amount),
        row.amount.clone(),
        amount_color,
    );
    if columns.home.is_some() {
        painter.galley(
            right(columns.end - 8.0, &row.home),
            row.home.clone(),
            palette.text_muted,
        );
    }
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        egui::Stroke::new(0.5, palette.border),
    );
    if resp.hover_pos().is_some_and(|at| account_cell.contains(at)) {
        resp.on_hover_ui(|ui| {
            ui.label(RichText::new(&row.why).color(palette.text));
        });
    }
}
