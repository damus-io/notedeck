// Copyright (c) 2019-2022 Naja Melan
// Copyright (c) 2023-2024 Yuki Kishimoto
// Distributed under the MIT software license

use js_sys::{ArrayBuffer, Uint8Array};
use wasm_bindgen::JsCast;
use web_sys::{Blob, MessageEvent};

use crate::message::Message;
use crate::wasm::Error;

/// This will convert the JavaScript event into a WsMessage. Note that this
/// will only work if the connection is set to use the binary type ArrayBuffer.
/// On binary type Blob, this will panic.
impl TryFrom<MessageEvent> for Message {
    type Error = Error;

    fn try_from(evt: MessageEvent) -> Result<Self, Self::Error> {
        match evt.data() {
            d if d.is_instance_of::<ArrayBuffer>() => {
                Ok(Message::Binary(Uint8Array::new(d.unchecked_ref()).to_vec()))
            }

            // We don't allow invalid encodings. In principle if needed,
            // we could add a variant to WsMessage with a CString or an OsString
            // to allow the user to access this data. However until there is a usecase,
            // I'm not inclined, amongst other things because the conversion from Js isn't very
            // clear and it would require a bunch of testing for something that's a rather bad
            // idea to begin with. If you need data that is not a valid string, use a binary
            // message.
            d if d.is_string() => match d.as_string() {
                Some(text) => Ok(Message::Text(text)),
                None => Err(Error::InvalidEncoding),
            },

            // We have set the binary mode to array buffer (WsMeta::connect), so normally this shouldn't happen.
            // That is as long as this is used within the context of the WsMeta constructor.
            d if d.is_instance_of::<Blob>() => Err(Error::CantDecodeBlob),

            // should never happen.
            _ => Err(Error::UnknownDataType),
        }
    }
}
