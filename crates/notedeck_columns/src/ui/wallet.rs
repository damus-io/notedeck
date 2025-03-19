use notedeck::{Accounts, GlobalWallet, Wallet, WalletError, WalletUIState};

use crate::route::{Route, Router};

#[derive(Debug)]
pub enum WalletAction {
    SaveURI,
    AddLocalOnly,
    Delete,
}

impl WalletAction {
    pub fn process(
        &self,
        accounts: &mut Accounts,
        global_wallet: &mut GlobalWallet,
        _router: &mut Router<Route>,
    ) {
        match &self {
            WalletAction::SaveURI => {
                let ui_state = &mut global_wallet.ui_state;
                if ui_state.for_local_only {
                    ui_state.for_local_only = false;
                    let Some(cur_acc) = accounts.get_selected_account_mut() else {
                        return;
                    };

                    if cur_acc.wallet.is_some() {
                        return;
                    }

                    let Some(wallet) = try_create_wallet(ui_state) else {
                        return;
                    };

                    accounts.update_current_account(move |acc| {
                        acc.wallet = Some(wallet);
                    });
                } else {
                    if global_wallet.wallet.is_some() {
                        return;
                    }

                    let Some(wallet) = try_create_wallet(ui_state) else {
                        return;
                    };

                    global_wallet.wallet = Some(wallet);
                    global_wallet.save_wallet();
                }
            }
            WalletAction::AddLocalOnly => {
                // router.route_to(Route::Wallet(notedeck::WalletType::Local));
                global_wallet.ui_state.for_local_only = true;
            }
            WalletAction::Delete => {
                if let Some(acc) = accounts.get_selected_account() {
                    if acc.wallet.is_some() {
                        accounts.update_current_account(|acc| {
                            acc.wallet = None;
                        });
                        return;
                    }
                }

                global_wallet.wallet = None;
                global_wallet.save_wallet();
            }
        }
    }
}

fn try_create_wallet(state: &mut WalletUIState) -> Option<Wallet> {
    let uri = &state.buf;

    let Ok(wallet) = Wallet::new(uri.to_owned()) else {
        state.error_msg = Some(WalletError::InvalidURI);
        return None;
    };

    *state = WalletUIState::default();
    Some(wallet)
}
