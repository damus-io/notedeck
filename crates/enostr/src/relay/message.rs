use crate::{Error, Result};
use ewebsock::{WsEvent, WsMessage};

#[derive(Debug, Eq, PartialEq)]
pub struct CommandResult<'a> {
    event_id: &'a str,
    status: bool,
    message: &'a str,
}

pub fn calculate_command_result_size(result: &CommandResult) -> usize {
    std::mem::size_of_val(result) + result.event_id.len() + result.message.len()
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

        // make sure we can inspect the begning of the message below ...
        if msg.len() < 12 {
            return Err(Error::DecodeFailed("message too short".into()));
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
                return Err(Error::DecodeFailed("Invalid EVENT format".into()));
            }
        }

        // EOSE (NIP-15)
        // Relay response format: ["EOSE", <subscription_id>]
        if &msg[0..=7] == "[\"EOSE\"," {
            let start = if msg.as_bytes().get(8).copied() == Some(b' ') {
                10 // Skip space after the comma
            } else {
                9 // Start immediately after the comma
            };

            // Use rfind to locate the last quote
            if let Some(end_bracket_index) = msg.rfind(']') {
                let end = end_bracket_index - 1; // Account for space before bracket
                if start < end {
                    // Trim subscription id and remove extra spaces and quotes
                    let subid = &msg[start..end].trim().trim_matches('"').trim();
                    return Ok(RelayMessage::eose(subid));
                }
            }
            return Err(Error::DecodeFailed(
                "Invalid subscription ID or format".into(),
            ));
        }

        // OK (NIP-20)
        // Relay response format: ["OK",<event_id>, <true|false>, <message>]
        if &msg[0..=5] == "[\"OK\"," && msg.len() >= 78 {
            let event_id = &msg[7..71];
            let booly = &msg[73..77];
            let status: bool = if booly == "true" {
                true
            } else if booly == "false" {
                false
            } else {
                return Err(Error::DecodeFailed("bad boolean value".into()));
            };
            let message_start = msg.rfind(',').unwrap() + 1;
            let message = &msg[message_start..msg.len() - 2].trim().trim_matches('"');
            return Ok(Self::ok(event_id, status, message));
        }

        Err(Error::DecodeFailed(format!(
            "unrecognized message type: '{msg}'"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_various_messages() -> Result<()> {
        let tests = vec![
            // Valid cases
            (
                // shortest valid message
                r#"["EOSE","x"]"#,
                Ok(RelayMessage::eose("x")),
            ),
            (
                // also very short
                r#"["NOTICE",""]"#,
                Ok(RelayMessage::notice("")),
            ),
            (
                r#"["NOTICE","Invalid event format!"]"#,
                Ok(RelayMessage::notice("Invalid event format!")),
            ),
            (
                r#"["EVENT", "random_string", {"id":"example","content":"test"}]"#,
                Ok(RelayMessage::event(
                    r#"["EVENT", "random_string", {"id":"example","content":"test"}]"#,
                    "random_string",
                )),
            ),
            (
                r#"["EOSE","random-subscription-id"]"#,
                Ok(RelayMessage::eose("random-subscription-id")),
            ),
            (
                r#"["EOSE", "random-subscription-id"]"#,
                Ok(RelayMessage::eose("random-subscription-id")),
            ),
            (
                r#"["EOSE", "random-subscription-id" ]"#,
                Ok(RelayMessage::eose("random-subscription-id")),
            ),
            (
                r#"["OK","b1a649ebe8b435ec71d3784793f3bbf4b93e64e17568a741aecd4c7ddeafce30",true,"pow: difficulty 25>=24"]"#,
                Ok(RelayMessage::ok(
                    "b1a649ebe8b435ec71d3784793f3bbf4b93e64e17568a741aecd4c7ddeafce30",
                    true,
                    "pow: difficulty 25>=24",
                )),
            ),
            // Invalid cases
            (
                r#"["EVENT","random_string"]"#,
                Err(Error::DecodeFailed("Invalid EVENT format".into())),
            ),
            (
                r#"["EOSE"]"#,
                Err(Error::DecodeFailed("message too short".into())),
            ),
            (
                r#"["NOTICE"]"#,
                Err(Error::DecodeFailed("message too short".into())),
            ),
            (
                r#"["NOTICE": 404]"#,
                Err(Error::DecodeFailed("unrecognized message type: '[\"NOTICE\": 404]'".into())),
            ),
            (
                r#"["OK","event_id"]"#,
                Err(Error::DecodeFailed("unrecognized message type: '[\"OK\",\"event_id\"]'".into())),
            ),
            (
                r#"["OK","b1a649ebe8b435ec71d3784793f3bbf4b93e64e17568a741aecd4c7ddeafce30"]"#,
                Err(Error::DecodeFailed("unrecognized message type: '[\"OK\",\"b1a649ebe8b435ec71d3784793f3bbf4b93e64e17568a741aecd4c7ddeafce30\"]'".into())),
            ),
            (
                r#"["OK","b1a649ebe8b435ec71d3784793f3bbf4b93e64e17568a741aecd4c7ddeafce30",hello,""]"#,
                Err(Error::DecodeFailed("bad boolean value".into())),
            ),
            (
                r#"["OK","b1a649ebe8b435ec71d3784793f3bbf4b93e64e17568a741aecd4c7ddeafce30",hello,404]"#,
                Err(Error::DecodeFailed("bad boolean value".into())),
            ),
        ];

        for (input, expected) in tests {
            match expected {
                Ok(expected_msg) => {
                    let result = RelayMessage::from_json(input);
                    assert_eq!(
                        result?, expected_msg,
                        "Expected {:?} for input: {}",
                        expected_msg, input
                    );
                }
                Err(expected_err) => {
                    let result = RelayMessage::from_json(input);
                    assert!(
                        matches!(result, Err(ref e) if *e.to_string() == expected_err.to_string()),
                        "Expected error {:?} for input: {}, but got: {:?}",
                        expected_err,
                        input,
                        result
                    );
                }
            }
        }
        Ok(())
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
}
