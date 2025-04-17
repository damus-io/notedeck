use std::sync::Arc;

use nwc::{
    nostr::nips::nip47::{NostrWalletConnectURI, PayInvoiceRequest, PayInvoiceResponse},
    NWC,
};
use poll_promise::Promise;
use tokenator::TokenSerializable;
use tokio::sync::RwLock;

use crate::{Accounts, DataPath, TokenHandler};

#[derive(Debug)]
pub enum WalletState<'a> {
    Wallet {
        wallet: &'a mut Wallet,
        can_create_local_wallet: bool,
    },
    NoWallet {
        state: &'a mut WalletUIState,
        show_local_only: bool,
    },
}

pub fn get_wallet_for_mut<'a>(
    accounts: &'a mut Accounts,
    global_wallet: &'a mut GlobalWallet,
    account_pk: &'a [u8; 32],
) -> Option<&'a mut Wallet> {
    let cur_acc = accounts.get_account_mut_optimized(account_pk)?;

    if let Some(wallet) = &mut cur_acc.wallet {
        return Some(wallet);
    }

    global_wallet.wallet.as_mut()
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
}

pub struct Wallet {
    pub uri: String,
    wallet: Arc<RwLock<NWC>>,
    balance: Option<Promise<Result<u64, nwc::Error>>>,
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

    pub fn get_balance(&mut self) -> Option<&Result<u64, nwc::Error>> {
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

    pub fn pay_invoice(
        &mut self,
        invoice: &str,
    ) -> Promise<Result<PayInvoiceResponse, nwc::Error>> {
        pay_invoice(
            self.wallet.clone(),
            PayInvoiceRequest::new(invoice.to_owned()),
        )
    }
}

fn get_balance(nwc: Arc<RwLock<NWC>>) -> Promise<Result<u64, nwc::Error>> {
    let (sender, promise) = Promise::new();

    tokio::spawn(async move {
        sender.send(nwc.read().await.get_balance().await);
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

impl TokenSerializable for Wallet {
    fn parse_from_tokens<'a>(
        parser: &mut tokenator::TokenParser<'a>,
    ) -> Result<Self, tokenator::ParseError<'a>> {
        parser.parse_token("nwc_uri")?;

        let raw_uri = parser.pull_token()?;

        let wallet =
            Wallet::new(raw_uri.to_owned()).map_err(|_| tokenator::ParseError::DecodeFailed)?;

        Ok(wallet)
    }

    fn serialize_tokens(&self, writer: &mut tokenator::TokenWriter) {
        writer.write_token("nwc_uri");
        writer.write_token(&self.uri);
    }
}

pub struct GlobalWallet {
    pub wallet: Option<Wallet>,
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

        match self.wallet_handler.save(wallet, "\t") {
            Ok(_) => {}
            Err(e) => tracing::error!("Could not save global wallet: {e}"),
        }
    }
}

fn construct_global_wallet(wallet_handler: &TokenHandler) -> Option<Wallet> {
    let Ok(res) = wallet_handler.load::<Wallet>("\t") else {
        return None;
    };

    let wallet = match res {
        Ok(wallet) => wallet,
        Err(e) => {
            tracing::error!("Error parsing wallet: {:?}", e);
            return None;
        }
    };

    Some(wallet)
}

#[cfg(test)]
mod tests {
    use tokenator::{TokenParser, TokenSerializable, TokenWriter};

    use crate::Wallet;

    const URI: &str = "nostr+walletconnect://b889ff5b1513b641e2a139f661a661364979c5beee91842f8f0ef42ab558e9d4?relay=wss%3A%2F%2Frelay.damus.io&secret=71a8c14c1407c113601079c4302dab36460f0ccd0ad506f1f2dc73b5100e4f3c&lud16=nostr%40nostr.com";

    #[test]

    fn test_uri() {
        assert!(Wallet::new(URI.to_owned()).is_ok())
    }

    #[test]
    fn test_wallet_serialize_deserialize() {
        let wallet = Wallet::new(URI.to_owned()).unwrap();

        let mut writer = TokenWriter::new("\t");
        wallet.serialize_tokens(&mut writer);
        let serialized = writer.str();

        let data = &serialized.split("\t").collect::<Vec<&str>>();
        let mut parser = TokenParser::new(data);
        let m_new_wallet = Wallet::parse_from_tokens(&mut parser);

        assert!(m_new_wallet.is_ok());

        let new_wallet = m_new_wallet.unwrap();

        assert_eq!(wallet.uri, new_wallet.uri);
    }
}
