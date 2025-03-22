use std::sync::Arc;

use nwc::{
    nostr::nips::nip47::{NostrWalletConnectURI, PayInvoiceRequest, PayInvoiceResponse},
    NWC,
};
use tokio::sync::RwLock;

use crate::jobs::{
    Job, JobError, JobId, JobParams, JobParamsOwned, JobState, Jobs, NWCInvoiceParams,
};

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
        let wallet = self.wallet.clone();
        let job_state =
            jobs.get_or_insert_with(&JobId::NWCBalance(&self.uri), None, move |_| async move {
                let balance = wallet.read().await.get_balance().await;
                Ok(Job::GetNWCBalance(balance))
            });

        let JobState::Completed(m_bal_job) = job_state else {
            return None;
        };

        let Job::GetNWCBalance(bal) = m_bal_job else {
            tracing::error!("incorrect job type: {:?}", m_bal_job);
            return None;
        };

        Some(bal)
    }

    pub fn pay_invoice<'a>(
        &mut self,
        invoice: &str,
        jobs: &'a mut Jobs,
    ) -> Option<&'a Result<PayInvoiceResponse, nwc::Error>> {
        let nwc = self.wallet.clone();
        let params = NWCInvoiceParams { invoice };

        let job_state = jobs.get_or_insert_with(
            &JobId::NWCInvoice(invoice),
            Some(JobParams::NWCInvoice(params)),
            move |params| async move {
                let Some(JobParamsOwned::NWCInvoice(params)) = params else {
                    return Err(JobError::InvalidParameters);
                };

                let invoice = nwc
                    .read()
                    .await
                    .pay_invoice(PayInvoiceRequest::new(params.invoice))
                    .await;

                Ok(Job::PayNWCInvoice(invoice))
            },
        );

        let JobState::Completed(m_invoice_job) = job_state else {
            return None;
        };

        let Job::PayNWCInvoice(invoice) = m_invoice_job else {
            tracing::error!("incorrect job type: {:?}", m_invoice_job);
            return None;
        };

        Some(invoice)
    }
}

pub enum WalletAction {
    SaveURI,
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
