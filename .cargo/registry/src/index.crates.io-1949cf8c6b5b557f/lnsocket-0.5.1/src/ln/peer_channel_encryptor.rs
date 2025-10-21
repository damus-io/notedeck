// This file is Copyright its original authors, visible in version control
// history.
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

//use crate::prelude::*;

use crate::ln::msgs;
use crate::ln::msgs::LightningError;
use crate::ln::wire;

use bitcoin::hashes::sha256::Hash as Sha256;
use bitcoin::hashes::{Hash, HashEngine};

use bitcoin::hex::DisplayHex;

use bitcoin::secp256k1::{self, PublicKey, Secp256k1, SecretKey, Signing, ecdh::SharedSecret};

use crate::crypto::chacha20poly1305rfc::ChaCha20Poly1305RFC;
use crate::crypto::utils::hkdf_extract_expand_twice;
use crate::util::ser::{VecWriter, Writeable};

/// Maximum Lightning message data length according to
/// [BOLT-8](https://github.com/lightning/bolts/blob/v1.0/08-transport.md#lightning-message-specification)
/// and [BOLT-1](https://github.com/lightning/bolts/blob/master/01-messaging.md#lightning-message-format):
pub const LN_MAX_MSG_LEN: usize = u16::MAX as usize; // Must be equal to 65535

/// The (rough) size buffer to pre-allocate when encoding a message. Messages should reliably be
/// smaller than this size by at least 32 bytes or so.
pub const MSG_BUF_ALLOC_SIZE: usize = 2048;

// Sha256("Noise_XK_secp256k1_ChaChaPoly_SHA256")
const NOISE_CK: [u8; 32] = [
    0x26, 0x40, 0xf5, 0x2e, 0xeb, 0xcd, 0x9e, 0x88, 0x29, 0x58, 0x95, 0x1c, 0x79, 0x42, 0x50, 0xee,
    0xdb, 0x28, 0x00, 0x2c, 0x05, 0xd7, 0xdc, 0x2e, 0xa0, 0xf1, 0x95, 0x40, 0x60, 0x42, 0xca, 0xf1,
];
// Sha256(NOISE_CK || "lightning")
const NOISE_H: [u8; 32] = [
    0xd1, 0xfb, 0xf6, 0xde, 0xe4, 0xf6, 0x86, 0xf1, 0x32, 0xfd, 0x70, 0x2c, 0x4a, 0xbf, 0x8f, 0xba,
    0x4b, 0xb4, 0x20, 0xd8, 0x9d, 0x2a, 0x04, 0x8a, 0x3c, 0x4f, 0x4c, 0x09, 0x2e, 0x37, 0xb6, 0x76,
];

#[derive(PartialEq)]
enum NoiseStep {
    PreActOne,
    PostActOne,
    //PostActTwo,
    // When done swap noise_state for NoiseState::Finished
}

struct BidirectionalNoiseState {
    h: [u8; 32],
    ck: [u8; 32],
}
enum DirectionalNoiseState {
    Outbound { ie: SecretKey },
    /*
    Inbound {
        ie: Option<PublicKey>,     // filled in if state >= PostActOne
        re: Option<SecretKey>,     // filled in if state >= PostActTwo
        temp_k2: Option<[u8; 32]>, // filled in if state >= PostActTwo
    },
    */
}
enum NoiseState {
    InProgress {
        state: NoiseStep,
        directional_state: DirectionalNoiseState,
        bidirectional_state: BidirectionalNoiseState,
    },
    Finished {
        sk: [u8; 32],
        sn: u64,
        sck: [u8; 32],
        rk: [u8; 32],
        rn: u64,
        rck: [u8; 32],
    },
}

pub struct PeerChannelEncryptor {
    their_node_id: Option<PublicKey>, // filled in for outbound, or inbound after noise_state is Finished

    noise_state: NoiseState,
}

impl PeerChannelEncryptor {
    pub fn new_outbound(
        their_node_id: PublicKey,
        ephemeral_key: SecretKey,
    ) -> PeerChannelEncryptor {
        let mut sha = Sha256::engine();
        sha.input(&NOISE_H);
        sha.input(&their_node_id.serialize()[..]);
        let h = Sha256::from_engine(sha).to_byte_array();

        PeerChannelEncryptor {
            their_node_id: Some(their_node_id),
            noise_state: NoiseState::InProgress {
                state: NoiseStep::PreActOne,
                directional_state: DirectionalNoiseState::Outbound { ie: ephemeral_key },
                bidirectional_state: BidirectionalNoiseState { h, ck: NOISE_CK },
            },
        }
    }

    #[inline]
    fn encrypt_with_ad(res: &mut [u8], n: u64, key: &[u8; 32], h: &[u8], plaintext: &[u8]) {
        let mut nonce = [0; 12];
        nonce[4..].copy_from_slice(&n.to_le_bytes()[..]);

        let mut chacha = ChaCha20Poly1305RFC::new(key, &nonce, h);
        let mut tag = [0; 16];
        chacha.encrypt(plaintext, &mut res[0..plaintext.len()], &mut tag);
        res[plaintext.len()..].copy_from_slice(&tag);
    }

    #[inline]
    /// Encrypts the message in res[offset..] in-place and pushes a 16-byte tag onto the end of
    /// res.
    fn encrypt_in_place_with_ad(
        res: &mut Vec<u8>,
        offset: usize,
        n: u64,
        key: &[u8; 32],
        h: &[u8],
    ) {
        let mut nonce = [0; 12];
        nonce[4..].copy_from_slice(&n.to_le_bytes()[..]);

        let mut chacha = ChaCha20Poly1305RFC::new(key, &nonce, h);
        let mut tag = [0; 16];
        chacha.encrypt_full_message_in_place(&mut res[offset..], &mut tag);
        res.extend_from_slice(&tag);
    }

    fn decrypt_in_place_with_ad(
        inout: &mut [u8],
        n: u64,
        key: &[u8; 32],
        h: &[u8],
    ) -> Result<(), LightningError> {
        let mut nonce = [0; 12];
        nonce[4..].copy_from_slice(&n.to_le_bytes()[..]);

        let mut chacha = ChaCha20Poly1305RFC::new(key, &nonce, h);
        let (inout, tag) = inout.split_at_mut(inout.len() - 16);
        if chacha.check_decrypt_in_place(inout, tag).is_err() {
            return Err(LightningError {
                err: "Bad MAC".to_owned(),
                action: msgs::ErrorAction::DisconnectPeer { msg: None },
            });
        }
        Ok(())
    }

    #[inline]
    fn decrypt_with_ad(
        res: &mut [u8],
        n: u64,
        key: &[u8; 32],
        h: &[u8],
        cyphertext: &[u8],
    ) -> Result<(), LightningError> {
        let mut nonce = [0; 12];
        nonce[4..].copy_from_slice(&n.to_le_bytes()[..]);

        /*
        println!(
            "n {} key {} h {} cypher {}",
            n,
            hex::encode(key),
            hex::encode(h),
            hex::encode(cyphertext)
        );
        */
        let mut chacha = ChaCha20Poly1305RFC::new(key, &nonce, h);
        if chacha
            .variable_time_decrypt(
                &cyphertext[0..cyphertext.len() - 16],
                res,
                &cyphertext[cyphertext.len() - 16..],
            )
            .is_err()
        {
            return Err(LightningError {
                err: "Bad MAC".to_owned(),
                action: msgs::ErrorAction::DisconnectPeer { msg: None },
            });
        }
        //println!("ok! {}", hex::encode(res));
        Ok(())
    }

    #[inline]
    fn hkdf(state: &mut BidirectionalNoiseState, ss: SharedSecret) -> [u8; 32] {
        let (t1, t2) = hkdf_extract_expand_twice(&state.ck, ss.as_ref());
        state.ck = t1;
        t2
    }

    #[inline]
    fn outbound_noise_act<T: secp256k1::Signing>(
        secp_ctx: &Secp256k1<T>,
        state: &mut BidirectionalNoiseState,
        our_key: &SecretKey,
        their_key: &PublicKey,
    ) -> ([u8; 50], [u8; 32]) {
        let our_pub = PublicKey::from_secret_key(secp_ctx, our_key);

        let mut sha = Sha256::engine();
        sha.input(&state.h);
        sha.input(&our_pub.serialize()[..]);
        state.h = Sha256::from_engine(sha).to_byte_array();

        let ss = SharedSecret::new(their_key, our_key);
        let temp_k = PeerChannelEncryptor::hkdf(state, ss);

        let mut res = [0; 50];
        res[1..34].copy_from_slice(&our_pub.serialize()[..]);
        PeerChannelEncryptor::encrypt_with_ad(&mut res[34..], 0, &temp_k, &state.h, &[0; 0]);

        let mut sha = Sha256::engine();
        sha.input(&state.h);
        sha.input(&res[34..]);
        state.h = Sha256::from_engine(sha).to_byte_array();

        (res, temp_k)
    }

    #[inline]
    fn inbound_noise_act(
        state: &mut BidirectionalNoiseState,
        act: &[u8],
        secret_key: &SecretKey,
    ) -> Result<(PublicKey, [u8; 32]), LightningError> {
        assert_eq!(act.len(), 50);

        if act[0] != 0 {
            return Err(LightningError {
                err: format!("Unknown handshake version number {}", act[0]),
                action: msgs::ErrorAction::DisconnectPeer { msg: None },
            });
        }

        let their_pub = match PublicKey::from_slice(&act[1..34]) {
            Err(_) => {
                return Err(LightningError {
                    err: format!("Invalid public key {}", &act[1..34].as_hex()),
                    action: msgs::ErrorAction::DisconnectPeer { msg: None },
                });
            }
            Ok(key) => key,
        };

        let mut sha = Sha256::engine();
        sha.input(&state.h);
        sha.input(&their_pub.serialize()[..]);
        state.h = Sha256::from_engine(sha).to_byte_array();

        let ss = SharedSecret::new(&their_pub, secret_key);
        let temp_k = PeerChannelEncryptor::hkdf(state, ss);

        let mut dec = [0; 0];
        PeerChannelEncryptor::decrypt_with_ad(&mut dec, 0, &temp_k, &state.h, &act[34..])?;

        let mut sha = Sha256::engine();
        sha.input(&state.h);
        sha.input(&act[34..]);
        state.h = Sha256::from_engine(sha).to_byte_array();

        Ok((their_pub, temp_k))
    }

    pub fn get_act_one<C: secp256k1::Signing>(&mut self, secp_ctx: &Secp256k1<C>) -> [u8; 50] {
        match self.noise_state {
            NoiseState::InProgress {
                ref mut state,
                ref directional_state,
                ref mut bidirectional_state,
            } => match directional_state {
                DirectionalNoiseState::Outbound { ie } => {
                    if *state != NoiseStep::PreActOne {
                        panic!("Requested act at wrong step");
                    }

                    let (res, _) = PeerChannelEncryptor::outbound_noise_act(
                        secp_ctx,
                        bidirectional_state,
                        ie,
                        &self.their_node_id.unwrap(),
                    );
                    *state = NoiseStep::PostActOne;
                    res
                } //_ => panic!("Wrong direction for act"),
            },
            _ => panic!("Cannot get act one after noise handshake completes"),
        }
    }
    /*
    // TODO: inbound

    pub fn _process_act_one_with_keys<C: secp256k1::Signing>(
        &mut self,
        act_one: &[u8],
        node_signer: &SecretKey,
        our_ephemeral: SecretKey,
        secp_ctx: &Secp256k1<C>,
    ) -> Result<[u8; 50], LightningError> {
        assert_eq!(act_one.len(), 50);

        match self.noise_state {
            NoiseState::InProgress {
                ref mut state,
                ref mut directional_state,
                ref mut bidirectional_state,
            } => match directional_state {
                &mut DirectionalNoiseState::Inbound {
                    ref mut ie,
                    ref mut re,
                    ref mut temp_k2,
                } => {
                    if *state != NoiseStep::PreActOne {
                        panic!("Requested act at wrong step");
                    }

                    let (their_pub, _) = PeerChannelEncryptor::inbound_noise_act(
                        bidirectional_state,
                        act_one,
                        node_signer,
                    )?;
                    ie.get_or_insert(their_pub);

                    re.get_or_insert(our_ephemeral);

                    let (res, temp_k) = PeerChannelEncryptor::outbound_noise_act(
                        secp_ctx,
                        bidirectional_state,
                        &re.unwrap(),
                        &ie.unwrap(),
                    );
                    *temp_k2 = Some(temp_k);
                    *state = NoiseStep::PostActTwo;
                    Ok(res)
                }
                _ => panic!("Wrong direction for act"),
            },
            _ => panic!("Cannot get act one after noise handshake completes"),
        }
    }
        */

    pub fn process_act_two<C: Signing>(
        &mut self,
        secp_ctx: &Secp256k1<C>,
        act_two: &[u8; 50],
        node_signer: &SecretKey,
    ) -> Result<[u8; 66], LightningError> {
        let final_hkdf;
        let ck;
        let res: [u8; 66] = match self.noise_state {
            NoiseState::InProgress {
                ref state,
                ref directional_state,
                ref mut bidirectional_state,
            } => match directional_state {
                DirectionalNoiseState::Outbound { ie } => {
                    if *state != NoiseStep::PostActOne {
                        panic!("Requested act at wrong step");
                    }

                    let (re, temp_k2) =
                        PeerChannelEncryptor::inbound_noise_act(bidirectional_state, act_two, ie)?;

                    let mut res = [0; 66];
                    let our_node_id = node_signer.public_key(secp_ctx);

                    PeerChannelEncryptor::encrypt_with_ad(
                        &mut res[1..50],
                        1,
                        &temp_k2,
                        &bidirectional_state.h,
                        &our_node_id.serialize()[..],
                    );

                    let mut sha = Sha256::engine();
                    sha.input(&bidirectional_state.h);
                    sha.input(&res[1..50]);
                    bidirectional_state.h = Sha256::from_engine(sha).to_byte_array();

                    let ss = SharedSecret::new(&re, node_signer);
                    let temp_k = PeerChannelEncryptor::hkdf(bidirectional_state, ss);

                    PeerChannelEncryptor::encrypt_with_ad(
                        &mut res[50..],
                        0,
                        &temp_k,
                        &bidirectional_state.h,
                        &[0; 0],
                    );
                    final_hkdf = hkdf_extract_expand_twice(&bidirectional_state.ck, &[0; 0]);
                    ck = bidirectional_state.ck;
                    res
                } //_ => panic!("Wrong direction for act"),
            },
            _ => panic!("Cannot get act one after noise handshake completes"),
        };

        let (sk, rk) = final_hkdf;
        self.noise_state = NoiseState::Finished {
            sk,
            sn: 0,
            sck: ck,
            rk,
            rn: 0,
            rck: ck,
        };

        Ok(res)
    }

    /*
        pub fn process_act_three(&mut self, act_three: &[u8]) -> Result<PublicKey, LightningError> {
            assert_eq!(act_three.len(), 66);

            let final_hkdf;
            let ck;
            match self.noise_state {
                NoiseState::InProgress {
                    ref state,
                    ref directional_state,
                    ref mut bidirectional_state,
                } => match directional_state {
                    &DirectionalNoiseState::Inbound {
                        ie: _,
                        ref re,
                        ref temp_k2,
                    } => {
                        if *state != NoiseStep::PostActTwo {
                            panic!("Requested act at wrong step");
                        }
                        if act_three[0] != 0 {
                            return Err(LightningError {
                                err: format!("Unknown handshake version number {}", act_three[0]),
                                action: msgs::ErrorAction::DisconnectPeer { msg: None },
                            });
                        }

                        let mut their_node_id = [0; 33];
                        PeerChannelEncryptor::decrypt_with_ad(
                            &mut their_node_id,
                            1,
                            &temp_k2.unwrap(),
                            &bidirectional_state.h,
                            &act_three[1..50],
                        )?;
                        self.their_node_id = Some(match PublicKey::from_slice(&their_node_id) {
                            Ok(key) => key,
                            Err(_) => {
                                return Err(LightningError {
                                    err: format!("Bad node_id from peer, {}", &their_node_id.as_hex()),
                                    action: msgs::ErrorAction::DisconnectPeer { msg: None },
                                });
                            }
                        });

                        let mut sha = Sha256::engine();
                        sha.input(&bidirectional_state.h);
                        sha.input(&act_three[1..50]);
                        bidirectional_state.h = Sha256::from_engine(sha).to_byte_array();

                        let ss = SharedSecret::new(&self.their_node_id.unwrap(), &re.unwrap());
                        let temp_k = PeerChannelEncryptor::hkdf(bidirectional_state, ss);

                        PeerChannelEncryptor::decrypt_with_ad(
                            &mut [0; 0],
                            0,
                            &temp_k,
                            &bidirectional_state.h,
                            &act_three[50..],
                        )?;
                        final_hkdf = hkdf_extract_expand_twice(&bidirectional_state.ck, &[0; 0]);
                        ck = bidirectional_state.ck.clone();
                    }
                    _ => panic!("Wrong direction for act"),
                },
                _ => panic!("Cannot get act one after noise handshake completes"),
            }

            let (rk, sk) = final_hkdf;
            self.noise_state = NoiseState::Finished {
                sk,
                sn: 0,
                sck: ck.clone(),
                rk,
                rn: 0,
                rck: ck,
            };

            Ok(self.their_node_id.unwrap().clone())
        }
    */

    /// Builds sendable bytes for a message.
    ///
    /// `msgbuf` must begin with 16 + 2 dummy/0 bytes, which will be filled with the encrypted
    /// message length and its MAC. It should then be followed by the message bytes themselves
    /// (including the two byte message type).
    ///
    /// For effeciency, the [`Vec::capacity`] should be at least 16 bytes larger than the
    /// [`Vec::len`], to avoid reallocating for the message MAC, which will be appended to the vec.
    fn encrypt_message_with_header_0s(&mut self, msgbuf: &mut Vec<u8>) {
        let msg_len = msgbuf.len() - 16 - 2;
        if msg_len > LN_MAX_MSG_LEN {
            panic!("Attempted to encrypt message longer than 65535 bytes!");
        }

        match self.noise_state {
            NoiseState::Finished {
                ref mut sk,
                ref mut sn,
                ref mut sck,
                rk: _,
                rn: _,
                rck: _,
            } => {
                if *sn >= 1000 {
                    let (new_sck, new_sk) = hkdf_extract_expand_twice(sck, sk);
                    *sck = new_sck;
                    *sk = new_sk;
                    *sn = 0;
                }

                Self::encrypt_with_ad(
                    &mut msgbuf[0..16 + 2],
                    *sn,
                    sk,
                    &[0; 0],
                    &(msg_len as u16).to_be_bytes(),
                );
                *sn += 1;

                Self::encrypt_in_place_with_ad(msgbuf, 16 + 2, *sn, sk, &[0; 0]);
                *sn += 1;
            }
            _ => panic!("Tried to encrypt a message prior to noise handshake completion"),
        }
    }

    /*
    /// Encrypts the given pre-serialized message, returning the encrypted version.
    /// panics if msg.len() > 65535 or Noise handshake has not finished.
    pub fn encrypt_buffer(&mut self, mut msg: MessageBuf) -> Vec<u8> {
        self.encrypt_message_with_header_0s(&mut msg.0);
        msg.0
    }
    */

    /// Encrypts the given message, returning the encrypted version.
    /// panics if the length of `message`, once encoded, is greater than 65535 or if the Noise
    /// handshake has not finished.
    pub fn encrypt_message<M: wire::Type + Writeable>(&mut self, message: &M) -> Vec<u8> {
        // Allocate a buffer with 2KB, fitting most common messages. Reserve the first 16+2 bytes
        // for the 2-byte message type prefix and its MAC.
        let mut res = VecWriter(Vec::with_capacity(MSG_BUF_ALLOC_SIZE));
        res.0.resize(16 + 2, 0);
        wire::write(message, &mut res).expect("In-memory messages must never fail to serialize");

        self.encrypt_message_with_header_0s(&mut res.0);
        res.0
    }

    /// Decrypts a message length header from the remote peer.
    /// panics if noise handshake has not yet finished or msg.len() != 18
    pub fn decrypt_length_header(&mut self, msg: &[u8; 18]) -> Result<u16, LightningError> {
        match self.noise_state {
            NoiseState::Finished {
                sk: _,
                sn: _,
                sck: _,
                ref mut rk,
                ref mut rn,
                ref mut rck,
            } => {
                if *rn >= 1000 {
                    let (new_rck, new_rk) = hkdf_extract_expand_twice(rck, rk);
                    *rck = new_rck;
                    *rk = new_rk;
                    *rn = 0;
                }

                let mut res = [0; 2];
                Self::decrypt_with_ad(&mut res, *rn, rk, &[0; 0], msg)?;
                *rn += 1;
                Ok(u16::from_be_bytes(res))
            }
            _ => panic!("Tried to decrypt a message prior to noise handshake completion"),
        }
    }

    /// Decrypts the given message up to msg.len() - 16. Bytes after msg.len() - 16 will be left
    /// undefined (as they contain the Poly1305 tag bytes).
    ///
    /// panics if msg.len() > 65535 + 16
    pub fn decrypt_message(&mut self, msg: &mut [u8]) -> Result<(), LightningError> {
        if msg.len() > LN_MAX_MSG_LEN + 16 {
            panic!("Attempted to decrypt message longer than 65535 + 16 bytes!");
        }

        match self.noise_state {
            NoiseState::Finished {
                sk: _,
                sn: _,
                sck: _,
                ref rk,
                ref mut rn,
                rck: _,
            } => {
                Self::decrypt_in_place_with_ad(&mut msg[..], *rn, rk, &[0; 0])?;
                *rn += 1;
                Ok(())
            }
            _ => panic!("Tried to decrypt a message prior to noise handshake completion"),
        }
    }

    /*
    //TODO: inbound
    pub fn is_ready_for_encryption(&self) -> bool {
        match self.noise_state {
            NoiseState::InProgress { .. } => false,
            NoiseState::Finished { .. } => true,
        }
    }
    */
}

// TODO: inbound
/*
/// A buffer which stores an encoded message (including the two message-type bytes) with some
/// padding to allow for future encryption/MACing.
pub struct MessageBuf(Vec<u8>);
impl MessageBuf {
    /// Creates a new buffer from an encoded message (i.e. the two message-type bytes followed by
    /// the message contents).
    ///
    /// Panics if the message is longer than 2^16.
    pub fn from_encoded(encoded_msg: &[u8]) -> Self {
        if encoded_msg.len() > LN_MAX_MSG_LEN {
            panic!("Attempted to encrypt message longer than 65535 bytes!");
        }
        // In addition to the message (continaing the two message type bytes), we also have to add
        // the message length header (and its MAC) and the message MAC.
        let mut res = Vec::with_capacity(encoded_msg.len() + 16 * 2 + 2);
        res.resize(encoded_msg.len() + 16 + 2, 0);
        res[16 + 2..].copy_from_slice(&encoded_msg);
        Self(res)
    }
}
*/
