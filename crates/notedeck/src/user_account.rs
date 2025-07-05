use enostr::{Keypair, KeypairUnowned, Pubkey};
use tokenator::{ParseError, TokenParser, TokenSerializable};

use crate::{
    wallet::{WalletSerializable, ZapWallet},
    AccountData, IsFollowing,
};

pub struct UserAccount {
    pub key: Keypair,
    pub wallet: Option<ZapWallet>,
    pub data: AccountData,
}

impl UserAccount {
    pub fn new(key: Keypair, data: AccountData) -> Self {
        Self {
            key,
            wallet: None,
            data,
        }
    }

    pub fn keypair(&self) -> KeypairUnowned {
        KeypairUnowned {
            pubkey: &self.key.pubkey,
            secret_key: self.key.secret_key.as_ref(),
        }
    }

    pub fn with_wallet(mut self, wallet: ZapWallet) -> Self {
        self.wallet = Some(wallet);
        self
    }

    pub fn is_following(&self, pk: &Pubkey) -> IsFollowing {
        self.data.contacts.is_following(pk)
    }
}

pub struct UserAccountSerializable {
    pub key: Keypair,
    pub wallet: Option<WalletSerializable>,
}

impl UserAccountSerializable {
    pub fn new(key: Keypair) -> Self {
        Self { key, wallet: None }
    }

    pub fn with_wallet(mut self, wallet: WalletSerializable) -> Self {
        self.wallet = Some(wallet);
        self
    }
}

impl From<&UserAccount> for UserAccountSerializable {
    fn from(value: &UserAccount) -> Self {
        Self {
            key: value.key.clone(),
            wallet: value.wallet.as_ref().map(|z| z.into()),
        }
    }
}

enum UserAccountRoute {
    Key(Keypair),
    Wallet(WalletSerializable),
}

impl TokenSerializable for UserAccountSerializable {
    fn parse_from_tokens<'a>(
        parser: &mut tokenator::TokenParser<'a>,
    ) -> Result<Self, tokenator::ParseError<'a>> {
        let mut m_key = None;
        let mut m_wallet = None;

        loop {
            let res = TokenParser::alt(
                parser,
                &[
                    |p| Ok(UserAccountRoute::Key(Keypair::parse_from_tokens(p)?)),
                    |p| {
                        Ok(UserAccountRoute::Wallet(
                            WalletSerializable::parse_from_tokens(p)?,
                        ))
                    },
                ],
            );

            match res {
                Ok(UserAccountRoute::Key(key)) => m_key = Some(key),
                Ok(UserAccountRoute::Wallet(wallet)) => m_wallet = Some(wallet),
                Err(ParseError::AltAllFailed) => break,
                Err(_) => {}
            }

            if m_key.is_some() && m_wallet.is_some() {
                break;
            }
        }

        let Some(key) = m_key else {
            return Err(ParseError::DecodeFailed);
        };

        let mut user_acc = UserAccountSerializable::new(key);

        if let Some(wallet) = m_wallet {
            user_acc = user_acc.with_wallet(wallet);
        };

        Ok(user_acc)
    }

    fn serialize_tokens(&self, writer: &mut tokenator::TokenWriter) {
        self.key.serialize_tokens(writer);

        let Some(wallet) = &self.wallet else {
            return;
        };

        wallet.serialize_tokens(writer);
    }
}

#[cfg(test)]
mod tests {
    use enostr::FullKeypair;
    use tokenator::{TokenParser, TokenSerializable, TokenWriter};

    use crate::{user_account::UserAccountSerializable, wallet::WalletSerializable};

    const URI: &str = "nostr+walletconnect://b889ff5b1513b641e2a139f661a661364979c5beee91842f8f0ef42ab558e9d4?relay=wss%3A%2F%2Frelay.damus.io&secret=71a8c14c1407c113601079c4302dab36460f0ccd0ad506f1f2dc73b5100e4f3c&lud16=nostr%40nostr.com";

    #[test]
    fn test_user_account_serialize_deserialize() {
        let kp = FullKeypair::generate();
        let acc = UserAccountSerializable::new(kp.to_keypair())
            .with_wallet(WalletSerializable::new(URI.to_owned()));

        let mut writer = TokenWriter::new("\t");
        acc.serialize_tokens(&mut writer);

        let serialized = writer.str();

        let data = &serialized.split("\t").collect::<Vec<&str>>();
        let mut parser = TokenParser::new(data);
        let m_new_acc = UserAccountSerializable::parse_from_tokens(&mut parser);

        assert!(m_new_acc.is_ok());
        let new_acc = m_new_acc.unwrap();

        assert_eq!(acc.key, new_acc.key);

        let Some(wallet) = new_acc.wallet else {
            panic!();
        };

        assert_eq!(wallet.uri, URI);
    }
}
