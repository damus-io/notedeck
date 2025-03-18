use crate::WalletState;
use enostr::Keypair;
use tokenator::TokenSerializable;

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

impl TokenSerializable for UserAccount {
    fn parse_from_tokens<'a>(
        parser: &mut tokenator::TokenParser<'a>,
    ) -> Result<Self, tokenator::ParseError<'a>> {
        Ok(UserAccount::new(Keypair::parse_from_tokens(parser)?))
    }

    fn serialize_tokens(&self, writer: &mut tokenator::TokenWriter) {
        self.key.serialize_tokens(writer);
    }
}
