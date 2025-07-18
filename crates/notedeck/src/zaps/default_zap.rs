use tokenator::{ParseError, TokenParser, TokenSerializable};

use crate::get_current_wallet;

const DEFAULT_ZAP_MSATS: u64 = 10_000;

#[derive(Debug, Default, Clone)]
pub struct DefaultZapMsats {
    pub msats: Option<u64>,
    pub pending: PendingDefaultZapState,
}

impl DefaultZapMsats {
    pub fn from_msats(msats: Option<u64>) -> Self {
        let mut default = DefaultZapMsats::default();

        if let Some(msats) = msats {
            default.set_user_selection(msats);
            default.pending.write_msats(msats);
        }

        default
    }
    pub fn from_user(value: Option<UserZapMsats>) -> Self {
        let mut obj = match value {
            Some(user_msats) => {
                let mut val = DefaultZapMsats::default();
                val.set_user_selection(user_msats.msats);
                val
            }
            None => DefaultZapMsats::default(),
        };

        obj.pending.write_msats(obj.get_default_zap_msats());
        obj
    }

    pub fn set_user_selection(&mut self, msats: u64) {
        self.msats = Some(msats);
    }

    pub fn get_default_zap_msats(&self) -> u64 {
        let Some(default_zap_msats) = self.msats else {
            return DEFAULT_ZAP_MSATS;
        };

        default_zap_msats
    }

    pub fn has_user_selection(&self) -> bool {
        self.msats.is_some()
    }

    pub fn try_into_user(&self) -> Option<UserZapMsats> {
        let user_zap_amount = self.msats?;

        Some(UserZapMsats {
            msats: user_zap_amount,
        })
    }
}

#[derive(Debug, Clone)]
pub struct UserZapMsats {
    pub msats: u64,
}

impl TokenSerializable for UserZapMsats {
    fn parse_from_tokens<'a>(parser: &mut TokenParser<'a>) -> Result<Self, ParseError<'a>> {
        parser.parse_token("default_zap")?;

        let msats: u64 = parser
            .pull_token()?
            .parse()
            .map_err(|_| ParseError::DecodeFailed)?;

        Ok(UserZapMsats { msats })
    }

    fn serialize_tokens(&self, writer: &mut tokenator::TokenWriter) {
        writer.write_token("default_zap");
        writer.write_token(&self.msats.to_string());
    }
}

#[derive(Debug, Clone)]
pub struct PendingDefaultZapState {
    pub amount_sats: String,
    pub error_message: Option<DefaultZapError>,
    pub is_rewriting: bool,
}

impl Default for PendingDefaultZapState {
    fn default() -> Self {
        Self {
            amount_sats: msats_to_sats_string(DEFAULT_ZAP_MSATS),
            error_message: Default::default(),
            is_rewriting: Default::default(),
        }
    }
}

impl PendingDefaultZapState {
    pub fn write_msats(&mut self, msats: u64) {
        self.amount_sats = msats_to_sats_string(msats);
    }
}

fn msats_to_sats_string(msats: u64) -> String {
    (msats / 1000).to_string()
}

#[derive(Debug, Clone)]
pub enum DefaultZapError {
    InvalidUserInput,
}

pub fn get_current_default_msats<'a>(
    accounts: &'a mut crate::Accounts,
    global_wallet: &'a mut crate::GlobalWallet,
) -> u64 {
    get_current_wallet(accounts, global_wallet)
        .map(|w| w.default_zap.get_default_zap_msats())
        .unwrap_or_else(|| crate::zaps::default_zap::DEFAULT_ZAP_MSATS)
}
