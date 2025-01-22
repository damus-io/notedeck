use super::{AccountLoginResponse, AccountsViewResponse};
use serde::{Deserialize, Serialize};
use tokenator::{ParseError, TokenParser, TokenSerializable, TokenWriter};

pub enum AccountsRouteResponse {
    Accounts(AccountsViewResponse),
    AddAccount(AccountLoginResponse),
}

#[derive(Debug, Eq, PartialEq, Clone, Copy, Serialize, Deserialize)]
pub enum AccountsRoute {
    Accounts,
    AddAccount,
}

impl AccountsRoute {
    /// Route tokens use in both serialization and deserialization
    fn tokens(&self) -> &'static [&'static str] {
        match self {
            Self::Accounts => &["accounts", "show"],
            Self::AddAccount => &["accounts", "new"],
        }
    }
}

impl TokenSerializable for AccountsRoute {
    fn serialize_tokens(&self, writer: &mut TokenWriter) {
        for token in self.tokens() {
            writer.write_token(token);
        }
    }

    fn parse_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Self, ParseError<'a>> {
        parser.peek_parse_token("accounts")?;

        TokenParser::alt(
            parser,
            &[
                |p| parse_accounts_route(p, AccountsRoute::Accounts),
                |p| parse_accounts_route(p, AccountsRoute::AddAccount),
            ],
        )
    }
}

fn parse_accounts_route<'a>(
    parser: &mut TokenParser<'a>,
    route: AccountsRoute,
) -> Result<AccountsRoute, ParseError<'a>> {
    parser.parse_all(|p| {
        for token in route.tokens() {
            p.parse_token(token)?;
        }
        Ok(route)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokenator::{TokenParser, TokenSerializable, TokenWriter};

    #[test]
    fn test_accounts_route_serialize() {
        let data_str = "accounts:show";
        let data = &data_str.split(":").collect::<Vec<&str>>();
        let mut token_writer = TokenWriter::default();
        let mut parser = TokenParser::new(&data);
        let parsed = AccountsRoute::parse_from_tokens(&mut parser).unwrap();
        let expected = AccountsRoute::Accounts;
        parsed.serialize_tokens(&mut token_writer);
        assert_eq!(expected, parsed);
        assert_eq!(token_writer.str(), data_str);
    }

    #[test]
    fn test_new_accounts_route_serialize() {
        let data_str = "accounts:new";
        let data = &data_str.split(":").collect::<Vec<&str>>();
        let mut token_writer = TokenWriter::default();
        let mut parser = TokenParser::new(data);
        let parsed = AccountsRoute::parse_from_tokens(&mut parser).unwrap();
        let expected = AccountsRoute::AddAccount;
        parsed.serialize_tokens(&mut token_writer);
        assert_eq!(expected, parsed);
        assert_eq!(token_writer.str(), data_str);
    }
}
