use std::{collections::HashMap, mem, time::SystemTime};

use ewebsock::WsMessage;
use nostrdb::Filter;

use crate::{ClientMessage, Error, RelayEvent, RelayMessage};

use super::message::calculate_command_result_size;

type RelayId = String;
type SubId = String;

pub struct SubsDebug {
    data: HashMap<RelayId, RelayStats>,
    time_incd: SystemTime,
    pub relay_events_selection: Option<RelayId>,
}

#[derive(Default)]
pub struct RelayStats {
    pub count: TransferStats,
    pub events: Vec<RelayLogEvent>,
    pub sub_data: HashMap<SubId, SubStats>,
}

#[derive(Clone)]
pub enum RelayLogEvent {
    Send(ClientMessage),
    Recieve(OwnedRelayEvent),
}

#[derive(Clone)]
pub enum OwnedRelayEvent {
    Opened,
    Closed,
    Other(String),
    Error(String),
    Message(String),
}

impl From<RelayEvent<'_>> for OwnedRelayEvent {
    fn from(value: RelayEvent<'_>) -> Self {
        match value {
            RelayEvent::Opened => OwnedRelayEvent::Opened,
            RelayEvent::Closed => OwnedRelayEvent::Closed,
            RelayEvent::Other(ws_message) => {
                let ws_str = match ws_message {
                    WsMessage::Binary(_) => "Binary".to_owned(),
                    WsMessage::Text(t) => format!("Text:{t}"),
                    WsMessage::Unknown(u) => format!("Unknown:{u}"),
                    WsMessage::Ping(_) => "Ping".to_owned(),
                    WsMessage::Pong(_) => "Pong".to_owned(),
                };
                OwnedRelayEvent::Other(ws_str)
            }
            RelayEvent::Error(error) => OwnedRelayEvent::Error(error.to_string()),
            RelayEvent::Message(relay_message) => {
                let relay_msg = match relay_message {
                    RelayMessage::OK(_) => "OK".to_owned(),
                    RelayMessage::Eose(s) => format!("EOSE:{s}"),
                    RelayMessage::Event(_, s) => format!("EVENT:{s}"),
                    RelayMessage::Notice(s) => format!("NOTICE:{s}"),
                    RelayMessage::Closed(sub_id, message) => {
                        format!("CLOSED:{sub_id}:{message}")
                    }
                };
                OwnedRelayEvent::Message(relay_msg)
            }
        }
    }
}

#[derive(PartialEq, Eq, Hash, Clone)]
pub struct _RelaySub {
    pub(crate) subid: String,
    pub(crate) filter: String,
}

#[derive(Default)]
pub struct SubStats {
    pub filter: String,
    pub count: TransferStats,
}

#[derive(Default)]
pub struct TransferStats {
    pub up_total: usize,
    pub down_total: usize,

    // 1 sec < last tick < 2 sec
    pub up_sec_prior: usize,
    pub down_sec_prior: usize,

    // < 1 sec since last tick
    up_sec_cur: usize,
    down_sec_cur: usize,
}

impl Default for SubsDebug {
    fn default() -> Self {
        Self {
            data: Default::default(),
            time_incd: SystemTime::now(),
            relay_events_selection: None,
        }
    }
}

impl SubsDebug {
    pub fn get_data(&self) -> &HashMap<RelayId, RelayStats> {
        &self.data
    }

    pub(crate) fn send_cmd(&mut self, relay: String, cmd: &ClientMessage) {
        let data = self.data.entry(relay).or_default();
        let msg_num_bytes = calculate_client_message_size(cmd);
        match cmd {
            ClientMessage::Req { sub_id, filters } => {
                data.sub_data.insert(
                    sub_id.to_string(),
                    SubStats {
                        filter: filters_to_string(filters),
                        count: Default::default(),
                    },
                );
            }

            ClientMessage::Close { sub_id } => {
                data.sub_data.remove(sub_id);
            }

            _ => {}
        }

        data.count.up_sec_cur += msg_num_bytes;

        data.events.push(RelayLogEvent::Send(cmd.clone()));
    }

    pub(crate) fn receive_cmd(&mut self, relay: String, cmd: RelayEvent) {
        let data = self.data.entry(relay).or_default();
        let msg_num_bytes = calculate_relay_event_size(&cmd);
        if let RelayEvent::Message(RelayMessage::Event(sid, _)) = cmd {
            if let Some(sub_data) = data.sub_data.get_mut(sid) {
                let c = &mut sub_data.count;
                c.down_sec_cur += msg_num_bytes;
            }
        };

        data.count.down_sec_cur += msg_num_bytes;

        data.events.push(RelayLogEvent::Recieve(cmd.into()));
    }

    pub fn try_increment_stats(&mut self) {
        let cur_time = SystemTime::now();
        if let Ok(dur) = cur_time.duration_since(self.time_incd) {
            if dur.as_secs() >= 1 {
                self.time_incd = cur_time;
                self.internal_inc_stats();
            }
        }
    }

    fn internal_inc_stats(&mut self) {
        for relay_data in self.data.values_mut() {
            let c = &mut relay_data.count;
            inc_data_count(c);

            for sub in relay_data.sub_data.values_mut() {
                inc_data_count(&mut sub.count);
            }
        }
    }
}

fn inc_data_count(c: &mut TransferStats) {
    c.up_total += c.up_sec_cur;
    c.up_sec_prior = c.up_sec_cur;

    c.down_total += c.down_sec_cur;
    c.down_sec_prior = c.down_sec_cur;

    c.up_sec_cur = 0;
    c.down_sec_cur = 0;
}

fn calculate_client_message_size(message: &ClientMessage) -> usize {
    match message {
        ClientMessage::Event(note) => note.note_json.len() + 10, // 10 is ["EVENT",]
        ClientMessage::Req { sub_id, filters } => {
            mem::size_of_val(message)
                + mem::size_of_val(sub_id)
                + sub_id.len()
                + filters.iter().map(mem::size_of_val).sum::<usize>()
        }
        ClientMessage::Close { sub_id } => {
            mem::size_of_val(message) + mem::size_of_val(sub_id) + sub_id.len()
        }
        ClientMessage::Raw(data) => mem::size_of_val(message) + data.len(),
    }
}

fn calculate_relay_event_size(event: &RelayEvent<'_>) -> usize {
    let base_size = mem::size_of_val(event); // Size of the enum on the stack

    let variant_size = match event {
        RelayEvent::Opened | RelayEvent::Closed => 0, // No additional data
        RelayEvent::Other(ws_message) => calculate_ws_message_size(ws_message),
        RelayEvent::Error(error) => calculate_error_size(error),
        RelayEvent::Message(message) => calculate_relay_message_size(message),
    };

    base_size + variant_size
}

fn calculate_ws_message_size(message: &WsMessage) -> usize {
    match message {
        WsMessage::Binary(vec) | WsMessage::Ping(vec) | WsMessage::Pong(vec) => {
            mem::size_of_val(message) + vec.len()
        }
        WsMessage::Text(string) | WsMessage::Unknown(string) => {
            mem::size_of_val(message) + string.len()
        }
    }
}

fn calculate_error_size(error: &Error) -> usize {
    match error {
        Error::Empty
        | Error::HexDecodeFailed
        | Error::InvalidBech32
        | Error::InvalidByteSize
        | Error::InvalidSignature
        | Error::InvalidRelayUrl
        | Error::Io(_)
        | Error::InvalidPublicKey => mem::size_of_val(error), // No heap usage

        Error::DecodeFailed(string) => mem::size_of_val(error) + string.len(),

        Error::Json(json_err) => mem::size_of_val(error) + json_err.to_string().len(),

        Error::Nostrdb(nostrdb_err) => mem::size_of_val(error) + nostrdb_err.to_string().len(),

        Error::Generic(string) => mem::size_of_val(error) + string.len(),
    }
}

fn calculate_relay_message_size(message: &RelayMessage) -> usize {
    match message {
        RelayMessage::OK(result) => calculate_command_result_size(result),
        RelayMessage::Eose(str_ref)
        | RelayMessage::Event(str_ref, _)
        | RelayMessage::Notice(str_ref) => mem::size_of_val(message) + str_ref.len(),
        RelayMessage::Closed(sub_id, reason) => {
            mem::size_of_val(message) + sub_id.len() + reason.len()
        }
    }
}

fn filters_to_string(f: &Vec<Filter>) -> String {
    let mut cur_str = String::new();
    for filter in f {
        if let Ok(json) = filter.json() {
            if !cur_str.is_empty() {
                cur_str.push_str(", ");
            }
            cur_str.push_str(&json);
        }
    }

    cur_str
}
