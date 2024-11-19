use super::{AccountLoginResponse, AccountsViewResponse};
use serde::{Deserialize, Serialize};

pub enum AccountsRouteResponse {
    Accounts(AccountsViewResponse),
    AddAccount(AccountLoginResponse),
}

#[derive(Debug, Eq, PartialEq, Clone, Copy, Serialize, Deserialize)]
pub enum AccountsRoute {
    Accounts,
    AddAccount,
}
