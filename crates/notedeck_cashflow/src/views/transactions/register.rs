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
use cashflow::projects::Projects;
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
    /// Show every transaction `filter` matches, as a link from another view
    /// does: the person's own filters are replaced, and an open account
    /// picker closes.
    pub(crate) fn show(&mut self, filter: RegisterFilter) {
        self.filter = filter;
        self.picker = None;
    }

    /// Match again if the books, the period or the filters moved. A change
    /// to `projects` bumps the generation too.
    #[profiling::function]
    fn refresh(&mut self, ledger: LedgerCx<'_>, projects: &Projects, range: DateRange) {
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
        self.ids = model::matching(ledger.books, projects, range, &self.filter);
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

/// What the register reads besides its own state.
#[derive(Clone, Copy)]
pub(crate) struct RegisterCx<'a> {
    /// The books and their triage.
    pub(crate) ledger: LedgerCx<'a>,
    /// The chart's display names and icons for accounts.
    pub(crate) labels: &'a Labels,
    /// Which project each account is in, for the "No project" filter.
    pub(crate) projects: &'a Projects,
    /// The days the shared period covers.
    pub(crate) range: DateRange,
}

/// Draw the register. Returns the record id of a row clicked, whose
/// detail to open.
#[profiling::function]
pub(crate) fn ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    cx: RegisterCx<'_>,
    state: &mut TransactionsState,
) -> Option<String> {
    let RegisterCx {
        ledger,
        labels,
        projects,
        range,
    } = cx;
    let TransactionsState {
        register, accounts, ..
    } = state;
    filters_ui(ui, palette, register, accounts, !projects.is_empty());
    register.refresh(ledger, projects, range);
    ui.add_space(4.0);
    ui.label(RichText::new(&register.count_text).color(palette.text_muted));
    ui.add_space(4.0);

    let narrow = notedeck::ui::is_narrow(ui.ctx());
    let columns = Columns::new(ui.available_width(), narrow);
    header_ui(ui, palette, columns);
    let total = register.ids.len();
    let mut clicked = None;
    egui::ScrollArea::vertical()
        .id_salt("cashflow_register")
        .auto_shrink([false, false])
        .show_rows(ui, ROW_HEIGHT, total, |ui, rows| {
            register.show_window(ui.painter(), ledger, labels, rows.clone());
            let first = register.window.first;
            for (i, row) in register.window.rows.iter().enumerate() {
                if row_ui(ui, palette, columns, row, first + i) {
                    clicked = Some(first + i);
                }
            }
        });
    let txn = *register.ids.get(clicked?)?;
    let journal = ledger.books.ledger().journal();
    let record = journal.txn(txn).id?;
    Some(journal.strings.get(record).to_owned())
}

/// The filter bar: payee search, account subtree, source, unknown-only,
/// and no-project when any account is in a project (`attributed`) or the
/// filter is already on.
fn filters_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    register: &mut RegisterState,
    accounts: &mut Accounts,
    attributed: bool,
) {
    let filter = &mut register.filter;
    ui.horizontal_wrapped(|ui| {
        let search = ui.add(
            egui::TextEdit::singleline(&mut filter.payee)
                .hint_text("Payee")
                .desired_width(180.0),
        );
        // Typing makes it a search again.
        if search.changed() {
            filter.whole_payee = false;
        }
        if filter.whole_payee
            && ui
                .add(egui::Button::selectable(
                    true,
                    RichText::new("Exact payee ✕").color(palette.text),
                ))
                .on_hover_text("Only this payee, not every payee containing it.")
                .clicked()
        {
            filter.whole_payee = false;
        }
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
        };
        if chip(ui, filter.source.is_none(), "Any source").clicked() {
            filter.source = None;
        }
        for s in &register.sources {
            let selected = filter.source.as_deref() == Some(s.as_str());
            if chip(ui, selected, s).clicked() && !selected {
                filter.source = Some(s.clone());
            }
        }
        if (attributed || filter.unattributed)
            && chip(ui, filter.unattributed, "No project")
                .on_hover_text("Only income and routing no project claims.")
                .clicked()
        {
            filter.unattributed = !filter.unattributed;
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
        ..Choices::default()
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

/// One row of the register, match `index`: true when it was clicked, to
/// open its detail. Hovering its account says what decided it.
fn row_ui(ui: &mut egui::Ui, palette: &Palette, columns: Columns, row: &Row, index: usize) -> bool {
    let (_, rect) = ui.allocate_space(egui::vec2(ui.available_width(), ROW_HEIGHT));
    // A fresh id over the whole row, so the click lands wherever it is,
    // keyed by the match so it stays the row's as the window scrolls.
    let resp = ui.interact(
        rect,
        egui::Id::unique(("cashflow_register_row", index)),
        Sense::click(),
    );
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, row.payee.text()));
    let painter = ui.painter_at(rect);
    if resp.hovered() {
        painter.rect_filled(rect, 4.0, palette.surface);
    }
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
    let clicked = resp.clicked();
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.hover_pos().is_some_and(|at| account_cell.contains(at)) {
        resp.on_hover_ui(|ui| {
            ui.label(RichText::new(&row.why).color(palette.text));
        });
    }
    clicked
}
