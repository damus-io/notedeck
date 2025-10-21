//! # LNSocket
//!
//! `lnsocket` is a minimal, async Lightning Network socket library built on `tokio`.
//! It implements the BOLT 8 Noise handshake and typed Lightning wire framing, and
//! it stays out of your way: no global state, no TLS, no heavy deps.
//!
//! ## What this crate gives you
//! - **`LNSocket`** – connect over TCP, perform Noise (act1/2/3), and read/write typed BOLT#1 messages.
//! - **`CommandoClient`** – a small client for Core Lightning **Commando** over a live `LNSocket`,
//!   with a background pump, **auto-reconnect**, and **retry/resend** semantics.
//!
//! ## Design philosophy
//! - Keep the transport tight and explicit. You own key management, policies, and backpressure.
//! - Avoid surprises: I/O errors return an `Error` that carries **`io::ErrorKind`** only.
//!
//! ## Quick starts
//!
//! ### Low-level: just a Lightning socket
//! ```no_run
//! use bitcoin::secp256k1::{SecretKey, PublicKey, rand};
//! use lnsocket::{LNSocket, ln::msgs};
//! # async fn demo(their_pubkey: PublicKey) -> Result<(), lnsocket::Error> {
//! let our_key = SecretKey::new(&mut rand::thread_rng());
//! let mut sock = LNSocket::connect_and_init(our_key, their_pubkey, "node.example.com:9735").await?;
//! sock.write(&msgs::Ping { ponglen: 4, byteslen: 8 }).await?;
//! let _msg = sock.read().await?; // e.g. expect a Pong
//! # Ok(()) }
//! ```
//!
//! ### Higher-level: Commando over LNSocket
//! ```no_run
//! use bitcoin::secp256k1::{SecretKey, PublicKey, rand};
//! use lnsocket::{LNSocket, CommandoClient};
//! use serde_json::json;
//! # async fn demo(their_pubkey: PublicKey, rune: &str) -> Result<(), lnsocket::Error> {
//! let key = SecretKey::new(&mut rand::thread_rng());
//! let sock = LNSocket::connect_and_init(key, their_pubkey, "ln.example.com:9735").await?;
//!
//! // Spawns a background pump task. IDs are generated internally.
//! let commando = CommandoClient::spawn(sock, rune);
//!
//! // Simple call with crate defaults (30s timeout, auto-reconnect, retry up to 3 times).
//! let info = commando.call("getinfo", json!({})).await?;
//! println!("getinfo: {}", info);
//! # Ok(()) }
//! ```
//!
//! ## Footguns & non-goals
//! - No built-in keepalives/backpressure – handle in your app.
//! - Reconnection logic lives in `CommandoClient`, **not** `LNSocket`.
//! - `LNSocket::perform_init` performs a minimal `init` exchange by design.

pub mod commando;
mod crypto;
pub mod error;
pub mod ln;
pub mod lnsocket;
mod sign;
mod socket_addr;
mod util;

pub use bitcoin;
pub use commando::{CallOpts, CommandoClient};
pub use error::{Error, RpcError};
pub use lnsocket::LNSocket;

mod prelude {
    #![allow(unused_imports)]

    pub use std::{boxed::Box, collections::VecDeque, string::String, vec, vec::Vec};

    pub use std::borrow::ToOwned;
    pub use std::string::ToString;

    pub use core::convert::{AsMut, AsRef, TryFrom, TryInto};
    pub use core::default::Default;
    pub use core::marker::Sized;

    pub(crate) use crate::util::hash_tables::*;
}

#[doc(hidden)]
/// IO utilities public only for use by in-crate macros. These should not be used externally
///
/// This is not exported to bindings users as it is not intended for public consumption.
pub mod io_extras {
    use std::io::{self, Read, Write};

    /// Creates an instance of a writer which will successfully consume all data.
    pub use std::io::sink;

    pub fn copy<R: Read + ?Sized, W: Write + ?Sized>(
        reader: &mut R,
        writer: &mut W,
    ) -> Result<u64, io::Error> {
        let mut count = 0;
        let mut buf = [0u8; 64];

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    writer.write_all(&buf[0..n])?;
                    count += n as u64;
                }
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            };
        }
        Ok(count)
    }

    pub fn read_to_end<D: Read>(d: &mut D) -> Result<std::vec::Vec<u8>, io::Error> {
        let mut result = vec![];
        let mut buf = [0u8; 64];
        loop {
            match d.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => result.extend_from_slice(&buf[0..n]),
                Err(ref e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            };
        }
        Ok(result)
    }
}
