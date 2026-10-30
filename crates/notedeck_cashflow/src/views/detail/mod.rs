//! One transaction's detail: its facts, why its counter leg went where it
//! did, where it came from, and what can be done with it.
//!
//! It is drawn in place of the view underneath it, which the rail or the tab
//! strip still names, while [`DetailState::open`] holds a record id: a
//! [`crate::route::CashflowRoute::Txn`] on the chrome's nav stack in the
//! app, the state alone in a chrome-less embedding. Its own "← <view>"
//! button and Escape step back.
//!
//! What it says is worked out once per record and load ([`model::text_of`]
//! over [`cashflow::detail::of`]), and laid out once per width
//! ([`Galleys`]), so a frame that holds still formats and lays out nothing:
//! it paints the galleys it has.
//!
//! - **Wide** (≥ [`TWO_COLUMNS`]): the facts on the left, a
//!   [`ACTIONS_WIDTH`] column of actions on the right.
//! - **Narrow**: one column, why → actions → source.
//!
//! Its actions are the Categorize section ([`categorize`]), the Transfer
//! section ([`transfer`]), first when a transfer is suggested or linked,
//! and the record's own: copy it, show its payee.

pub mod categorize;
pub mod model;
pub mod transfer;

use std::sync::Arc;

use egui::{Color32, FontFamily, FontId, Galley, Sense};

use super::ViewCx;
use super::transactions::model::RegisterFilter;
use crate::View;
use crate::ui::palette::Palette;
use model::{DetailText, Tier, TransferText};

/// From this content width, the actions get their own column.
pub const TWO_COLUMNS: f32 = 720.0;

/// The width of the actions column.
pub const ACTIONS_WIDTH: f32 = 300.0;

/// The gap between the two columns.
const GUTTER: f32 = 24.0;

/// Height of one posting row.
const POSTING_HEIGHT: f32 = 24.0;

/// Width of each amount column of a posting row.
const AMOUNT_WIDTH: f32 = 110.0;

/// Width of a metadata key.
const META_KEY_WIDTH: f32 = 150.0;

/// What the detail keeps between frames.
#[derive(Default)]
pub(crate) struct DetailState {
    /// The record whose detail shows; `None` shows the view underneath.
    pub(crate) open: Option<String>,
    /// What [`Self::open`] says, worked out for one load.
    cached: Option<Cached>,
    /// Its Categorize section's editors and the write waiting to apply.
    categorize: categorize::CategorizeState,
    /// Its Transfer section's link or unlink waiting to apply.
    transfer: transfer::TransferState,
}

/// One record's text and galleys, and what they were made from.
struct Cached {
    /// The books' generation it was worked out for.
    generation: u64,
    /// The record.
    record: String,
    /// What the detail says; `None` when the books don't hold the record.
    text: Option<DetailText>,
    /// That laid out; `None` until the first frame draws it.
    galleys: Option<Galleys>,
}

/// What a frame of the detail asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DetailOut {
    /// Step back: its "←" or Escape.
    pub(crate) back: bool,
}

impl DetailState {
    /// The record whose detail shows, if any.
    pub(crate) fn open(&self) -> Option<&str> {
        self.open.as_deref()
    }

    /// Show `record`'s detail, or the view underneath for `None`. The same
    /// record again keeps what was worked out for it.
    pub(crate) fn set_open(&mut self, record: Option<&str>) {
        if self.open.as_deref() == record {
            return;
        }
        self.open = record.map(str::to_owned);
        self.categorize = categorize::CategorizeState::default();
        self.transfer = transfer::TransferState::default();
    }

    /// A vault write failed with `err`: the section that queued it says
    /// so.
    pub(crate) fn write_failed(&mut self, err: &str) {
        self.categorize.write_failed(err);
        self.transfer.write_failed(err);
    }

    /// Close the detail because a view was picked over it. True when one
    /// was open, so its nav entries go.
    pub(crate) fn leave(&mut self) -> bool {
        self.open.take().is_some()
    }

    /// Work out [`Self::open`]'s text again if the books or the record
    /// changed.
    fn refresh(&mut self, cx: &ViewCx<'_>) {
        let Some(record) = self.open.as_deref() else {
            return;
        };
        let generation = cx.ledger.map_or(0, |l| l.generation);
        let fresh = self
            .cached
            .as_ref()
            .is_some_and(|c| c.generation == generation && c.record == record);
        if fresh {
            return;
        }
        // The triage's inputs are the ones these books were derived with,
        // which `detail::of` needs to trace a decision to its edit.
        let text = cx.ledger.and_then(|ledger| {
            let detail =
                cashflow::detail::of(ledger.books, &ledger.triage.inputs, cx.projects, record)?;
            Some(model::text_of(model::TextOf {
                books: ledger.books,
                labels: cx.labels,
                detail: &detail,
                suggested: ledger.triage.suggestion_for(record),
                now: unix_now(),
            }))
        });
        self.cached = Some(Cached {
            generation,
            record: record.to_owned(),
            text,
            galleys: None,
        });
    }
}

/// "payee · date" of `record` in `books`, for its nav entry; the record id
/// itself when the books don't hold it.
pub(crate) fn title_of(books: &cashflow::Books, record: &str) -> String {
    let ledger = books.ledger();
    let Some(txn) = ledger.find(record) else {
        return record.to_owned();
    };
    let journal = ledger.journal();
    let row = journal.txn(txn);
    model::title(journal.strings.get(row.payee), row.date)
}

/// The system clock, in unix seconds.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Every text of the detail, laid out in [`Color32::PLACEHOLDER`] so
/// painting picks the color, for one width and scale.
struct Galleys {
    /// The width of the facts column they wrap in.
    facts_width: f32,
    /// The width of the actions column.
    actions_width: f32,
    pixels_per_point: f32,
    /// The view the back button names.
    under: View,
    back: Arc<Galley>,
    title: Arc<Galley>,
    payee: Arc<Galley>,
    subtitle: Arc<Galley>,
    tier: Arc<Galley>,
    amount: Arc<Galley>,
    home: Option<Arc<Galley>>,
    postings_heading: Arc<Galley>,
    postings: Vec<PostingGalleys>,
    /// Whether any posting has a home amount, so the column is drawn.
    any_home: bool,
    why_heading: Arc<Galley>,
    headline: Arc<Galley>,
    lines: Vec<Arc<Galley>>,
    written: Option<Arc<Galley>>,
    agentium: Option<Arc<Galley>>,
    rejected: Vec<Arc<Galley>>,
    source_heading: Arc<Galley>,
    record: Arc<Galley>,
    origin: Arc<Galley>,
    segment: Arc<Galley>,
    meta: Vec<MetaGalleys>,
    comments: Vec<Arc<Galley>>,
    actions_heading: Arc<Galley>,
    copy_record: Arc<Galley>,
    copy_ledger: Arc<Galley>,
    show_payee: Arc<Galley>,
    categorize: categorize::CategorizeGalleys,
    transfer: transfer::TransferGalleys,
    /// Whether the Transfer section comes before Categorize: a transfer is
    /// suggested or linked, so it is the thing to act on.
    transfer_first: bool,
}

/// One posting's galleys.
struct PostingGalleys {
    account: Arc<Galley>,
    full: Option<Arc<Galley>>,
    amount: Arc<Galley>,
    home: Arc<Galley>,
    unknown: bool,
}

/// One metadata entry's galleys.
struct MetaGalleys {
    key: Arc<Galley>,
    value: Arc<Galley>,
}

/// The widths a frame draws the detail at.
#[derive(Clone, Copy, PartialEq)]
struct Widths {
    /// Whether the actions get their own column.
    two_columns: bool,
    /// The facts column.
    facts: f32,
    /// The actions column.
    actions: f32,
}

impl Widths {
    /// The columns of a detail `width` wide.
    fn of(width: f32) -> Self {
        if width >= TWO_COLUMNS {
            Widths {
                two_columns: true,
                facts: width - ACTIONS_WIDTH - GUTTER,
                actions: ACTIONS_WIDTH,
            }
        } else {
            Widths {
                two_columns: false,
                facts: width,
                actions: width,
            }
        }
    }
}

/// Lays text out for [`Galleys`].
struct Layouter<'a> {
    painter: &'a egui::Painter,
    bold: FontFamily,
}

impl Layouter<'_> {
    /// `text` at `size`, on one line.
    fn line(&self, text: &str, size: f32) -> Arc<Galley> {
        self.painter.layout_no_wrap(
            text.to_owned(),
            FontId::proportional(size),
            Color32::PLACEHOLDER,
        )
    }

    /// `text` at `size` in the bold face, on one line.
    fn bold(&self, text: &str, size: f32) -> Arc<Galley> {
        self.painter.layout_no_wrap(
            text.to_owned(),
            FontId::new(size, self.bold.clone()),
            Color32::PLACEHOLDER,
        )
    }

    /// `text` at `size`, wrapped at `width`.
    fn wrapped(&self, text: &str, size: f32, width: f32) -> Arc<Galley> {
        self.painter.layout(
            text.to_owned(),
            FontId::proportional(size),
            Color32::PLACEHOLDER,
            width.max(40.0),
        )
    }
}

impl Galleys {
    /// `text` laid out for `widths` with `painter`'s fonts, the back button
    /// naming `under`.
    fn lay_out(
        painter: &egui::Painter,
        palette: &Palette,
        text: &DetailText,
        widths: Widths,
        under: View,
    ) -> Self {
        let l = Layouter {
            painter,
            bold: palette.faces.bold.clone(),
        };
        let facts = widths.facts;
        // The postings' account column is what is left of the row.
        let account_width = facts - 2.0 * AMOUNT_WIDTH;
        Galleys {
            facts_width: widths.facts,
            actions_width: widths.actions,
            pixels_per_point: painter.pixels_per_point(),
            under,
            back: l.line(&format!("← {}", under.label()), 14.0),
            title: l.line(&text.title, 14.0),
            payee: l.bold(&text.payee, 22.0),
            subtitle: l.line(&text.subtitle, 13.0),
            tier: l.line(text.tier.label(), 12.0),
            amount: l.bold(&text.amount, 22.0),
            home: text.home.as_deref().map(|h| l.line(h, 13.0)),
            postings_heading: l.bold("Postings", 15.0),
            any_home: text.postings.iter().any(|p| !p.home.is_empty()),
            postings: text
                .postings
                .iter()
                .map(|p| PostingGalleys {
                    account: l.wrapped(&p.account, 14.0, account_width),
                    full: p.full.as_deref().map(|f| l.line(f, 12.0)),
                    amount: l.line(&p.amount, 14.0),
                    home: l.line(&p.home, 13.0),
                    unknown: p.unknown,
                })
                .collect(),
            why_heading: l.bold("Why this account", 15.0),
            headline: l.wrapped(&text.why.headline, 15.0, facts),
            lines: text
                .why
                .lines
                .iter()
                .map(|s| l.wrapped(s, 13.0, facts))
                .collect(),
            written: text.why.written.as_deref().map(|w| l.line(w, 12.0)),
            agentium: text.why.agentium.as_deref().map(|a| l.line(a, 12.0)),
            rejected: text
                .why
                .rejected
                .iter()
                .map(|s| l.wrapped(s, 13.0, facts))
                .collect(),
            source_heading: l.bold("Source", 15.0),
            record: l.line(&text.record, 13.0),
            origin: l.line(&text.source.origin, 13.0),
            segment: l.line(&text.source.segment, 13.0),
            meta: text
                .meta
                .iter()
                .map(|m| MetaGalleys {
                    key: l.line(&m.key, 12.0),
                    value: l.wrapped(&m.value, 12.0, facts - META_KEY_WIDTH),
                })
                .collect(),
            comments: text
                .comments
                .iter()
                .map(|c| l.wrapped(c, 12.0, facts))
                .collect(),
            actions_heading: l.bold("Actions", 15.0),
            copy_record: l.line("Copy record id", 14.0),
            copy_ledger: l.line("Copy as ledger", 14.0),
            show_payee: l.line("Show payee in register", 14.0),
            categorize: categorize::CategorizeGalleys::lay_out(&l, text, widths.actions),
            transfer: transfer::TransferGalleys::lay_out(&l, &text.transfer, widths.actions),
            transfer_first: matches!(
                text.transfer,
                TransferText::Suggested(_) | TransferText::Linked { .. }
            ),
        }
    }

    /// Whether these were laid out for `widths` and `under` at `ppp`.
    fn fits(&self, widths: Widths, under: View, ppp: f32) -> bool {
        self.facts_width == widths.facts
            && self.actions_width == widths.actions
            && self.pixels_per_point == ppp
            && self.under == under
    }
}

/// A click in the detail this frame.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Click {
    Back,
    CopyRecord,
    CopyLedger,
    ShowPayee,
    Agentium,
    /// Something the Categorize section asked for.
    Categorize(categorize::Event),
    /// Something the Transfer section asked for.
    Transfer(transfer::Event),
}

/// Draw the open record's detail, over `under`, the view the rail names.
#[profiling::function]
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut egui::Ui, under: View) -> DetailOut {
    let palette = cx.palette;
    let mut state = std::mem::take(&mut cx.states.detail);
    state.refresh(cx);
    if let (Some(ledger), Some(record)) = (cx.ledger, state.open.as_deref()) {
        cx.states.transactions.accounts.refresh(ledger);
        state
            .categorize
            .prepare(cx.plans, ledger.generation, record);
        let shown = state.cached.as_ref().and_then(|c| c.text.as_ref());
        let transfer = shown.map_or(&TransferText::NotApplicable, |t| &t.transfer);
        state
            .transfer
            .prepare(cx.plans, ledger.generation, transfer);
    }
    let widths = Widths::of(ui.available_width());
    let ppp = ui.ctx().pixels_per_point();

    // Escape steps back, consumed so the chrome's own Escape (the side
    // menu) doesn't fire too. Only the pass that lands on the detail acts.
    let mut click = None;
    if cx.lands
        && ui
            .ctx()
            .input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        click = Some(Click::Back);
    }

    let DetailState {
        cached,
        categorize,
        transfer,
        ..
    } = &mut state;
    match cached.as_mut() {
        Some(Cached {
            text: Some(text),
            galleys,
            ..
        }) => {
            if !galleys.as_ref().is_some_and(|g| g.fits(widths, under, ppp)) {
                *galleys = Some(Galleys::lay_out(ui.painter(), palette, text, widths, under));
            }
            let Some(g) = galleys.as_ref() else {
                return DetailOut::default();
            };
            let sections = Sections {
                categorize: categorize::Section {
                    state: categorize,
                    choices: cx.states.transactions.accounts.choices(true),
                },
                transfer,
            };
            if let Some(c) = detail_ui(ui, palette, text.tier, g, widths, sections) {
                click = Some(c);
            }
        }
        _ => {
            if missing_ui(ui, palette, under, state.open()) {
                click = Some(Click::Back);
            }
        }
    }

    let out = act(cx, ui, &mut state, click);
    cx.states.detail = state;
    out
}

/// Do what `click` asked for.
fn act(
    cx: &mut ViewCx<'_>,
    ui: &egui::Ui,
    state: &mut DetailState,
    click: Option<Click>,
) -> DetailOut {
    let text = state.cached.as_ref().and_then(|c| c.text.as_ref());
    match click {
        None => {}
        Some(Click::Back) => return DetailOut { back: true },
        Some(Click::CopyRecord) => {
            if let Some(record) = state.open() {
                ui.ctx().copy_text(record.to_owned());
            }
        }
        Some(Click::CopyLedger) => {
            if let Some(ledger) = state.open().and_then(|r| ledger_text(cx, r)) {
                ui.ctx().copy_text(ledger);
            }
        }
        Some(Click::ShowPayee) => {
            if let Some(text) = text {
                cx.states.transactions.show_register(RegisterFilter {
                    payee: text.payee.clone(),
                    whole_payee: true,
                    ..RegisterFilter::default()
                });
                cx.open = Some(View::Transactions);
            }
        }
        Some(Click::Agentium) => {
            cx.open_card = text.and_then(|t| t.why.agentium.clone());
        }
        Some(Click::Categorize(event)) => {
            let (Some(text), Some(ledger)) = (text, cx.ledger) else {
                return DetailOut::default();
            };
            let write = categorize::Cx {
                books: ledger.books,
                raw: ledger.raw,
                edits: &ledger.triage.inputs.edits,
                generation: ledger.generation,
                plans: cx.plans,
                outbox: cx.outbox,
                accounts: &mut cx.states.transactions.accounts,
            };
            categorize::act(&mut state.categorize, event, text, write);
        }
        Some(Click::Transfer(event)) => {
            let (Some(text), Some(ledger)) = (text, cx.ledger) else {
                return DetailOut::default();
            };
            match event {
                transfer::Event::OpenOther => cx.open_txn = other_side(&text.transfer),
                transfer::Event::Uncategorize => {
                    let open = categorize::Event::Open(categorize::Open::Uncategorize);
                    let write = categorize::Cx {
                        books: ledger.books,
                        raw: ledger.raw,
                        edits: &ledger.triage.inputs.edits,
                        generation: ledger.generation,
                        plans: cx.plans,
                        outbox: cx.outbox,
                        accounts: &mut cx.states.transactions.accounts,
                    };
                    categorize::act(&mut state.categorize, open, text, write);
                }
                event => {
                    let write = transfer::Cx {
                        plans: cx.plans,
                        outbox: cx.outbox,
                        generation: ledger.generation,
                    };
                    transfer::act(
                        &mut state.transfer,
                        event,
                        &text.record,
                        &text.transfer,
                        write,
                    );
                }
            }
        }
    }
    DetailOut::default()
}

/// The other side of the transfer `transfer` shows, suggested or linked.
fn other_side(transfer: &TransferText) -> Option<String> {
    match transfer {
        TransferText::Suggested(leg) => Some(leg.record.clone()),
        TransferText::Linked { other } => Some(other.clone()),
        TransferText::NotApplicable | TransferText::Unmatched | TransferText::Categorized => None,
    }
}

/// `record` as the books write it, ledger-cli text.
fn ledger_text(cx: &ViewCx<'_>, record: &str) -> Option<String> {
    let ledger = cx.ledger?.books.ledger();
    let txn = ledger.find(record)?;
    let mut out = String::new();
    ledger
        .journal()
        .write_txns([txn], &mut out, ledgerdb::WriteOptions::default())
        .ok()?;
    Some(out)
}

/// The placeholder for a record the loaded books don't hold. True when
/// its back is clicked.
fn missing_ui(ui: &mut egui::Ui, palette: &Palette, under: View, record: Option<&str>) -> bool {
    let back = ui
        .add(egui::Button::new(egui::RichText::new(format!("← {}", under.label()))).frame(false))
        .clicked();
    ui.add_space(24.0);
    ui.label(
        egui::RichText::new("Not in the loaded journal (is its layer off?)")
            .size(16.0)
            .color(palette.text),
    );
    if let Some(record) = record {
        ui.label(egui::RichText::new(record).color(palette.text_muted));
    }
    back
}

/// The action sections' state, which [`detail_ui`] draws with.
struct Sections<'a> {
    categorize: categorize::Section<'a>,
    transfer: &'a transfer::TransferState,
}

/// The detail, laid out in `g`.
fn detail_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    tier: Tier,
    g: &Galleys,
    widths: Widths,
    sections: Sections<'_>,
) -> Option<Click> {
    let mut click = None;
    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new(g.back.clone()).frame(false))
            .clicked()
        {
            click = Some(Click::Back);
        }
        // A phone's hero says the same right under it, and has no room.
        if widths.two_columns {
            text(ui, &g.title, palette.text_muted);
        }
    });
    ui.add_space(12.0);
    egui::ScrollArea::vertical()
        .id_salt("cashflow_detail")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let mut on = |c: Option<Click>| {
                if c.is_some() {
                    click = c;
                }
            };
            if widths.two_columns {
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(widths.facts, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(widths.facts);
                            hero_ui(ui, palette, tier, g, true);
                            postings_ui(ui, palette, g);
                            on(why_ui(ui, palette, g));
                            source_ui(ui, palette, g);
                        },
                    );
                    ui.add_space(GUTTER);
                    ui.allocate_ui_with_layout(
                        egui::vec2(widths.actions, 0.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(widths.actions);
                            on(actions_ui(ui, palette, g, sections));
                        },
                    );
                });
            } else {
                hero_ui(ui, palette, tier, g, false);
                postings_ui(ui, palette, g);
                on(why_ui(ui, palette, g));
                on(actions_ui(ui, palette, g, sections));
                source_ui(ui, palette, g);
            }
        });
    click
}

/// Paint `galley` in `color` where the layout puts it, as a label.
fn text(ui: &mut egui::Ui, galley: &Arc<Galley>, color: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(galley.size(), Sense::hover());
    ui.painter().galley(rect.min, galley.clone(), color);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, galley.text()));
    resp
}

/// A section's heading, with the space above it.
fn heading(ui: &mut egui::Ui, palette: &Palette, galley: &Arc<Galley>) {
    ui.add_space(18.0);
    text(ui, galley, palette.text);
    ui.add_space(6.0);
}

/// The payee, its tier chip and the amount, and the date and source under
/// them; `wide` puts the amount at the payee's right, a phone under it.
fn hero_ui(ui: &mut egui::Ui, palette: &Palette, tier: Tier, g: &Galleys, wide: bool) {
    ui.horizontal(|ui| {
        text(ui, &g.payee, palette.text);
        ui.add_space(8.0);
        chip(ui, &g.tier, tier_color(palette, tier));
        if wide {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                text(ui, &g.amount, palette.text);
            });
        }
    });
    if !wide {
        text(ui, &g.amount, palette.text);
    }
    ui.horizontal(|ui| {
        text(ui, &g.subtitle, palette.text_muted);
        if let Some(home) = &g.home {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                text(ui, home, palette.text_muted);
            });
        }
    });
}

/// The color a tier's chip is drawn in.
fn tier_color(palette: &Palette, tier: Tier) -> Color32 {
    match tier {
        Tier::Unknown => palette.warning,
        Tier::Transfer => palette.transfer,
        Tier::AsImported => palette.text_muted,
        Tier::Rule | Tier::Override | Tier::Split => palette.accent,
    }
}

/// `galley` in a rounded outline of `color`.
fn chip(ui: &mut egui::Ui, galley: &Arc<Galley>, color: Color32) {
    let pad = egui::vec2(8.0, 3.0);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + 2.0 * pad, Sense::hover());
    ui.painter().rect_stroke(
        rect,
        rect.height() / 2.0,
        egui::Stroke::new(1.0, color),
        egui::StrokeKind::Inside,
    );
    ui.painter().galley(rect.min + pad, galley.clone(), color);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, galley.text()));
}

/// Every posting: account, amount and home amount, an unknown one in the
/// warning color.
fn postings_ui(ui: &mut egui::Ui, palette: &Palette, g: &Galleys) {
    heading(ui, palette, &g.postings_heading);
    for (i, p) in g.postings.iter().enumerate() {
        let full = p.full.as_ref().map_or(0.0, |f| f.size().y);
        let height = (p.account.size().y + full + 6.0).max(POSTING_HEIGHT);
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), height), Sense::hover());
        let painter = ui.painter_at(rect);
        let account = if p.unknown {
            palette.warning
        } else {
            palette.text
        };
        let top = rect.top() + 3.0;
        painter.galley(egui::pos2(rect.left(), top), p.account.clone(), account);
        if let Some(f) = &p.full {
            let at = egui::pos2(rect.left(), top + p.account.size().y);
            painter.galley(at, f.clone(), palette.text_muted);
        }
        let right = |x: f32, g: &Galley| egui::pos2(x - g.size().x, top);
        let amount_end = if g.any_home {
            rect.right() - AMOUNT_WIDTH
        } else {
            rect.right()
        };
        painter.galley(right(amount_end, &p.amount), p.amount.clone(), palette.text);
        painter.galley(
            right(rect.right(), &p.home),
            p.home.clone(),
            palette.text_muted,
        );
        painter.hline(
            rect.x_range(),
            rect.bottom() - 0.5,
            egui::Stroke::new(0.5, palette.border),
        );
        // The rows are painted, so tell accesskit what they say.
        let id = egui::Id::unique(("cashflow_detail_posting", i));
        let resp = ui.interact(rect, id, Sense::hover());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, p.account.text()));
    }
}

/// Why the counter leg went where it did, and who wrote the note that
/// decided it.
fn why_ui(ui: &mut egui::Ui, palette: &Palette, g: &Galleys) -> Option<Click> {
    heading(ui, palette, &g.why_heading);
    text(ui, &g.headline, palette.text);
    for line in &g.lines {
        text(ui, line, palette.text_muted);
    }
    let mut click = None;
    if g.written.is_some() || g.agentium.is_some() {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if let Some(written) = &g.written {
                text(ui, written, palette.text_muted);
            }
            if let Some(agentium) = &g.agentium
                && ui
                    .add(egui::Button::new(agentium.clone()).small())
                    .on_hover_text("Open the session that wrote it")
                    .clicked()
            {
                click = Some(Click::Agentium);
            }
        });
    }
    for rejected in &g.rejected {
        text(ui, rejected, palette.warning);
    }
    click
}

/// The record id and where it came from, then its metadata and comments.
fn source_ui(ui: &mut egui::Ui, palette: &Palette, g: &Galleys) {
    heading(ui, palette, &g.source_heading);
    text(ui, &g.record, palette.text);
    text(ui, &g.origin, palette.text_muted);
    text(ui, &g.segment, palette.text_muted);
    if !g.meta.is_empty() {
        ui.add_space(8.0);
    }
    for m in &g.meta {
        ui.horizontal_top(|ui| {
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(META_KEY_WIDTH, m.key.size().y), Sense::hover());
            ui.painter()
                .galley(rect.min, m.key.clone(), palette.text_muted);
            text(ui, &m.value, palette.text);
        });
    }
    if !g.comments.is_empty() {
        ui.add_space(8.0);
    }
    for c in &g.comments {
        text(ui, c, palette.text_muted);
    }
}

/// What can be done with the record: categorize it and link it as a
/// transfer, the transfer first when one is suggested or linked, then the
/// record's own actions. A later card adds the project here.
fn actions_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    g: &Galleys,
    sections: Sections<'_>,
) -> Option<Click> {
    let Sections {
        categorize,
        transfer,
    } = sections;
    let mut click = None;
    let mut on = |c: Option<Click>| {
        if c.is_some() {
            click = c;
        }
    };
    let transfer_ui = |ui: &mut egui::Ui| {
        transfer::section_ui(ui, palette, &g.transfer, transfer).map(Click::Transfer)
    };
    if g.transfer_first {
        on(transfer_ui(ui));
    }
    on(categorize::section_ui(ui, palette, &g.categorize, categorize).map(Click::Categorize));
    if !g.transfer_first {
        on(transfer_ui(ui));
    }
    heading(ui, palette, &g.actions_heading);
    for (galley, c) in [
        (&g.copy_record, Click::CopyRecord),
        (&g.copy_ledger, Click::CopyLedger),
        (&g.show_payee, Click::ShowPayee),
    ] {
        if ui.add(egui::Button::new(galley.clone())).clicked() {
            click = Some(c);
        }
    }
    click
}
