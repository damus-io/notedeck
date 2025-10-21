// This file is Copyright its original authors, visible in version control
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

//! Wire encoding/decoding for Lightning messages according to [BOLT #1], and for
//! custom message through the [`CustomMessageReader`] trait.
//!
//! [BOLT #1]: https://github.com/lightning/bolts/blob/master/01-messaging.md

use crate::ln::msgs;
use crate::util::ser::{LengthLimitedRead, LengthReadable, Readable, Writeable, Writer};
use std::io;

// TestEq is a dummy trait which requires PartialEq when built in testing, and otherwise is
// blanket-implemented for all types.

/// A Lightning message returned by [`read`] when decoding bytes received over the wire. Each
/// variant contains a message from [`msgs`] or otherwise the message type if unknown.
#[allow(missing_docs)]
#[derive(Debug)]
pub enum Message<T> {
    Init(msgs::Init),
    Error(msgs::ErrorMessage),
    Warning(msgs::WarningMessage),
    Ping(msgs::Ping),
    Pong(msgs::Pong),
    /// A message that could not be decoded because its type is unknown.
    Unknown(u16),
    /// A message that was produced by a [`CustomMessageReader`] and is to be handled by a
    /// [`crate::ln::peer_handler::CustomMessageHandler`].
    Custom(T),
}

impl<T: core::fmt::Debug + Type + Writeable> Writeable for Message<T> {
    fn write<W: Writer>(&self, writer: &mut W) -> Result<(), io::Error> {
        match self {
            Message::Init(msg) => msg.write(writer),
            Message::Error(msg) => msg.write(writer),
            Message::Warning(msg) => msg.write(writer),
            Message::Ping(msg) => msg.write(writer),
            Message::Pong(msg) => msg.write(writer),
            Message::Unknown(_) => Ok(()),
            Message::Custom(msg) => msg.write(writer),
        }
    }
}

impl<T: core::fmt::Debug + Type> Type for Message<T> {
    /// Returns the type that was used to decode the message payload.
    fn type_id(&self) -> u16 {
        match self {
            Message::Init(msg) => msg.type_id(),
            Message::Error(msg) => msg.type_id(),
            Message::Warning(msg) => msg.type_id(),
            Message::Ping(msg) => msg.type_id(),
            Message::Pong(msg) => msg.type_id(),
            Message::Unknown(type_id) => *type_id,
            Message::Custom(msg) => msg.type_id(),
        }
    }
}

impl<T: core::fmt::Debug + Type> Message<T> {
    /// Returns whether the message's type is even, indicating both endpoints must support it.
    pub fn is_even(&self) -> bool {
        (self.type_id() & 1) == 0
    }
}

/// Reads a message from the data buffer consisting of a 2-byte big-endian type and a
/// variable-length payload conforming to the type.
///
/// # Errors
///
/// Returns an error if the message payload could not be decoded as the specified type.
pub fn read<T, R>(
    buffer: &mut R,
    custom_reader: impl FnOnce(u16, &mut R) -> Result<Option<T>, msgs::DecodeError>,
) -> Result<Message<T>, (msgs::DecodeError, Option<u16>)>
where
    R: LengthLimitedRead,
{
    let message_type = <u16 as Readable>::read(buffer).map_err(|e| (e, None))?;
    //println!("message_type {}", message_type);
    do_read(buffer, message_type, custom_reader).map_err(|e| (e, Some(message_type)))
}

fn do_read<T, R>(
    buffer: &mut R,
    message_type: u16,
    custom_reader: impl FnOnce(u16, &mut R) -> Result<Option<T>, msgs::DecodeError>,
) -> Result<Message<T>, msgs::DecodeError>
where
    R: LengthLimitedRead,
{
    match message_type {
        msgs::Init::TYPE => {
            let r = LengthReadable::read_from_fixed_length_buffer(buffer);
            //println!("remaining end {} r {:?}", buffer.remaining_bytes(), r);
            Ok(Message::Init(r?))
        }
        msgs::ErrorMessage::TYPE => Ok(Message::Error(
            LengthReadable::read_from_fixed_length_buffer(buffer)?,
        )),
        msgs::WarningMessage::TYPE => Ok(Message::Warning(
            LengthReadable::read_from_fixed_length_buffer(buffer)?,
        )),
        msgs::Ping::TYPE => Ok(Message::Ping(
            LengthReadable::read_from_fixed_length_buffer(buffer)?,
        )),
        msgs::Pong::TYPE => Ok(Message::Pong(
            LengthReadable::read_from_fixed_length_buffer(buffer)?,
        )),
        _ => {
            if let Some(custom) = custom_reader(message_type, buffer)? {
                Ok(Message::Custom(custom))
            } else {
                Ok(Message::Unknown(message_type))
            }
        }
    }
}

/// Writes a message to the data buffer encoded as a 2-byte big-endian type and a variable-length
/// payload.
///
/// # Errors
///
/// Returns an I/O error if the write could not be completed.
pub(crate) fn write<M: Type + Writeable, W: Writer>(
    message: &M,
    buffer: &mut W,
) -> Result<(), io::Error> {
    message.type_id().write(buffer)?;
    message.write(buffer)
}

mod encode {
    /// Defines a constant type identifier for reading messages from the wire.
    pub trait Encode {
        /// The type identifying the message payload.
        const TYPE: u16;
    }
}

pub(crate) use self::encode::Encode;

/// Defines a type identifier for sending messages over the wire.
///
/// Messages implementing this trait specify a type and must be [`Writeable`].
pub trait Type {
    /// Returns the type identifying the message payload.
    fn type_id(&self) -> u16;
}

impl Type for () {
    fn type_id(&self) -> u16 {
        unreachable!();
    }
}

impl<T: Encode + core::fmt::Debug + Writeable> Type for T {
    fn type_id(&self) -> u16 {
        T::TYPE
    }
}

impl Encode for msgs::Init {
    const TYPE: u16 = 16;
}

impl Encode for msgs::ErrorMessage {
    const TYPE: u16 = 17;
}

impl Encode for msgs::WarningMessage {
    const TYPE: u16 = 1;
}

impl Encode for msgs::Ping {
    const TYPE: u16 = 18;
}

impl Encode for msgs::Pong {
    const TYPE: u16 = 19;
}
