use std::sync::Arc;

use nwc::{
    nostr::nips::nip47::{NostrWalletConnectURI, PayInvoiceRequest, PayInvoiceResponse},
    NWC,
};
use poll_promise::Promise;
use tokio::sync::RwLock;

use crate::{Accounts, Job, JobId, Jobs};

#[derive(Debug)]
pub enum WalletState {
    Wallet(Wallet),
    NoWallet(NoWallet),
}

#[derive(Default, Debug)]
pub struct NoWallet {
    pub buf: String,
    pub error_msg: Option<WalletError>,
}

#[derive(Debug)]
pub enum WalletError {
    InvalidURI,
}

pub struct Wallet {
    pub uri: String,
    wallet: Arc<RwLock<NWC>>,
}

impl std::fmt::Debug for Wallet {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Wallet({})", self.uri)
    }
}

impl Default for WalletState {
    fn default() -> Self {
        WalletState::NoWallet(NoWallet {
            buf: String::new(),
            error_msg: None,
        })
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
        })
    }

    pub fn get_balance<'a>(&mut self, jobs: &'a mut Jobs) -> Option<&'a Result<u64, nwc::Error>> {
        let m_bal_job = jobs.get_or_insert_with(&JobId::NWCBalance(&self.uri), || {
            Job::GetNWCBalance(get_balance(self.wallet.clone()))
        });

        let Job::GetNWCBalance(promise) = m_bal_job else {
            tracing::error!("incorrect job type: {:?}", m_bal_job);
            return None;
        };

        promise.ready()
    }

    pub fn pay_invoice<'a>(
        &mut self,
        invoice: &str,
        jobs: &'a mut Jobs,
    ) -> Option<&'a Result<PayInvoiceResponse, nwc::Error>> {
        let m_invoice_job = jobs.get_or_insert_with(&JobId::NWCInvoice(invoice), || {
            Job::PayNWCInvoice(pay_invoice(
                self.wallet.clone(),
                PayInvoiceRequest::new(invoice.to_owned()),
            ))
        });

        let Job::PayNWCInvoice(promise) = m_invoice_job else {
            tracing::error!("incorrect job type: {:?}", m_invoice_job);
            return None;
        };

        promise.ready()
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

pub enum WalletAction {
    SaveURI,
}

impl WalletAction {
    pub fn process(&self, accounts: &mut Accounts) {
        match &self {
            WalletAction::SaveURI => {
                save_uri(accounts);
            }
        }
    }
}

fn save_uri(accounts: &mut Accounts) {
    accounts.update_current_account(|acc| {
        let WalletState::NoWallet(no_wallet) = &mut acc.wallet_state else {
            return;
        };

        let uri = &no_wallet.buf;

        let Ok(wallet) = Wallet::new(uri.to_owned()) else {
            no_wallet.error_msg = Some(WalletError::InvalidURI);
            return;
        };

        acc.wallet_state = WalletState::Wallet(wallet);
    });
}
#[cfg(test)]
mod tests {
    use crate::Wallet;
    const URI: &str = "nostr+walletconnect://b889ff5b1513b641e2a139f661a661364979c5beee91842f8f0ef42ab558e9d4?relay=wss%3A%2F%2Frelay.damus.io&secret=71a8c14c1407c113601079c4302dab36460f0ccd0ad506f1f2dc73b5100e4f3c&lud16=nostr%40nostr.com";

    #[test]
    fn test_uri() {
        assert!(Wallet::new(URI.to_owned()).is_ok())
    }
}
