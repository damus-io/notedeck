use crate::channels::Channel;
use crate::channels::Channels;
use crate::channels::ListPeerChannel;
use crate::event::ClnResponse;
use crate::event::ConnectionState;
use crate::event::Event;
use crate::event::LoadingState;
use crate::event::Request;
use crate::invoice::Invoice;
use crate::summary::Summary;
use crate::watch::fetch_paid_invoices;

use lnsocket::bitcoin::secp256k1::{PublicKey, SecretKey, rand};
use lnsocket::{CommandoClient, LNSocket};
use nostrdb::Ndb;
use notedeck::{AppAction, AppContext};
use serde_json::json;
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

mod channels;
mod event;
mod invoice;
mod summary;
mod ui;
mod watch;

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

impl ClnDash {
    fn show(&mut self, ui: &mut egui::Ui, ctx: &mut AppContext) {
        egui::Frame::new()
            .inner_margin(egui::Margin::same(20))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui::connection_state_ui(ui, &self.connection_state);
                    crate::summary::summary_ui(ui, self.last_summary.as_ref(), &self.summary);
                    crate::invoice::invoices_ui(ui, &self.invoice_zap_reqs, ctx, &self.invoices);
                    crate::channels::channels_ui(ui, &self.channels);
                    crate::ui::get_info_ui(ui, &self.get_info);
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
                            self.last_summary = Some(crate::summary::compute_summary(prev));
                        }

                        self.summary = match &chans {
                            Ok(chans) => {
                                LoadingState::Loaded(crate::summary::compute_summary(chans))
                            }
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
