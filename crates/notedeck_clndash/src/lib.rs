use crate::event::Channel;
use crate::event::Channels;
use crate::event::ClnResponse;
use crate::event::Event;
use crate::event::Invoice;
use crate::event::ListPeerChannel;
use crate::event::Request;
use crate::watch::fetch_paid_invoices;

use egui::{Color32, Label, RichText, Widget};
use lnsocket::bitcoin::secp256k1::{PublicKey, SecretKey, rand};
use lnsocket::{CommandoClient, LNSocket};
use nostrdb::Ndb;
use notedeck::{AppAction, AppContext};
use serde_json::json;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

mod event;
mod watch;

pub enum LoadingState<T, E> {
    Loading,
    Failed(E),
    Loaded(T),
}

impl<T, E> Default for LoadingState<T, E> {
    fn default() -> Self {
        Self::Loading
    }
}

impl<T, E> LoadingState<T, E> {
    fn _as_ref(&self) -> LoadingState<&T, &E> {
        match self {
            Self::Loading => LoadingState::<&T, &E>::Loading,
            Self::Failed(err) => LoadingState::<&T, &E>::Failed(err),
            Self::Loaded(t) => LoadingState::<&T, &E>::Loaded(t),
        }
    }

    fn from_result(res: Result<T, E>) -> LoadingState<T, E> {
        match res {
            Ok(r) => LoadingState::Loaded(r),
            Err(err) => LoadingState::Failed(err),
        }
    }

    /*
    fn unwrap(self) -> T {
        let Self::Loaded(t) = self else {
            panic!("unwrap in LoadingState");
        };

        t
    }
    */
}

#[derive(Default)]
pub struct ClnDash {
    initialized: bool,
    connection_state: ConnectionState,
    summary: LoadingState<Summary, lnsocket::Error>,
    get_info: LoadingState<String, lnsocket::Error>,
    channels: LoadingState<Channels, lnsocket::Error>,
    invoices: LoadingState<Vec<Invoice>, lnsocket::Error>,
    channel: Option<CommChannel>,
    last_summary: Option<Summary>,
    // invoice label to zapreq id
    invoice_zap_reqs: HashMap<String, [u8; 32]>,
}

#[derive(serde::Deserialize)]
pub struct ZapReqId {
    #[serde(with = "hex::serde")]
    id: [u8; 32],
}

impl Default for ConnectionState {
    fn default() -> Self {
        ConnectionState::Dead("uninitialized".to_string())
    }
}

struct CommChannel {
    req_tx: UnboundedSender<Request>,
    event_rx: UnboundedReceiver<Event>,
}

enum ConnectionState {
    Dead(String),
    Connecting,
    Active,
}

impl notedeck::App for ClnDash {
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> Option<AppAction> {
        if !self.initialized {
            self.connection_state = ConnectionState::Connecting;

            self.setup_connection();
            self.initialized = true;
        }

        self.process_events(ctx.ndb);

        self.show(ui, ctx);

        None
    }
}

fn connection_state_ui(ui: &mut egui::Ui, state: &ConnectionState) {
    match state {
        ConnectionState::Active => {
            ui.add(Label::new(RichText::new("Connected").color(Color32::GREEN)));
        }

        ConnectionState::Connecting => {
            ui.add(Label::new(
                RichText::new("Connecting").color(Color32::YELLOW),
            ));
        }

        ConnectionState::Dead(reason) => {
            ui.add(Label::new(
                RichText::new(format!("Disconnected: {reason}")).color(Color32::RED),
            ));
        }
    }
}

fn summary_ui(
    ui: &mut egui::Ui,
    last_summary: Option<&Summary>,
    summary: &LoadingState<Summary, lnsocket::Error>,
) {
    match summary {
        LoadingState::Loading => {
            ui.label("loading summary");
        }
        LoadingState::Failed(err) => {
            ui.label(format!("Failed to get summary: {err}"));
        }
        LoadingState::Loaded(summary) => {
            summary_cards_ui(ui, summary, last_summary);
            ui.add_space(8.0);
        }
    }
}

impl ClnDash {
    fn show(&mut self, ui: &mut egui::Ui, ctx: &mut AppContext) {
        egui::Frame::new()
            .inner_margin(egui::Margin::same(20))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    connection_state_ui(ui, &self.connection_state);
                    summary_ui(ui, self.last_summary.as_ref(), &self.summary);
                    invoices_ui(ui, &self.invoice_zap_reqs, ctx, &self.invoices);
                    channels_ui(ui, &self.channels);
                    get_info_ui(ui, &self.get_info);
                });
            });
    }

    fn setup_connection(&mut self) {
        let (req_tx, mut req_rx) = unbounded_channel::<Request>();
        let (event_tx, event_rx) = unbounded_channel::<Event>();
        self.channel = Some(CommChannel { req_tx, event_rx });

        tokio::spawn(async move {
            let key = SecretKey::new(&mut rand::thread_rng());
            let their_pubkey = PublicKey::from_str(
                "03f3c108ccd536b8526841f0a5c58212bb9e6584a1eb493080e7c1cc34f82dad71",
            )
            .unwrap();

            let lnsocket =
                match LNSocket::connect_and_init(key, their_pubkey, "ln.damus.io:9735").await {
                    Err(err) => {
                        let _ = event_tx.send(Event::Ended {
                            reason: err.to_string(),
                        });
                        return;
                    }

                    Ok(lnsocket) => {
                        let _ = event_tx.send(Event::Connected);
                        lnsocket
                    }
                };

            let rune = std::env::var("RUNE").unwrap_or(
                "Vns1Zxvidr4J8pP2ZCg3Wjp2SyGyyf5RHgvFG8L36yM9MzMmbWV0aG9kPWdldGluZm8=".to_string(),
            );
            let commando = Arc::new(CommandoClient::spawn(lnsocket, &rune));

            loop {
                match req_rx.recv().await {
                    None => {
                        let _ = event_tx.send(Event::Ended {
                            reason: "channel dead?".to_string(),
                        });
                        break;
                    }

                    Some(req) => {
                        tracing::debug!("calling {req:?}");
                        match req {
                            Request::GetInfo => {
                                let event_tx = event_tx.clone();
                                let commando = commando.clone();
                                tokio::spawn(async move {
                                    match commando.call("getinfo", json!({})).await {
                                        Ok(v) => {
                                            let _ = event_tx
                                                .send(Event::Response(ClnResponse::GetInfo(v)));
                                        }
                                        Err(err) => {
                                            tracing::error!("get_info error {}", err);
                                        }
                                    }
                                });
                            }

                            Request::PaidInvoices(n) => {
                                let event_tx = event_tx.clone();
                                let commando = commando.clone();
                                tokio::spawn(async move {
                                    let invoices = fetch_paid_invoices(commando, n).await;
                                    let _ = event_tx
                                        .send(Event::Response(ClnResponse::PaidInvoices(invoices)));
                                });
                            }

                            Request::ListPeerChannels => {
                                let event_tx = event_tx.clone();
                                let commando = commando.clone();
                                tokio::spawn(async move {
                                    let peer_channels =
                                        commando.call("listpeerchannels", json!({})).await;
                                    let channels = peer_channels.map(|v| {
                                        let peer_channels: Vec<ListPeerChannel> =
                                            serde_json::from_value(v["channels"].clone()).unwrap();
                                        to_channels(peer_channels)
                                    });
                                    let _ = event_tx.send(Event::Response(
                                        ClnResponse::ListPeerChannels(channels),
                                    ));
                                });
                            }
                        }
                    }
                }
            }
        });
    }

    fn process_events(&mut self, ndb: &Ndb) {
        let Some(channel) = &mut self.channel else {
            return;
        };

        while let Ok(event) = channel.event_rx.try_recv() {
            match event {
                Event::Ended { reason } => {
                    self.connection_state = ConnectionState::Dead(reason);
                }

                Event::Connected => {
                    self.connection_state = ConnectionState::Active;
                    let _ = channel.req_tx.send(Request::GetInfo);
                    let _ = channel.req_tx.send(Request::ListPeerChannels);
                    let _ = channel.req_tx.send(Request::PaidInvoices(100));
                }

                Event::Response(resp) => match resp {
                    ClnResponse::ListPeerChannels(chans) => {
                        if let LoadingState::Loaded(prev) = &self.channels {
                            self.last_summary = Some(compute_summary(prev));
                        }

                        self.summary = match &chans {
                            Ok(chans) => LoadingState::Loaded(compute_summary(chans)),
                            Err(err) => LoadingState::Failed(err.clone()),
                        };
                        self.channels = LoadingState::from_result(chans);
                    }

                    ClnResponse::GetInfo(value) => {
                        let res = serde_json::to_string_pretty(&value);
                        self.get_info =
                            LoadingState::from_result(res.map_err(|_| lnsocket::Error::Json));
                    }

                    ClnResponse::PaidInvoices(invoices) => {
                        // process zap requests

                        if let Ok(invoices) = &invoices {
                            for invoice in invoices {
                                let zap_req_id: Option<ZapReqId> =
                                    serde_json::from_str(&invoice.description).ok();
                                if let Some(zap_req_id) = zap_req_id {
                                    self.invoice_zap_reqs
                                        .insert(invoice.label.clone(), zap_req_id.id);
                                    let _ = ndb.process_event(&format!(
                                        "[\"EVENT\",\"a\",{}]",
                                        &invoice.description
                                    ));
                                }
                            }
                        }

                        self.invoices = LoadingState::from_result(invoices);
                    }
                },
            }
        }
    }
}

fn get_info_ui(ui: &mut egui::Ui, info: &LoadingState<String, lnsocket::Error>) {
    ui.horizontal_wrapped(|ui| match info {
        LoadingState::Loading => {}
        LoadingState::Failed(err) => {
            ui.label(format!("failed to fetch node info: {err}"));
        }
        LoadingState::Loaded(info) => {
            ui.add(Label::new(info).wrap_mode(egui::TextWrapMode::Wrap));
        }
    });
}

fn channel_ui(ui: &mut egui::Ui, c: &Channel, max_total_msat: i64) {
    // ---------- numbers ----------
    let short_channel_id = &c.original.short_channel_id;

    let cap_ratio = (c.original.total_msat as f32 / max_total_msat.max(1) as f32).clamp(0.0, 1.0);
    // Feel free to switch to log scaling if you have whales:
    //let cap_ratio = ((c.original.total_msat as f32 + 1.0).log10() / (max_total_msat as f32 + 1.0).log10()).clamp(0.0, 1.0);

    // ---------- colors & style ----------
    let out_color = Color32::from_rgb(84, 69, 201); // blue
    let in_color = Color32::from_rgb(158, 56, 180); // purple

    // Thickness scales with capacity, but keeps a nice minimum
    let thickness = 10.0 + cap_ratio * 22.0; // 10 → 32 px
    let row_h = thickness + 14.0;

    // ---------- layout ----------
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_h),
        egui::Sense::hover(),
    );
    let painter = ui.painter_at(rect);

    let bar_rect = egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.center().y - thickness * 0.5),
        egui::pos2(rect.right(), rect.center().y + thickness * 0.5),
    );
    let corner_radius = (thickness * 0.5) as u8;
    let out_radius = egui::CornerRadius {
        ne: 0,
        nw: corner_radius,
        sw: corner_radius,
        se: 0,
    };
    let in_radius = egui::CornerRadius {
        ne: corner_radius,
        nw: 0,
        sw: 0,
        se: corner_radius,
    };
    /*
    painter.rect_filled(bar_rect, rounding, track_color);
    painter.rect_stroke(bar_rect, rounding, track_stroke, egui::StrokeKind::Middle);
    */

    // Split widths
    let usable = (c.to_us + c.to_them).max(1) as f32;
    let out_w = (bar_rect.width() * (c.to_us as f32 / usable)).round();
    let split_x = bar_rect.left() + out_w;

    // Outbound fill (left)
    let out_rect = egui::Rect::from_min_max(bar_rect.min, egui::pos2(split_x, bar_rect.max.y));
    if out_rect.width() > 0.5 {
        painter.rect_filled(out_rect, out_radius, out_color);
    }

    // Inbound fill (right)
    let in_rect = egui::Rect::from_min_max(egui::pos2(split_x, bar_rect.min.y), bar_rect.max);
    if in_rect.width() > 0.5 {
        painter.rect_filled(in_rect, in_radius, in_color);
    }

    // Tooltip
    response.on_hover_text_at_pointer(format!(
        "Channel ID {short_channel_id}\nOutbound (ours): {} sats\nInbound (theirs): {} sats\nCapacity: {} sats",
        human_sat(c.to_us),
        human_sat(c.to_them),
        human_sat(c.original.total_msat),
    ));
}

// ---------- helper ----------
fn human_sat(msat: i64) -> String {
    let sats = msat / 1000;
    if sats >= 1_000_000 {
        format!("{:.1}M", sats as f64 / 1_000_000.0)
    } else if sats >= 1_000 {
        format!("{:.1}k", sats as f64 / 1_000.0)
    } else {
        sats.to_string()
    }
}

fn human_verbose_sat(msat: i64) -> String {
    if msat < 1_000 {
        // less than 1 sat
        format!("{msat} msat")
    } else {
        let sats = msat / 1_000;
        if sats < 100_000_000 {
            // less than 1 BTC
            format!("{sats} sat")
        } else {
            let btc = sats / 100_000_000;
            format!("{btc} BTC")
        }
    }
}

fn channels_ui(ui: &mut egui::Ui, channels: &LoadingState<Channels, lnsocket::Error>) {
    match channels {
        LoadingState::Loaded(channels) => {
            if channels.channels.is_empty() {
                ui.label("no channels yet...");
                return;
            }

            for channel in &channels.channels {
                channel_ui(ui, channel, channels.max_total_msat);
            }

            ui.label(format!("available out {}", human_sat(channels.avail_out)));
            ui.label(format!("available in {}", human_sat(channels.avail_in)));
        }
        LoadingState::Failed(err) => {
            ui.label(format!("error fetching channels: {err}"));
        }
        LoadingState::Loading => {
            ui.label("fetching channels...");
        }
    }
}

fn to_channels(peer_channels: Vec<ListPeerChannel>) -> Channels {
    let mut avail_out: i64 = 0;
    let mut avail_in: i64 = 0;
    let mut max_total_msat: i64 = 0;

    let mut channels: Vec<Channel> = peer_channels
        .into_iter()
        .map(|c| {
            let to_us = (c.to_us_msat - c.our_reserve_msat).max(0);
            let to_them_raw = (c.total_msat - c.to_us_msat).max(0);
            let to_them = (to_them_raw - c.their_reserve_msat).max(0);

            avail_out += to_us;
            avail_in += to_them;
            if c.total_msat > max_total_msat {
                max_total_msat = c.total_msat; // <-- max, not sum
            }

            Channel {
                to_us,
                to_them,
                original: c,
            }
        })
        .collect();

    channels.sort_by(|a, b| {
        let a_capacity = a.to_them + a.to_us;
        let b_capacity = b.to_them + b.to_us;

        a_capacity.partial_cmp(&b_capacity).unwrap().reverse()
    });

    Channels {
        max_total_msat,
        avail_out,
        avail_in,
        channels,
    }
}

fn summary_cards_ui(ui: &mut egui::Ui, s: &Summary, prev: Option<&Summary>) {
    let old = prev.cloned().unwrap_or_default();
    let items: [(&str, String, Option<String>); 6] = [
        (
            "Total capacity",
            human_sat(s.total_msat),
            prev.map(|_| delta_str(s.total_msat, old.total_msat)),
        ),
        (
            "Avail out",
            human_sat(s.avail_out_msat),
            prev.map(|_| delta_str(s.avail_out_msat, old.avail_out_msat)),
        ),
        (
            "Avail in",
            human_sat(s.avail_in_msat),
            prev.map(|_| delta_str(s.avail_in_msat, old.avail_in_msat)),
        ),
        ("# Channels", s.channel_count.to_string(), None),
        ("Largest", human_sat(s.largest_msat), None),
        (
            "Outbound %",
            format!("{:.0}%", s.outbound_pct * 100.0),
            None,
        ),
    ];

    // --- responsive columns ---
    let min_card = 160.0;
    let cols = ((ui.available_width() / min_card).floor() as usize).max(1);

    egui::Grid::new("summary_grid")
        .num_columns(cols)
        .min_col_width(min_card)
        .spacing(egui::vec2(8.0, 8.0))
        .show(ui, |ui| {
            let items_len = items.len();
            for (i, (t, v, d)) in items.into_iter().enumerate() {
                card_cell(ui, t, v, d, min_card);

                // End the row when we filled a row worth of cells
                if (i + 1) % cols == 0 {
                    ui.end_row();
                }
            }

            // If the last row wasn't full, close it anyway
            if items_len % cols != 0 {
                ui.end_row();
            }
        });
}

fn card_cell(ui: &mut egui::Ui, title: &str, value: String, delta: Option<String>, min_card: f32) {
    let weak = ui.visuals().weak_text_color();
    egui::Frame::group(ui.style())
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::same(10))
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .show(ui, |ui| {
            ui.set_min_width(min_card);
            ui.vertical(|ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(title).small().color(weak))
                        .wrap_mode(egui::TextWrapMode::Wrap),
                );
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(egui::RichText::new(value).strong().size(18.0))
                        .wrap_mode(egui::TextWrapMode::Wrap),
                );
                if let Some(d) = delta {
                    ui.add_space(2.0);
                    ui.add(
                        egui::Label::new(egui::RichText::new(d).small().color(weak))
                            .wrap_mode(egui::TextWrapMode::Wrap),
                    );
                }
            });
            ui.set_min_height(20.0);
        });
}

#[derive(Clone, Default)]
struct Summary {
    total_msat: i64,
    avail_out_msat: i64,
    avail_in_msat: i64,
    channel_count: usize,
    largest_msat: i64,
    outbound_pct: f32, // fraction of total capacity
}

fn compute_summary(ch: &Channels) -> Summary {
    let total_msat: i64 = ch.channels.iter().map(|c| c.original.total_msat).sum();
    let largest_msat: i64 = ch
        .channels
        .iter()
        .map(|c| c.original.total_msat)
        .max()
        .unwrap_or(0);
    let outbound_pct = if total_msat > 0 {
        ch.avail_out as f32 / total_msat as f32
    } else {
        0.0
    };

    Summary {
        total_msat,
        avail_out_msat: ch.avail_out,
        avail_in_msat: ch.avail_in,
        channel_count: ch.channels.len(),
        largest_msat,
        outbound_pct,
    }
}

fn delta_str(new: i64, old: i64) -> String {
    let d = new - old;
    match d.cmp(&0) {
        std::cmp::Ordering::Greater => format!("↑ {}", human_sat(d)),
        std::cmp::Ordering::Less => format!("↓ {}", human_sat(-d)),
        std::cmp::Ordering::Equal => "·".into(),
    }
}

fn invoices_ui(
    ui: &mut egui::Ui,
    invoice_notes: &HashMap<String, [u8; 32]>,
    ctx: &mut AppContext,
    invoices: &LoadingState<Vec<Invoice>, lnsocket::Error>,
) {
    match invoices {
        LoadingState::Loading => {
            ui.label("loading invoices...");
        }

        LoadingState::Failed(err) => {
            ui.label(format!("failed to load invoices: {err}"));
        }

        LoadingState::Loaded(invoices) => {
            use egui_extras::{Column, TableBuilder};

            TableBuilder::new(ui)
                .column(Column::auto().resizable(true))
                .column(Column::remainder())
                .vscroll(false)
                .header(20.0, |mut header| {
                    header.col(|ui| {
                        ui.strong("description");
                    });
                    header.col(|ui| {
                        ui.strong("amount");
                    });
                })
                .body(|mut body| {
                    for invoice in invoices {
                        body.row(20.0, |mut row| {
                            row.col(|ui| {
                                if invoice.description.starts_with("{") {
                                    ui.label("Zap!").on_hover_ui_at_pointer(|ui| {
                                        note_hover_ui(ui, &invoice.label, ctx, invoice_notes);
                                    });
                                } else {
                                    ui.label(&invoice.description);
                                }
                            });
                            row.col(|ui| match invoice.bolt11.amount_milli_satoshis() {
                                None => {
                                    ui.label("any");
                                }
                                Some(amt) => {
                                    ui.label(human_verbose_sat(amt as i64));
                                }
                            });
                        });
                    }
                });
        }
    }
}

fn note_hover_ui(
    ui: &mut egui::Ui,
    label: &str,
    ctx: &mut AppContext,
    invoice_notes: &HashMap<String, [u8; 32]>,
) -> Option<notedeck::NoteAction> {
    let zap_req_id = invoice_notes.get(label)?;

    let Ok(txn) = nostrdb::Transaction::new(ctx.ndb) else {
        return None;
    };

    let Ok(zapreq_note) = ctx.ndb.get_note_by_id(&txn, zap_req_id) else {
        return None;
    };

    for tag in zapreq_note.tags() {
        let Some("e") = tag.get_str(0) else {
            continue;
        };

        let Some(target_id) = tag.get_id(1) else {
            continue;
        };

        let Ok(note) = ctx.ndb.get_note_by_id(&txn, target_id) else {
            return None;
        };

        let author = ctx
            .ndb
            .get_profile_by_pubkey(&txn, zapreq_note.pubkey())
            .ok();

        // TODO(jb55): make this less horrible
        let mut note_context = notedeck::NoteContext {
            ndb: ctx.ndb,
            accounts: ctx.accounts,
            img_cache: ctx.img_cache,
            note_cache: ctx.note_cache,
            zaps: ctx.zaps,
            pool: ctx.pool,
            job_pool: ctx.job_pool,
            unknown_ids: ctx.unknown_ids,
            clipboard: ctx.clipboard,
            i18n: ctx.i18n,
            global_wallet: ctx.global_wallet,
        };

        let mut jobs = notedeck::JobsCache::default();
        let options = notedeck_ui::NoteOptions::default();

        notedeck_ui::ProfilePic::from_profile_or_default(note_context.img_cache, author.as_ref())
            .ui(ui);

        let nostr_name = notedeck::name::get_display_name(author.as_ref());
        ui.label(format!("{} zapped you", nostr_name.name()));

        return notedeck_ui::NoteView::new(&mut note_context, &note, options, &mut jobs)
            .preview_style()
            .hide_media(true)
            .show(ui)
            .action;
    }

    None
}
