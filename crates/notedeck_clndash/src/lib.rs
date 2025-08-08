use egui::{Color32, Label, RichText};
use lnsocket::bitcoin::secp256k1::{PublicKey, SecretKey, rand};
use lnsocket::{CommandoClient, LNSocket};
use notedeck::{AppAction, AppContext};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::str::FromStr;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

type JsonCache = HashMap<String, String>;

#[derive(Default)]
pub struct ClnDash {
    initialized: bool,
    connection_state: ConnectionState,
    get_info: Option<String>,
    peer_channels: Option<Vec<Value>>,
    json_cache: JsonCache,
    channel: Option<Channel>,
}

impl Default for ConnectionState {
    fn default() -> Self {
        ConnectionState::Dead("uninitialized".to_string())
    }
}

struct Channel {
    req_tx: UnboundedSender<Request>,
    event_rx: UnboundedReceiver<Event>,
}

/// Responses from the socket
enum ClnResponse {
    GetInfo(Value),
    ListPeerChannels(Value),
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
            .inner_margin(egui::Margin::same(50))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    connection_state_ui(ui, &self.connection_state);

                    channels_ui(ui, &mut self.json_cache, &self.peer_channels);

                    if let Some(info) = self.get_info.as_ref() {
                        get_info_ui(ui, info);
                    }
                });
            });
    }

    fn setup_connection(&mut self) {
        let (req_tx, mut req_rx) = unbounded_channel::<Request>();
        let (event_tx, event_rx) = unbounded_channel::<Event>();
        self.channel = Some(Channel { req_tx, event_rx });

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
                                        let _ = event_tx.send(Event::Response(
                                            ClnResponse::ListPeerChannels(v),
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
                        if let Some(vs) = chans["channels"].as_array() {
                            self.peer_channels = Some(vs.to_owned());
                        }
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

fn channel_ui(ui: &mut egui::Ui, cache: &mut JsonCache, channel: &Value) {
    let short_channel_id = channel["short_channel_id"].as_str().unwrap_or("??");

    egui::CollapsingHeader::new(format!("channel {short_channel_id}"))
        .id_salt(("section", short_channel_id))
        .show(ui, |ui| {
            let json: &String = cache
                .entry(short_channel_id.to_owned())
                .or_insert_with(|| serde_json::to_string_pretty(channel).unwrap());

            ui.add(Label::new(json).wrap_mode(egui::TextWrapMode::Wrap));
        });
}

fn channels_ui(ui: &mut egui::Ui, json_cache: &mut JsonCache, channels: &Option<Vec<Value>>) {
    let Some(channels) = channels else {
        ui.label("no channels");
        return;
    };

    for channel in channels {
        channel_ui(ui, json_cache, channel);
    }
}
