use std::{fmt::Display, sync::Arc};

use nwc::{
    nostr::nips::nip47::{NostrWalletConnectURI, PayInvoiceRequest, PayInvoiceResponse},
    NWC,
};
use poll_promise::Promise;
use tokenator::{ParseError, TokenParser, TokenSerializable};
use tokio::sync::RwLock;

use crate::{zaps::UserZapMsats, Accounts, DataPath, DefaultZapMsats, TokenHandler};

pub fn get_wallet_for<'a>(
    accounts: &'a Accounts,
    global_wallet: &'a mut GlobalWallet,
    account_pk: &'a [u8; 32],
) -> Option<&'a ZapWallet> {
    let cur_acc = accounts.cache.get_bytes(account_pk)?;

    if let Some(wallet) = &cur_acc.wallet {
        return Some(wallet);
    }

    global_wallet.wallet.as_ref()
}

pub fn get_current_wallet<'a>(
    accounts: &'a mut Accounts,
    global_wallet: &'a mut GlobalWallet,
) -> Option<&'a mut ZapWallet> {
    let Some(wallet) = accounts.get_selected_wallet_mut() else {
        return global_wallet.wallet.as_mut();
    };

    Some(wallet)
}

#[derive(Clone, Eq, PartialEq, Debug)]
pub enum WalletType {
    Auto,
    Local,
}

#[derive(Default, Debug)]
pub struct WalletUIState {
    pub buf: String,
    pub error_msg: Option<WalletError>,
    pub for_local_only: bool,
}

#[derive(Debug)]
pub enum WalletError {
    InvalidURI,
    NoWallet,
}

pub struct Wallet {
    pub uri: String,
    wallet: Arc<RwLock<NWC>>,
    balance: Option<Promise<Result<u64, NwcError>>>,
}

impl Clone for Wallet {
    fn clone(&self) -> Self {
        Self {
            uri: self.uri.clone(),
            wallet: self.wallet.clone(),
            balance: None,
        }
    }
}

#[derive(Clone)]
pub struct WalletSerializable {
    pub uri: String,
    pub default_mzap: Option<UserZapMsats>,
}

impl WalletSerializable {
    pub fn new(uri: String) -> Self {
        Self {
            uri,
            default_mzap: None,
        }
    }
}

impl std::fmt::Debug for Wallet {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Wallet({})", self.uri)
    }
}

impl Wallet {
    pub fn new(uri: String) -> Result<Self, crate::Error> {
        let nwc_uri = NostrWalletConnectURI::parse(uri.clone())
            .map_err(|e| crate::Error::Generic(e.to_string()))?;

        let nwc = NWC::new(nwc_uri);

        Ok(Self {
            uri,
            wallet: Arc::new(RwLock::new(nwc)),
            balance: Default::default(),
        })
    }

    pub fn get_balance(&mut self) -> Option<&Result<u64, NwcError>> {
        if self.balance.is_none() {
            self.balance = Some(get_balance(self.wallet.clone()));
            return None;
        }
        let promise = self.balance.as_ref().unwrap();

        if let Some(bal) = promise.ready() {
            Some(bal)
        } else {
            None
        }
    }

    pub fn pay_invoice(&self, invoice: &str) -> Promise<Result<PayInvoiceResponse, nwc::Error>> {
        pay_invoice(
            self.wallet.clone(),
            PayInvoiceRequest::new(invoice.to_owned()),
        )
    }
}

#[derive(Clone)]
pub enum NwcError {
    /// NIP47 error
    NIP47(String),
    /// Relay
    Relay(String),
    /// Premature exit
    PrematureExit,
    /// Request timeout
    Timeout,
}

impl From<nwc::Error> for NwcError {
    fn from(value: nwc::Error) -> Self {
        match value {
            nwc::error::Error::NIP47(error) => NwcError::NIP47(error.to_string()),
            nwc::error::Error::Relay(error) => NwcError::Relay(error.to_string()),
            nwc::error::Error::PrematureExit => NwcError::PrematureExit,
            nwc::error::Error::Timeout => NwcError::Timeout,
        }
    }
}

impl Display for NwcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NwcError::NIP47(err) => write!(f, "NIP47 error: {err}"),
            NwcError::Relay(err) => write!(f, "Relay error: {err}"),
            NwcError::PrematureExit => write!(f, "Premature exit"),
            NwcError::Timeout => write!(f, "Request timed out"),
        }
    }
}

fn get_balance(nwc: Arc<RwLock<NWC>>) -> Promise<Result<u64, NwcError>> {
    let (sender, promise) = Promise::new();

    tokio::spawn(async move {
        sender.send(
            nwc.read()
                .await
                .get_balance()
                .await
                .map_err(nwc::Error::into),
        );
    });

    promise
}

fn pay_invoice(
    nwc: Arc<RwLock<NWC>>,
    invoice: PayInvoiceRequest,
) -> Promise<Result<PayInvoiceResponse, nwc::Error>> {
    let (sender, promise) = Promise::new();

    tokio::spawn(async move {
        sender.send(nwc.read().await.pay_invoice(invoice).await);
    });

    promise
}

pub struct GlobalWallet {
    pub wallet: Option<ZapWallet>,
    pub ui_state: WalletUIState,
    wallet_handler: TokenHandler,
}

impl GlobalWallet {
    pub fn new(path: &DataPath) -> Self {
        let wallet_handler =
            TokenHandler::new(path, crate::DataPathType::Setting, "global_wallet.txt");
        let wallet = construct_global_wallet(&wallet_handler);

        Self {
            wallet,
            ui_state: WalletUIState::default(),
            wallet_handler,
        }
    }

    pub fn save_wallet(&self) {
        let Some(wallet) = &self.wallet else {
            // saving with no wallet means delete
            if let Err(e) = self.wallet_handler.clear() {
                tracing::error!("Could not clear wallet: {e}");
            }

            return;
        };

        let serializable: WalletSerializable = wallet.into();
        match self.wallet_handler.save(&serializable, "\t") {
            Ok(_) => {}
            Err(e) => tracing::error!("Could not save global wallet: {e}"),
        }
    }
}

fn construct_global_wallet(wallet_handler: &TokenHandler) -> Option<ZapWallet> {
    let Ok(res) = wallet_handler.load::<WalletSerializable>("\t") else {
        return None;
    };

    let wallet = match res {
        Ok(wallet) => {
            let m_zap_wallet: Result<ZapWallet, crate::Error> = wallet.into();
            m_zap_wallet.ok()?
        }
        Err(e) => {
            tracing::error!("Error parsing wallet: {:?}", e);
            return None;
        }
    };

    Some(wallet)
}

#[derive(Debug, Clone)]
pub struct ZapWallet {
    pub wallet: Wallet,
    pub default_zap: DefaultZapMsats,
}

enum ZapWalletRoute {
    Wallet(String),
    DefaultZapMsats(UserZapMsats),
}

impl ZapWallet {
    pub fn new(wallet: Wallet) -> Self {
        Self {
            wallet,
            default_zap: DefaultZapMsats::default(),
        }
    }

    pub fn with_default_zap_msats(mut self, msats: u64) -> Self {
        self.default_zap.set_user_selection(msats);
        self
    }
}

impl From<Wallet> for ZapWallet {
    fn from(value: Wallet) -> Self {
        ZapWallet::new(value)
    }
}

impl From<&ZapWallet> for WalletSerializable {
    fn from(value: &ZapWallet) -> Self {
        Self {
            uri: value.wallet.uri.to_string(),
            default_mzap: value.default_zap.try_into_user(),
        }
    }
}

impl From<WalletSerializable> for Result<ZapWallet, crate::Error> {
    fn from(value: WalletSerializable) -> Result<ZapWallet, crate::Error> {
        Ok(ZapWallet {
            wallet: Wallet::new(value.uri)?,
            default_zap: DefaultZapMsats::from_user(value.default_mzap),
        })
    }
}

impl TokenSerializable for WalletSerializable {
    fn parse_from_tokens<'a>(
        parser: &mut tokenator::TokenParser<'a>,
    ) -> Result<Self, tokenator::ParseError<'a>> {
        let mut m_wallet = None;
        let mut m_default_zap = None;
        loop {
            let res = TokenParser::alt(
                parser,
                &[
                    |p| {
                        p.parse_token("nwc_uri")?;
                        let raw_uri = p.pull_token()?;

                        Ok(ZapWalletRoute::Wallet(raw_uri.to_string()))
                    },
                    |p| {
                        Ok(ZapWalletRoute::DefaultZapMsats(
                            UserZapMsats::parse_from_tokens(p)?,
                        ))
                    },
                ],
            );

            match res {
                Ok(ZapWalletRoute::Wallet(wallet)) => m_wallet = Some(wallet),
                Ok(ZapWalletRoute::DefaultZapMsats(msats)) => m_default_zap = Some(msats),
                Err(ParseError::AltAllFailed) => break,
                Err(_) => {}
            }

            if m_wallet.is_some() && m_default_zap.is_some() {
                break;
            }
        }

        let Some(wallet) = m_wallet else {
            return Err(ParseError::DecodeFailed);
        };

        Ok(WalletSerializable {
            uri: wallet,
            default_mzap: m_default_zap,
        })
    }

    fn serialize_tokens(&self, writer: &mut tokenator::TokenWriter) {
        writer.write_token("nwc_uri");
        writer.write_token(&self.uri);

        if let Some(msats) = &self.default_mzap {
            msats.serialize_tokens(writer);
        }
    }
}

#[cfg(test)]
mod tests {
    use tokenator::{TokenParser, TokenSerializable, TokenWriter};

    use crate::{wallet::WalletSerializable, Wallet};

    use super::ZapWallet;

    const URI: &str = "nostr+walletconnect://b889ff5b1513b641e2a139f661a661364979c5beee91842f8f0ef42ab558e9d4?relay=wss%3A%2F%2Frelay.damus.io&secret=71a8c14c1407c113601079c4302dab36460f0ccd0ad506f1f2dc73b5100e4f3c&lud16=nostr%40nostr.com";

    #[test]

    fn test_uri() {
        assert!(Wallet::new(URI.to_owned()).is_ok())
    }

    #[test]
    fn test_wallet_serialize_deserialize() {
        let wallet = WalletSerializable::new(URI.to_owned());

        let mut writer = TokenWriter::new("\t");
        wallet.serialize_tokens(&mut writer);
        let serialized = writer.str();

        let data = &serialized.split("\t").collect::<Vec<&str>>();
        let mut parser = TokenParser::new(data);
        let m_new_wallet = WalletSerializable::parse_from_tokens(&mut parser);

        assert!(m_new_wallet.is_ok());

        let new_wallet = m_new_wallet.unwrap();

        assert_eq!(wallet.uri, new_wallet.uri);
    }

    #[test]
    fn test_zap_wallet_serialize_deserialize() {
        const MSATS: u64 = 64_000;
        let zap_wallet =
            ZapWallet::new(Wallet::new(URI.to_owned()).unwrap()).with_default_zap_msats(MSATS);

        let mut writer = TokenWriter::new("\t");

        let serializable: WalletSerializable = (&zap_wallet).into();
        serializable.serialize_tokens(&mut writer);
        let serialized = writer.str();

        let data = &serialized.split("\t").collect::<Vec<&str>>();
        let mut parser = TokenParser::new(data);

        let m_deserialized = WalletSerializable::parse_from_tokens(&mut parser);
        assert!(m_deserialized.is_ok());

        let deserialized = m_deserialized.unwrap();

        let m_new_zap_wallet: Result<ZapWallet, crate::Error> = deserialized.into();

        assert!(m_new_zap_wallet.is_ok());

        let new_zap_wallet = m_new_zap_wallet.unwrap();

        assert_eq!(zap_wallet.wallet.uri, new_zap_wallet.wallet.uri);
        assert_eq!(new_zap_wallet.default_zap.get_default_zap_msats(), MSATS);
    }
}
