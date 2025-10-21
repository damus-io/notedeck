use crate::util::{
    logger,
    ser::{LengthLimitedRead, LengthReadable, Readable, WithoutLength, Writeable, Writer},
};
use crate::{encode_tlv_stream, ln::types::ChannelId, socket_addr::SocketAddress};
use bitcoin::blockdata::constants::ChainHash;
use lightning_types::features::InitFeatures;
use std::io;

/// An Err type for failure to process messages.
#[derive(Clone, Debug)]
pub struct LightningError {
    /// A human-readable message describing the error
    pub err: String,
    /// The action which should be taken against the offending peer.
    pub action: ErrorAction,
}

/// An error in decoding a message or struct.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub enum DecodeError {
    /// A version byte specified something we don't know how to handle.
    ///
    /// Includes unknown realm byte in an onion hop data packet.
    UnknownVersion,
    /// Unknown feature mandating we fail to parse message (e.g., TLV with an even, unknown type)
    UnknownRequiredFeature,
    /// Value was invalid.
    ///
    /// For example, a byte which was supposed to be a bool was something other than a 0
    /// or 1, a public key/private key/signature was invalid, text wasn't UTF-8, TLV was
    /// syntactically incorrect, etc.
    InvalidValue,
    /// The buffer to be read was too short.
    ShortRead,
    /// A length descriptor in the packet didn't describe the later data correctly.
    BadLengthDescriptor,
    /// Error from [`crate::io`].
    Io(std::io::ErrorKind),
}

impl From<std::io::Error> for DecodeError {
    fn from(err: std::io::Error) -> Self {
        DecodeError::Io(err.kind())
    }
}

/// An [`init`] message to be sent to or received from a peer.
///
/// [`init`]: https://github.com/lightning/bolts/blob/master/01-messaging.md#the-init-message
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Init {
    /// The relevant features which the sender supports.
    pub global_features: Vec<u8>,
    pub features: Vec<u8>,
    /// Indicates chains the sender is interested in.
    ///
    /// If there are no common chains, the connection will be closed.
    pub networks: Option<Vec<ChainHash>>,
    /// The receipient's network address.
    ///
    /// This adds the option to report a remote IP address back to a connecting peer using the init
    /// message. A node can decide to use that information to discover a potential update to its
    /// public IPv4 address (NAT) and use that for a [`NodeAnnouncement`] update message containing
    /// the new address.
    pub remote_network_address: Option<SocketAddress>,
}

/// An [`error`] message to be sent to or received from a peer.
///
/// [`error`]: https://github.com/lightning/bolts/blob/master/01-messaging.md#the-error-and-warning-messages
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct ErrorMessage {
    /// The channel ID involved in the error.
    ///
    /// All-0s indicates a general error unrelated to a specific channel, after which all channels
    /// with the sending peer should be closed.
    pub channel_id: ChannelId,
    /// A possibly human-readable error description.
    ///
    /// The string should be sanitized before it is used (e.g., emitted to logs or printed to
    /// `stdout`). Otherwise, a well crafted error message may trigger a security vulnerability in
    /// the terminal emulator or the logging subsystem.
    pub data: String,
}

/// A [`warning`] message to be sent to or received from a peer.
///
/// [`warning`]: https://github.com/lightning/bolts/blob/master/01-messaging.md#the-error-and-warning-messages
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct WarningMessage {
    /// The channel ID involved in the warning.
    ///
    /// All-0s indicates a warning unrelated to a specific channel.
    pub channel_id: ChannelId,
    /// A possibly human-readable warning description.
    ///
    /// The string should be sanitized before it is used (e.g. emitted to logs or printed to
    /// stdout). Otherwise, a well crafted error message may trigger a security vulnerability in
    /// the terminal emulator or the logging subsystem.
    pub data: String,
}

/// A [`ping`] message to be sent to or received from a peer.
///
/// [`ping`]: https://github.com/lightning/bolts/blob/master/01-messaging.md#the-ping-and-pong-messages
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Ping {
    /// The desired response length.
    pub ponglen: u16,
    /// The ping packet size.
    ///
    /// This field is not sent on the wire. byteslen zeros are sent.
    pub byteslen: u16,
}

/// A [`pong`] message to be sent to or received from a peer.
///
/// [`pong`]: https://github.com/lightning/bolts/blob/master/01-messaging.md#the-ping-and-pong-messages
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Pong {
    /// The pong packet size.
    ///
    /// This field is not sent on the wire. byteslen zeros are sent.
    pub byteslen: u16,
}

/// Used to put an error message in a [`LightningError`].
#[derive(Clone, Debug, Hash, PartialEq)]
pub enum ErrorAction {
    /// The peer took some action which made us think they were useless. Disconnect them.
    DisconnectPeer {
        /// An error message which we should make an effort to send before we disconnect.
        msg: Option<ErrorMessage>,
    },
    /// The peer did something incorrect. Tell them without closing any channels and disconnect them.
    DisconnectPeerWithWarning {
        /// A warning message which we should make an effort to send before we disconnect.
        msg: WarningMessage,
    },
    /// The peer did something harmless that we weren't able to process, just log and ignore
    // New code should *not* use this. New code must use IgnoreAndLog, below!
    IgnoreError,
    /// The peer did something harmless that we weren't able to meaningfully process.
    /// If the error is logged, log it at the given level.
    //IgnoreAndLog(logger::Level),
    /// The peer provided us with a gossip message which we'd already seen. In most cases this
    /// should be ignored, but it may result in the message being forwarded if it is a duplicate of
    /// our own channel announcements.
    IgnoreDuplicateGossip,
    /// The peer did something incorrect. Tell them.
    SendErrorMessage {
        /// The message to send.
        msg: ErrorMessage,
    },
    /// The peer did something incorrect. Tell them without closing any channels.
    SendWarningMessage {
        /// The message to send.
        msg: WarningMessage,
        /// The peer may have done something harmless that we weren't able to meaningfully process,
        /// though we should still tell them about it.
        /// If this event is logged, log it at the given level.
        log_level: logger::Level,
    },
}

impl Writeable for Init {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), std::io::Error> {
        // global_features gets the bottom 13 bits of our features, and local_features gets all of
        // our relevant feature bits. This keeps us compatible with old nodes.
        //write_features_up_to_13(w, self.features.le_flags())?;
        self.global_features.write(w)?;
        self.features.write(w)?;
        encode_tlv_stream!(w, {
            (1, self.networks.as_ref().map(WithoutLength), option),
            (3, self.remote_network_address, option),
        });
        Ok(())
    }
}

/*
pub(crate) fn write_features_up_to_13<W: Writer>(
    w: &mut W,
    le_flags: &[u8],
) -> Result<(), io::Error> {
    let len = core::cmp::min(2, le_flags.len());
    (len as u16).write(w)?;
    for i in (0..len).rev() {
        if i == 0 {
            le_flags[i].write(w)?;
        } else {
            // On byte 1, we want up-to-and-including-bit-13, 0-indexed, which is
            // up-to-and-including-bit-5, 0-indexed, on this byte:
            (le_flags[i] & 0b00_11_11_11).write(w)?;
        }
    }
    Ok(())
}
*/

macro_rules! impl_feature_len_prefixed_write {
    ($features: ident) => {
        impl Writeable for $features {
            fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
                let bytes = self.le_flags();
                (bytes.len() as u16).write(w)?;
                write_be(w, bytes)
            }
        }
        impl Readable for $features {
            fn read<R: io::Read>(r: &mut R) -> Result<Self, DecodeError> {
                Ok(Self::from_be_bytes(Vec::<u8>::read(r)?))
            }
        }
    };
}

//impl_feature_len_prefixed_write!(NodeFeatures);
impl_feature_len_prefixed_write!(InitFeatures);

fn write_be<W: Writer>(w: &mut W, le_flags: &[u8]) -> Result<(), io::Error> {
    // Swap back to big-endian
    for f in le_flags.iter().rev() {
        f.write(w)?;
    }
    Ok(())
}

impl Writeable for ErrorMessage {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        self.channel_id.write(w)?;
        (self.data.len() as u16).write(w)?;
        w.write_all(self.data.as_bytes())?;
        Ok(())
    }
}

impl LengthReadable for ErrorMessage {
    fn read_from_fixed_length_buffer<R: LengthLimitedRead>(r: &mut R) -> Result<Self, DecodeError> {
        Ok(Self {
            channel_id: Readable::read(r)?,
            data: {
                let sz: usize = <u16 as Readable>::read(r)? as usize;
                let mut data = vec![0; sz];
                r.read_exact(&mut data)?;
                match String::from_utf8(data) {
                    Ok(s) => s,
                    Err(_) => return Err(DecodeError::InvalidValue),
                }
            },
        })
    }
}

impl Writeable for WarningMessage {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        self.channel_id.write(w)?;
        (self.data.len() as u16).write(w)?;
        w.write_all(self.data.as_bytes())?;
        Ok(())
    }
}

impl LengthReadable for WarningMessage {
    fn read_from_fixed_length_buffer<R: LengthLimitedRead>(r: &mut R) -> Result<Self, DecodeError> {
        Ok(Self {
            channel_id: Readable::read(r)?,
            data: {
                let sz: usize = <u16 as Readable>::read(r)? as usize;
                let mut data = vec![0; sz];
                r.read_exact(&mut data)?;
                match String::from_utf8(data) {
                    Ok(s) => s,
                    Err(_) => return Err(DecodeError::InvalidValue),
                }
            },
        })
    }
}

impl Writeable for Pong {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        vec![0u8; self.byteslen as usize].write(w)?; // size-unchecked write
        Ok(())
    }
}

impl LengthReadable for Pong {
    fn read_from_fixed_length_buffer<R: LengthLimitedRead>(r: &mut R) -> Result<Self, DecodeError> {
        Ok(Pong {
            byteslen: {
                let byteslen = Readable::read(r)?;
                r.read_exact(&mut vec![0u8; byteslen as usize][..])?;
                byteslen
            },
        })
    }
}

impl Writeable for Ping {
    fn write<W: Writer>(&self, w: &mut W) -> Result<(), io::Error> {
        self.ponglen.write(w)?;
        vec![0u8; self.byteslen as usize].write(w)?; // size-unchecked write
        Ok(())
    }
}

impl LengthReadable for Ping {
    fn read_from_fixed_length_buffer<R: LengthLimitedRead>(r: &mut R) -> Result<Self, DecodeError> {
        Ok(Ping {
            ponglen: Readable::read(r)?,
            byteslen: {
                let byteslen = Readable::read(r)?;
                r.read_exact(&mut vec![0u8; byteslen as usize][..])?;
                byteslen
            },
        })
    }
}

impl LengthReadable for Init {
    fn read_from_fixed_length_buffer<R: LengthLimitedRead>(r: &mut R) -> Result<Self, DecodeError> {
        //println!("remaining 1 {}", r.remaining_bytes());
        let global_features: Vec<u8> = Readable::read(r)?;
        //println!("reading global features {:?}", global_features);
        let features: Vec<u8> = Readable::read(r)?;
        //println!("reading remote features {:?}", features);
        //let mut remote_network_address: Option<SocketAddress> = None;
        //let mut networks: Option<WithoutLength<Vec<ChainHash>>> = None;

        let mut buf = Vec::with_capacity(r.remaining_bytes() as usize);
        r.read_to_end(&mut buf)?;

        // TODO: fixme
        /*
        decode_tlv_stream!(r, {
            (1, networks, option),
            (3, remote_network_address, option)
        });
        */
        Ok(Init {
            global_features,
            features,
            networks: None,
            remote_network_address: None,
        })
    }
}
