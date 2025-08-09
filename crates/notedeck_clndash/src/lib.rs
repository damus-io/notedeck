use egui::{Color32, Label, RichText};
use lnsocket::bitcoin::secp256k1::{PublicKey, SecretKey, rand};
use lnsocket::{CommandoClient, LNSocket};
use notedeck::{AppAction, AppContext};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::str::FromStr;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

struct Channel {
    to_us: i64,
    to_them: i64,
    original: ListPeerChannel,
}

struct Channels {
    max_total_msat: i64,
    avail_in: i64,
    avail_out: i64,
    channels: Vec<Channel>,
}

#[derive(Default)]
pub struct ClnDash {
    initialized: bool,
    connection_state: ConnectionState,
    get_info: Option<String>,
    channels: Option<Channels>,
    channel: Option<CommChannel>,
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

/// Responses from the socket
enum ClnResponse {
    GetInfo(Value),
    ListPeerChannels(Channels),
}

#[derive(Deserialize, Serialize)]
struct ListPeerChannel {
    short_channel_id: String,
    our_reserve_msat: i64,
    to_us_msat: i64,
    total_msat: i64,
    their_reserve_msat: i64,
}

enum ConnectionState {
    Dead(String),
    Connecting,
    Active,
}

#[derive(Eq, PartialEq, Clone, Debug)]
enum Request {
    GetInfo,
    ListPeerChannels,
}

enum Event {
    /// We lost the socket somehow
    Ended {
        reason: String,
    },

    Connected,

    Response(ClnResponse),
}

impl notedeck::App for ClnDash {
    fn update(&mut self, _ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> Option<AppAction> {
        if !self.initialized {
            self.connection_state = ConnectionState::Connecting;
            self.setup_connection();
            self.initialized = true;
        }

        self.process_events();

        self.show(ui);

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

impl ClnDash {
    fn show(&mut self, ui: &mut egui::Ui) {
        egui::Frame::new()
            .inner_margin(egui::Margin::same(20))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    connection_state_ui(ui, &self.connection_state);
                    channels_ui(ui, &self.channels);

                    if let Some(info) = self.get_info.as_ref() {
                        get_info_ui(ui, info);
                    }
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
            let commando = CommandoClient::spawn(lnsocket, &rune);

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
                            Request::GetInfo => match commando.call("getinfo", json!({})).await {
                                Ok(v) => {
                                    let _ = event_tx.send(Event::Response(ClnResponse::GetInfo(v)));
                                }
                                Err(err) => {
                                    tracing::error!("get_info error {}", err);
                                }
                            },

                            Request::ListPeerChannels => {
                                match commando.call("listpeerchannels", json!({})).await {
                                    Ok(v) => {
                                        let peer_channels: Vec<ListPeerChannel> =
                                            serde_json::from_value(v["channels"].clone()).unwrap();
                                        let _ = event_tx.send(Event::Response(
                                            ClnResponse::ListPeerChannels(to_channels(
                                                peer_channels,
                                            )),
                                        ));
                                    }
                                    Err(err) => {
                                        tracing::error!("listpeerchannels error {}", err);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    fn process_events(&mut self) {
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
                }

                Event::Response(resp) => match resp {
                    ClnResponse::ListPeerChannels(chans) => {
                        self.channels = Some(chans);
                    }

                    ClnResponse::GetInfo(value) => {
                        if let Ok(s) = serde_json::to_string_pretty(&value) {
                            self.get_info = Some(s);
                        }
                    }
                },
            }
        }
    }
}

fn get_info_ui(ui: &mut egui::Ui, info: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.add(Label::new(info).wrap_mode(egui::TextWrapMode::Wrap));
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

fn channels_ui(ui: &mut egui::Ui, channels: &Option<Channels>) {
    let Some(channels) = channels else {
        ui.label("no channels");
        return;
    };

    for channel in &channels.channels {
        channel_ui(ui, channel, channels.max_total_msat);
    }

    ui.label(format!("available out {}", human_sat(channels.avail_out)));
    ui.label(format!("available in {}", human_sat(channels.avail_in)));
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
