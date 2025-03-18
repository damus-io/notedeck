use crate::{Wallet, WalletState};
use enostr::Keypair;
use tokenator::{ParseError, TokenParser, TokenSerializable};

#[derive(Debug)]
pub struct UserAccount {
    pub key: Keypair,
    pub wallet_state: WalletState,
}

impl UserAccount {
    pub fn new(key: Keypair) -> Self {
        Self {
            key,
            wallet_state: WalletState::default(),
        }
    }
}

enum UserAccountRoute {
    Key(Keypair),
    Wallet(Wallet),
}

impl TokenSerializable for UserAccount {
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
                    |p| Ok(UserAccountRoute::Wallet(Wallet::parse_from_tokens(p)?)),
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
        let wallet_state = match m_wallet {
            Some(wallet) => WalletState::Wallet(wallet),
            None => WalletState::default(),
        };

        Ok(UserAccount { key, wallet_state })
    }

    fn serialize_tokens(&self, writer: &mut tokenator::TokenWriter) {
        self.key.serialize_tokens(writer);

        let WalletState::Wallet(wallet) = &self.wallet_state else {
            return;
        };

        wallet.serialize_tokens(writer);
    }
}

#[cfg(test)]
mod tests {
    use enostr::FullKeypair;
    use tokenator::{TokenParser, TokenSerializable, TokenWriter};

    use crate::{Wallet, WalletState};

    use super::UserAccount;

    const URI: &str = "nostr+walletconnect://b889ff5b1513b641e2a139f661a661364979c5beee91842f8f0ef42ab558e9d4?relay=wss%3A%2F%2Frelay.damus.io&secret=71a8c14c1407c113601079c4302dab36460f0ccd0ad506f1f2dc73b5100e4f3c&lud16=nostr%40nostr.com";

    #[test]
    fn test_user_account_serialize_deserialize() {
        let kp = FullKeypair::generate();
        let acc = UserAccount {
            key: kp.to_keypair(),
            wallet_state: crate::WalletState::Wallet(Wallet::new(URI.to_owned()).unwrap()),
        };

        let mut writer = TokenWriter::new("\t");
        acc.serialize_tokens(&mut writer);

        let serialized = writer.str();

        let data = &serialized.split("\t").collect::<Vec<&str>>();
        let mut parser = TokenParser::new(data);
        let m_new_acc = UserAccount::parse_from_tokens(&mut parser);

        assert!(m_new_acc.is_ok());
        let new_acc = m_new_acc.unwrap();

        assert_eq!(acc.key, new_acc.key);

        let WalletState::Wallet(new_wallet) = new_acc.wallet_state else {
            panic!()
        };
        assert_eq!(new_wallet.uri, URI);
    }
}
