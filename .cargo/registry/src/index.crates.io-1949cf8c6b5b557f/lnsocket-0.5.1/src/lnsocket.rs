use crate::{
    Error,
    ln::{
        msgs::{self, DecodeError},
        peer_channel_encryptor::PeerChannelEncryptor,
        wire::{self, Message},
    },
    util::ser::Writeable,
};
use bitcoin::secp256k1::{PublicKey, Secp256k1, SecretKey, rand};
use std::io::{self, Cursor};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpSocket, TcpStream, lookup_host};

const ACT_TWO_SIZE: usize = 50;

struct ReconnectData {
    our_key: SecretKey,
    their_pubkey: PublicKey,
    addr: String,
}

/// A Lightning Network TCP socket that performs the BOLT 8 Noise handshake and message encryption.
///
/// [`LNSocket`] wraps a `tokio::net::TcpStream` with Noise state (via [`PeerChannelEncryptor`])
/// to handle encrypted Lightning messages over TCP.
///
/// # Typical usage
/// ```no_run
/// use bitcoin::secp256k1::{SecretKey, PublicKey, rand};
/// use lnsocket::LNSocket;
/// use lnsocket::ln::msgs;
///
/// # async fn example(peer: PublicKey) -> Result<(), lnsocket::Error> {
/// let sk = SecretKey::new(&mut rand::thread_rng());
/// let mut sock = LNSocket::connect_and_init(sk, peer, "node.example.com:9735").await?;
/// sock.write(&msgs::Ping { ponglen: 4, byteslen: 8 }).await?;
/// let msg = sock.read().await?;
/// # Ok(()) }
/// ```
///
/// ⚠️ This type does **not** do retries/keepalive; see [`CommandoClient`] if you want managed reconnects.
pub struct LNSocket {
    channel: PeerChannelEncryptor,
    stream: TcpStream,
    reconnect: ReconnectData,
}

impl LNSocket {
    /// Connect to a Lightning peer and complete the BOLT 8 Noise handshake.
    ///
    /// Resolves the given `addr`, establishes a TCP connection, and performs act1/act2/act3
    /// handshake using `our_key` and the peer’s public key.
    ///
    /// Does **not** send or expect an `init` message.  
    /// Use [`LNSocket::connect_and_init`] if you want handshake + `init` exchange.
    pub async fn connect(
        our_key: SecretKey,
        their_pubkey: PublicKey,
        addr: &str,
    ) -> Result<LNSocket, Error> {
        let secp_ctx = Secp256k1::signing_only();

        // Look up host to resolve domain name to IP address
        let addr = lookup_host(addr).await?.next().ok_or(Error::DnsError)?;

        let socket = if addr.is_ipv4() {
            TcpSocket::new_v4()?
        } else {
            TcpSocket::new_v6()?
        };

        let mut stream = socket.connect(addr).await?;
        let ephemeral = SecretKey::new(&mut rand::thread_rng());

        let mut channel = PeerChannelEncryptor::new_outbound(their_pubkey, ephemeral);
        let act_one = channel.get_act_one(&secp_ctx);
        stream.write_all(&act_one).await?;

        let mut act_two = [0u8; ACT_TWO_SIZE];
        stream.read_exact(&mut act_two).await?;
        let act_three = channel.process_act_two(&secp_ctx, &act_two, &our_key)?;

        // Finalize the handshake by sending act3
        stream.write_all(&act_three).await?;

        Ok(Self {
            channel,
            stream,
            reconnect: ReconnectData {
                our_key: our_key.clone(),
                their_pubkey,
                addr: addr.to_string(),
            },
        })
    }

    /// Connect as above and also perform a minimal `init` exchange.
    /// Fails with `Error::FirstMessageNotInit` if the peer’s first message isn’t `Init`.
    pub async fn connect_and_init(
        our_key: SecretKey,
        their_pubkey: PublicKey,
        addr: &str,
    ) -> Result<LNSocket, Error> {
        let mut lnsocket = LNSocket::connect(our_key, their_pubkey, addr).await?;
        lnsocket.perform_init().await?;
        Ok(lnsocket)
    }

    /// Build a brand-new socket using the stored reconnect inputs.
    pub async fn reconnect_fresh(&self) -> Result<LNSocket, Error> {
        LNSocket::connect_and_init(
            self.reconnect.our_key.clone(),
            self.reconnect.their_pubkey,
            &self.reconnect.addr,
        )
        .await
    }

    /// Completes the initial `init` message exchange.
    ///
    /// This must be called before issuing any other Lightning messages.
    /// Fails if the first incoming message isn’t `Init`.
    pub async fn perform_init(&mut self) -> Result<(), Error> {
        // first message should be init, if not, we fail
        if let Message::Init(_) = self.read().await? {
            // ok
        } else {
            return Err(Error::FirstMessageNotInit);
        }

        // send some bs
        Ok(self
            .write(&msgs::Init {
                features: vec![0; 5],
                global_features: vec![0; 2],
                remote_network_address: None,
                networks: Some(vec![bitcoin::constants::ChainHash::BITCOIN]),
            })
            .await?)
    }

    pub async fn write<M: wire::Type + Writeable>(&mut self, m: &M) -> Result<(), io::Error> {
        let msg = self.channel.encrypt_message(m);
        self.stream.write_all(&msg).await?;
        Ok(())
    }

    pub async fn read(&mut self) -> Result<Message<()>, Error> {
        self.read_custom(|_type, _buf| Ok(None)).await
    }

    pub async fn read_custom<T>(
        &mut self,
        handler: impl FnOnce(u16, &mut Cursor<&[u8]>) -> Result<Option<T>, DecodeError>,
    ) -> Result<Message<T>, Error>
    where
        T: core::fmt::Debug,
    {
        let mut hdr = [0u8; 18];

        self.stream.read_exact(&mut hdr).await?;
        let size = self.channel.decrypt_length_header(&hdr)? as usize;
        //println!("len header {size}");
        let mut buf = vec![0; size + 16];
        self.stream.read_exact(&mut buf).await?;
        //println!("got cipher bytes {}", hex::encode(&buf));
        self.channel.decrypt_message(&mut buf)?;
        let u8_buf: &[u8] = &buf[..buf.len() - 16];
        let mut cursor = io::Cursor::new(u8_buf);

        Ok(wire::read(&mut cursor, handler).map_err(|(de, _)| de)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOpts;
    use crate::ln::msgs;
    use std::str::FromStr;

    #[tokio::test]
    async fn test_ping_pong() -> Result<(), Error> {
        let key = SecretKey::new(&mut rand::thread_rng());
        let their_key = PublicKey::from_str(
            "03f3c108ccd536b8526841f0a5c58212bb9e6584a1eb493080e7c1cc34f82dad71",
        )
        .unwrap();

        let mut lnsocket = LNSocket::connect_and_init(key, their_key, "ln.damus.io:9735").await?;

        //println!("got here");
        lnsocket
            .write(&msgs::Ping {
                ponglen: 4,
                byteslen: 8,
            })
            .await?;

        loop {
            if let Message::Pong(_) = lnsocket.read().await? {
                break;
            } else {
                // didn't get pong?
            }
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_commando() -> Result<(), Error> {
        use crate::commando::CommandoClient;
        use bitcoin::secp256k1::{PublicKey, SecretKey, rand};
        use serde_json::json;
        use std::str::FromStr;

        let key = SecretKey::new(&mut rand::thread_rng());
        let pk_str = "03f3c108ccd536b8526841f0a5c58212bb9e6584a1eb493080e7c1cc34f82dad71";
        let their_key = PublicKey::from_str(pk_str).unwrap();

        // Connect as before
        let sock = LNSocket::connect_and_init(key, their_key, "ln.damus.io:9735").await?;

        // New API: spawn the client (it owns the socket + pump task)
        let commando = CommandoClient::spawn(
            sock,
            "hfYByx-RDwdBfAK-vOWeOCDJVYlvKSioVKU_y7jccZU9MjkmbWV0aG9kPWdldGluZm8=",
        );

        // New call signature: no socket arg, optional wait timeout
        let resp_fut = commando.call_with_opts(
            "getinfo",
            json!({}),
            CallOpts::default().filter(json!({"id": true})),
        );

        let bad_resp_fut = commando.call("invoice", json!({"msatoshi": "any"}));

        let resp = resp_fut.await?;

        assert_eq!(resp["id"].as_str().unwrap(), pk_str);

        // test filtering
        assert_eq!(resp.get("alias").is_none(), true);

        let bad_resp = bad_resp_fut.await.err().unwrap();
        if let Error::Rpc(rpc_err) = bad_resp {
            assert_eq!(rpc_err.code, 19537);
            assert_eq!(
                rpc_err.message,
                "Invalid rune: Not permitted: method is not equal to getinfo"
            );
        } else {
            assert_eq!(true, false);
        }

        Ok(())
    }
}
