use crate::Error;
use crate::Event;
use crate::Result;
use log::debug;
use serde_json::Value;

use ewebsock::{WsEvent, WsMessage};

#[derive(Debug, Eq, PartialEq)]
pub struct CommandResult {
    event_id: String,
    status: bool,
    message: String,
}

#[derive(Debug, Eq, PartialEq)]
pub enum RelayMessage {
    OK(CommandResult),
    Eose(String),
    Event(String, Event),
    Notice(String),
}

#[derive(Debug)]
pub enum RelayEvent {
    Opened,
    Closed,
    Other(WsMessage),
    Message(RelayMessage),
}

impl TryFrom<WsEvent> for RelayEvent {
    type Error = Error;

    fn try_from(message: WsEvent) -> Result<Self> {
        match message {
            WsEvent::Opened => Ok(RelayEvent::Opened),
            WsEvent::Closed => Ok(RelayEvent::Closed),
            WsEvent::Message(ws_msg) => ws_msg.try_into(),
            WsEvent::Error(s) => Err(s.into()),
        }
    }
}

impl TryFrom<WsMessage> for RelayEvent {
    type Error = Error;

    fn try_from(wsmsg: WsMessage) -> Result<Self> {
        match wsmsg {
            WsMessage::Text(s) => RelayMessage::from_json(&s).map(RelayEvent::Message),
            wsmsg => Ok(RelayEvent::Other(wsmsg)),
        }
    }
}

impl RelayMessage {
    pub fn eose(subid: String) -> Self {
        RelayMessage::Eose(subid)
    }

    pub fn notice(msg: String) -> Self {
        RelayMessage::Notice(msg)
    }

    pub fn ok(event_id: String, status: bool, message: String) -> Self {
        RelayMessage::OK(CommandResult {
            event_id: event_id,
            status,
            message: message,
        })
    }

    pub fn event(ev: Event, sub_id: String) -> Self {
        RelayMessage::Event(sub_id, ev)
    }

    // I was lazy and took this from the nostr crate. thx yuki!
    pub fn from_json(msg: &str) -> Result<Self> {
        if msg.is_empty() {
            return Err(Error::Empty);
        }

        let v: Vec<Value> = serde_json::from_str(msg).map_err(|_| Error::DecodeFailed)?;

        // Notice
        // Relay response format: ["NOTICE", <message>]
        if v[0] == "NOTICE" {
            if v.len() != 2 {
                return Err(Error::DecodeFailed);
            }
            let v_notice: String =
                serde_json::from_value(v[1].clone()).map_err(|_| Error::DecodeFailed)?;
            return Ok(Self::notice(v_notice));
        }

        // Event
        // Relay response format: ["EVENT", <subscription id>, <event JSON>]
        if v[0] == "EVENT" {
            if v.len() != 3 {
                return Err(Error::DecodeFailed);
            }

            let event = Event::from_json(&v[2].to_string()).map_err(|_| Error::DecodeFailed)?;

            let subscription_id: String =
                serde_json::from_value(v[1].clone()).map_err(|_| Error::DecodeFailed)?;

            return Ok(Self::event(event, subscription_id));
        }

        // EOSE (NIP-15)
        // Relay response format: ["EOSE", <subscription_id>]
        if v[0] == "EOSE" {
            if v.len() != 2 {
                return Err(Error::DecodeFailed);
            }

            let subscription_id: String =
                serde_json::from_value(v[1].clone()).map_err(|_| Error::DecodeFailed)?;

            return Ok(Self::eose(subscription_id));
        }

        // OK (NIP-20)
        // Relay response format: ["OK", <event_id>, <true|false>, <message>]
        if v[0] == "OK" {
            if v.len() != 4 {
                return Err(Error::DecodeFailed);
            }

            let event_id: String =
                serde_json::from_value(v[1].clone()).map_err(|_| Error::DecodeFailed)?;

            let status: bool =
                serde_json::from_value(v[2].clone()).map_err(|_| Error::DecodeFailed)?;

            let message: String =
                serde_json::from_value(v[3].clone()).map_err(|_| Error::DecodeFailed)?;

            return Ok(Self::ok(event_id, status, message));
        }

        Err(Error::DecodeFailed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_valid_notice() -> Result<()> {
        let valid_notice_msg = r#"["NOTICE","Invalid event format!"]"#;
        let handled_valid_notice_msg = RelayMessage::notice("Invalid event format!".to_string());

        assert_eq!(
            RelayMessage::from_json(valid_notice_msg)?,
            handled_valid_notice_msg
        );

        Ok(())
    }
    #[test]
    fn test_handle_invalid_notice() {
        //Missing content
        let invalid_notice_msg = r#"["NOTICE"]"#;
        //The content is not string
        let invalid_notice_msg_content = r#"["NOTICE": 404]"#;

        assert_eq!(
            RelayMessage::from_json(invalid_notice_msg).unwrap_err(),
            Error::DecodeFailed
        );
        assert_eq!(
            RelayMessage::from_json(invalid_notice_msg_content).unwrap_err(),
            Error::DecodeFailed
        );
    }

    #[test]
    fn test_handle_valid_event() -> Result<()> {
        use log::debug;

        env_logger::init();
        let valid_event_msg = r#"["EVENT", "random_string", {"id":"70b10f70c1318967eddf12527799411b1a9780ad9c43858f5e5fcd45486a13a5","pubkey":"379e863e8357163b5bce5d2688dc4f1dcc2d505222fb8d74db600f30535dfdfe","created_at":1612809991,"kind":1,"tags":[],"content":"test","sig":"273a9cd5d11455590f4359500bccb7a89428262b96b3ea87a756b770964472f8c3e87f5d5e64d8d2e859a71462a3f477b554565c4f2f326cb01dd7620db71502"}]"#;

        let id = "70b10f70c1318967eddf12527799411b1a9780ad9c43858f5e5fcd45486a13a5";
        let pubkey = "379e863e8357163b5bce5d2688dc4f1dcc2d505222fb8d74db600f30535dfdfe";
        let created_at = 1612809991;
        let kind = 1;
        let tags = vec![];
        let content = "test";
        let sig = "273a9cd5d11455590f4359500bccb7a89428262b96b3ea87a756b770964472f8c3e87f5d5e64d8d2e859a71462a3f477b554565c4f2f326cb01dd7620db71502";

        let handled_event = Event::new_dummy(id, pubkey, created_at, kind, tags, content, sig);
        debug!("event {:?}", handled_event);

        let msg = RelayMessage::from_json(valid_event_msg);
        debug!("msg {:?}", msg);

        assert_eq!(
            msg?,
            RelayMessage::event(handled_event?, "random_string".to_string())
        );

        Ok(())
    }

    #[test]
    fn test_handle_invalid_event() {
        //Mising Event field
        let invalid_event_msg = r#"["EVENT", "random_string"]"#;
        //Event JSON with incomplete content
        let invalid_event_msg_content = r#"["EVENT", "random_string", {"id":"70b10f70c1318967eddf12527799411b1a9780ad9c43858f5e5fcd45486a13a5","pubkey":"379e863e8357163b5bce5d2688dc4f1dcc2d505222fb8d74db600f30535dfdfe"}]"#;

        assert_eq!(
            RelayMessage::from_json(invalid_event_msg).unwrap_err(),
            Error::DecodeFailed
        );

        assert_eq!(
            RelayMessage::from_json(invalid_event_msg_content).unwrap_err(),
            Error::DecodeFailed
        );
    }

    #[test]
    fn test_handle_valid_eose() -> Result<()> {
        let valid_eose_msg = r#"["EOSE","random-subscription-id"]"#;
        let handled_valid_eose_msg = RelayMessage::eose("random-subscription-id".to_string());

        assert_eq!(
            RelayMessage::from_json(valid_eose_msg)?,
            handled_valid_eose_msg
        );

        Ok(())
    }
    #[test]
    fn test_handle_invalid_eose() {
        // Missing subscription ID
        assert_eq!(
            RelayMessage::from_json(r#"["EOSE"]"#).unwrap_err(),
            Error::DecodeFailed
        );

        // The subscription ID is not string
        assert_eq!(
            RelayMessage::from_json(r#"["EOSE", 404]"#).unwrap_err(),
            Error::DecodeFailed
        );
    }

    #[test]
    fn test_handle_valid_ok() -> Result<()> {
        let valid_ok_msg = r#"["OK", "b1a649ebe8b435ec71d3784793f3bbf4b93e64e17568a741aecd4c7ddeafce30", true, "pow: difficulty 25>=24"]"#;
        let handled_valid_ok_msg = RelayMessage::ok(
            "b1a649ebe8b435ec71d3784793f3bbf4b93e64e17568a741aecd4c7ddeafce30".to_string(),
            true,
            "pow: difficulty 25>=24".into(),
        );

        assert_eq!(RelayMessage::from_json(valid_ok_msg)?, handled_valid_ok_msg);

        Ok(())
    }
    #[test]
    fn test_handle_invalid_ok() {
        // Missing params
        assert_eq!(
            RelayMessage::from_json(
                r#"["OK", "b1a649ebe8b435ec71d3784793f3bbf4b93e64e17568a741aecd4c7ddeafce30"]"#
            )
            .unwrap_err(),
            Error::DecodeFailed
        );

        // Invalid status
        assert_eq!(
            RelayMessage::from_json(r#"["OK", "b1a649ebe8b435ec71d3784793f3bbf4b93e64e17568a741aecd4c7ddeafce30", hello, ""]"#).unwrap_err(),
            Error::DecodeFailed
        );

        // Invalid message
        assert_eq!(
            RelayMessage::from_json(r#"["OK", "b1a649ebe8b435ec71d3784793f3bbf4b93e64e17568a741aecd4c7ddeafce30", hello, 404]"#).unwrap_err(),
            Error::DecodeFailed
        );
    }
}
