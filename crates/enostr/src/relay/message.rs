use crate::{Error, Result};
use ewebsock::{WsEvent, WsMessage};

#[derive(Debug, Eq, PartialEq)]
pub struct CommandResult<'a> {
    event_id: &'a str,
    status: bool,
    message: &'a str,
}

#[derive(Debug, Eq, PartialEq)]
pub enum RelayMessage<'a> {
    OK(CommandResult<'a>),
    Eose(&'a str),
    Event(&'a str, &'a str),
    Notice(&'a str),
}

#[derive(Debug)]
pub enum RelayEvent<'a> {
    Opened,
    Closed,
    Other(&'a WsMessage),
    Error(Error),
    Message(RelayMessage<'a>),
}

impl<'a> From<&'a WsEvent> for RelayEvent<'a> {
    fn from(event: &'a WsEvent) -> RelayEvent<'a> {
        match event {
            WsEvent::Opened => RelayEvent::Opened,
            WsEvent::Closed => RelayEvent::Closed,
            WsEvent::Message(ref ws_msg) => ws_msg.into(),
            WsEvent::Error(s) => RelayEvent::Error(Error::Generic(s.to_owned())),
        }
    }
}

impl<'a> From<&'a WsMessage> for RelayEvent<'a> {
    fn from(wsmsg: &'a WsMessage) -> RelayEvent<'a> {
        match wsmsg {
            WsMessage::Text(s) => match RelayMessage::from_json(s).map(RelayEvent::Message) {
                Ok(msg) => msg,
                Err(err) => RelayEvent::Error(err),
            },
            wsmsg => RelayEvent::Other(wsmsg),
        }
    }
}

impl<'a> RelayMessage<'a> {
    pub fn eose(subid: &'a str) -> Self {
        RelayMessage::Eose(subid)
    }

    pub fn notice(msg: &'a str) -> Self {
        RelayMessage::Notice(msg)
    }

    pub fn ok(event_id: &'a str, status: bool, message: &'a str) -> Self {
        RelayMessage::OK(CommandResult {
            event_id,
            status,
            message,
        })
    }

    pub fn event(ev: &'a str, sub_id: &'a str) -> Self {
        RelayMessage::Event(sub_id, ev)
    }

    pub fn from_json(msg: &'a str) -> Result<RelayMessage<'a>> {
        if msg.is_empty() {
            return Err(Error::Empty);
        }

        // Notice
        // Relay response format: ["NOTICE", <message>]
        if msg.len() >= 12 && &msg[0..=9] == "[\"NOTICE\"," {
            // TODO: there could be more than one space, whatever
            let start = if msg.as_bytes().get(10).copied() == Some(b' ') {
                12
            } else {
                11
            };
            let end = msg.len() - 2;
            return Ok(Self::notice(&msg[start..end]));
        }

        // Event
        // Relay response format: ["EVENT", <subscription id>, <event JSON>]
        if &msg[0..=7] == "[\"EVENT\"" {
            let mut start = 9;
            while let Some(&b' ') = msg.as_bytes().get(start) {
                start += 1; // Move past optional spaces
            }
            if let Some(comma_index) = msg[start..].find(',') {
                let subid_end = start + comma_index;
                let subid = &msg[start..subid_end].trim().trim_matches('"');
                return Ok(Self::event(msg, subid));
            } else {
                return Ok(Self::event(msg, "fixme"));
            }
        }

        // EOSE (NIP-15)
        // Relay response format: ["EOSE", <subscription_id>]
        if &msg[0..=7] == "[\"EOSE\"," {
            let start = if msg.as_bytes().get(8).copied() == Some(b' ') {
                10
            } else {
                9
            };
            let end = msg.len() - 2;
            return Ok(Self::eose(&msg[start..end]));
        }

        // OK (NIP-20)
        // Relay response format: ["OK",<event_id>, <true|false>, <message>]
        if &msg[0..=5] == "[\"OK\"," && msg.len() >= 78 {
            // TODO: fix this
            let event_id = &msg[7..71];
            let booly = &msg[73..77];
            let status: bool = if booly == "true" {
                true
            } else if booly == "false" {
                false
            } else {
                return Err(Error::DecodeFailed);
            };

            return Ok(Self::ok(event_id, status, "fixme"));
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
        let handled_valid_notice_msg = RelayMessage::notice("Invalid event format!");

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

        assert!(matches!(
            RelayMessage::from_json(invalid_notice_msg).unwrap_err(),
            Error::DecodeFailed
        ));
        assert!(matches!(
            RelayMessage::from_json(invalid_notice_msg_content).unwrap_err(),
            Error::DecodeFailed
        ));
    }

    /*
    #[test]
    fn test_handle_valid_event() -> Result<()> {
        use tracing::debug;

        let valid_event_msg = r#"["EVENT", "random_string", {"id":"70b10f70c1318967eddf12527799411b1a9780ad9c43858f5e5fcd45486a13a5","pubkey":"379e863e8357163b5bce5d2688dc4f1dcc2d505222fb8d74db600f30535dfdfe","created_at":1612809991,"kind":1,"tags":[],"content":"test","sig":"273a9cd5d11455590f4359500bccb7a89428262b96b3ea87a756b770964472f8c3e87f5d5e64d8d2e859a71462a3f477b554565c4f2f326cb01dd7620db71502"}]"#;

        let id = "70b10f70c1318967eddf12527799411b1a9780ad9c43858f5e5fcd45486a13a5";
        let pubkey = "379e863e8357163b5bce5d2688dc4f1dcc2d505222fb8d74db600f30535dfdfe";
        let created_at = 1612809991;
        let kind = 1;
        let tags = vec![];
        let content = "test";
        let sig = "273a9cd5d11455590f4359500bccb7a89428262b96b3ea87a756b770964472f8c3e87f5d5e64d8d2e859a71462a3f477b554565c4f2f326cb01dd7620db71502";

        let handled_event = Note::new_dummy(id, pubkey, created_at, kind, tags, content, sig).expect("ev");
        debug!("event {:?}", handled_event);

        let msg = RelayMessage::from_json(valid_event_msg).expect("valid json");
        debug!("msg {:?}", msg);

        let note_json = serde_json::to_string(&handled_event).expect("json ev");

        assert_eq!(
            msg,
            RelayMessage::event(&note_json, "random_string")
        );

        Ok(())
    }

    #[test]
    fn test_handle_invalid_event() {
        //Mising Event field
        let invalid_event_msg = r#"["EVENT","random_string"]"#;
        //Event JSON with incomplete content
        let invalid_event_msg_content = r#"["EVENT","random_string",{"id":"70b10f70c1318967eddf12527799411b1a9780ad9c43858f5e5fcd45486a13a5","pubkey":"379e863e8357163b5bce5d2688dc4f1dcc2d505222fb8d74db600f30535dfdfe"}]"#;

        assert!(matches!(
            RelayMessage::from_json(invalid_event_msg).unwrap_err(),
            Error::DecodeFailed
        ));

        assert!(matches!(
            RelayMessage::from_json(invalid_event_msg_content).unwrap_err(),
            Error::DecodeFailed
        ));
    }
    */

    #[test]
    fn test_handle_valid_eose() -> Result<()> {
        let valid_eose_msg = r#"["EOSE","random-subscription-id"]"#;
        let handled_valid_eose_msg = RelayMessage::eose("random-subscription-id");

        assert_eq!(
            RelayMessage::from_json(valid_eose_msg)?,
            handled_valid_eose_msg
        );

        Ok(())
    }

    // TODO: fix these tests
    /*
    #[test]
    fn test_handle_invalid_eose() {
        // Missing subscription ID
        assert!(matches!(
            RelayMessage::from_json(r#"["EOSE"]"#).unwrap_err(),
            Error::DecodeFailed
        ));

        // The subscription ID is not string
        assert!(matches!(
            RelayMessage::from_json(r#"["EOSE",404]"#).unwrap_err(),
            Error::DecodeFailed
        ));
    }

    #[test]
    fn test_handle_valid_ok() -> Result<()> {
        let valid_ok_msg = r#"["OK","b1a649ebe8b435ec71d3784793f3bbf4b93e64e17568a741aecd4c7ddeafce30",true,"pow: difficulty 25>=24"]"#;
        let handled_valid_ok_msg = RelayMessage::ok(
            "b1a649ebe8b435ec71d3784793f3bbf4b93e64e17568a741aecd4c7ddeafce30",
            true,
            "pow: difficulty 25>=24".into(),
        );

        assert_eq!(RelayMessage::from_json(valid_ok_msg)?, handled_valid_ok_msg);

        Ok(())
    }
    */

    #[test]
    fn test_handle_invalid_ok() {
        // Missing params
        assert!(matches!(
            RelayMessage::from_json(
                r#"["OK","b1a649ebe8b435ec71d3784793f3bbf4b93e64e17568a741aecd4c7ddeafce30"]"#
            )
            .unwrap_err(),
            Error::DecodeFailed
        ));

        // Invalid status
        assert!(
            matches!(RelayMessage::from_json(r#"["OK","b1a649ebe8b435ec71d3784793f3bbf4b93e64e17568a741aecd4c7ddeafce30",hello,""]"#).unwrap_err(),
            Error::DecodeFailed)
        );

        // Invalid message
        assert!(
            matches!(RelayMessage::from_json(r#"["OK","b1a649ebe8b435ec71d3784793f3bbf4b93e64e17568a741aecd4c7ddeafce30",hello,404]"#).unwrap_err(),
            Error::DecodeFailed)
        );
    }
}
