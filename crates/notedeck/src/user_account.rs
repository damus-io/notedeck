use enostr::Keypair;
use tokenator::{ParseError, TokenParser, TokenSerializable};

pub struct UserAccount {
    pub key: Keypair,
}

impl UserAccount {
    pub fn new(key: Keypair) -> Self {
        Self { key }
    }
}

enum UserAccountRoute {
    Key(Keypair),
}

impl TokenSerializable for UserAccount {
    fn parse_from_tokens<'a>(
        parser: &mut tokenator::TokenParser<'a>,
    ) -> Result<Self, tokenator::ParseError<'a>> {
        let mut m_key = None;

        loop {
            let res = TokenParser::alt(
                parser,
                &[|p| Ok(UserAccountRoute::Key(Keypair::parse_from_tokens(p)?))],
            );

            match res {
                Ok(UserAccountRoute::Key(key)) => m_key = Some(key),
                Err(ParseError::AltAllFailed) => break,
                Err(_) => {}
            }

            if m_key.is_some() {
                break;
            }
        }

        let Some(key) = m_key else {
            return Err(ParseError::DecodeFailed);
        };

        Ok(UserAccount { key })
    }

    fn serialize_tokens(&self, writer: &mut tokenator::TokenWriter) {
        self.key.serialize_tokens(writer);
    }
}

#[cfg(test)]
mod tests {
    use enostr::FullKeypair;
    use tokenator::{TokenParser, TokenSerializable, TokenWriter};

    use super::UserAccount;

    #[test]
    fn test_user_account_serialize_deserialize() {
        let kp = FullKeypair::generate();
        let acc = UserAccount {
            key: kp.to_keypair(),
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
    }
}
