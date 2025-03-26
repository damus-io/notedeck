use egui::Layout;
use notedeck::{Accounts, GlobalWallet, Wallet, WalletError, WalletState, WalletUIState};

use crate::route::{Route, Router};

use super::widgets::styled_button;

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
        router: &mut Router<Route>,
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
                router.route_to(Route::Wallet(notedeck::WalletType::Local));
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

pub struct WalletView<'a> {
    state: WalletState<'a>,
}

impl<'a> WalletView<'a> {
    pub fn new(state: WalletState<'a>) -> Self {
        Self { state }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<WalletAction> {
        egui::Frame::NONE
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| self.inner_ui(ui))
            .inner
    }

    fn inner_ui(&mut self, ui: &mut egui::Ui) -> Option<WalletAction> {
        match &mut self.state {
            WalletState::Wallet {
                wallet,
                can_create_local_wallet,
            } => show_with_wallet(ui, wallet, *can_create_local_wallet),
            WalletState::NoWallet {
                state,
                show_local_only,
            } => show_no_wallet(ui, state, *show_local_only),
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

fn show_no_wallet(
    ui: &mut egui::Ui,
    state: &mut WalletUIState,
    show_local_only: bool,
) -> Option<WalletAction> {
    ui.horizontal_wrapped(|ui| 's: {
        let text_edit = egui::TextEdit::singleline(&mut state.buf)
            .hint_text(
                egui::RichText::new("Paste your NWC URI here...")
                    .text_style(notedeck::NotedeckTextStyle::Body.text_style()),
            )
            .vertical_align(egui::Align::Center)
            .desired_width(f32::INFINITY)
            .min_size(egui::Vec2::new(0.0, 40.0))
            .margin(egui::Margin::same(12))
            .password(true);

        ui.add(text_edit);

        let Some(error_msg) = &state.error_msg else {
            break 's;
        };

        match error_msg {
            WalletError::InvalidURI => {
                ui.colored_label(ui.visuals().warn_fg_color, "Invalid NWC URI")
            }
        };
    });

    ui.add_space(8.0);

    if show_local_only {
        ui.checkbox(
            &mut state.for_local_only,
            "Use this wallet for the current account only",
        );
        ui.add_space(8.0);
    }

    ui.with_layout(Layout::top_down(egui::Align::Center), |ui| {
        ui.add(styled_button("Add Wallet", notedeck_ui::colors::PINK))
            .clicked()
            .then_some(WalletAction::SaveURI)
    })
    .inner
}

fn show_with_wallet(
    ui: &mut egui::Ui,
    wallet: &mut Wallet,
    can_create_local_wallet: bool,
) -> Option<WalletAction> {
    ui.horizontal_wrapped(|ui| {
        let balance = wallet.get_balance();

        if let Some(balance) = balance {
            match balance {
                Ok(msats) => show_balance(ui, *msats),
                Err(e) => ui.colored_label(egui::Color32::RED, format!("error: {e}")),
            }
        } else {
            ui.with_layout(Layout::top_down(egui::Align::Center), |ui| {
                ui.add(egui::Spinner::new().size(48.0))
            })
            .inner
        }
    });

    ui.with_layout(Layout::bottom_up(egui::Align::Min), |ui| 's: {
        if ui
            .add(styled_button("Delete Wallet", ui.visuals().window_fill))
            .clicked()
        {
            break 's Some(WalletAction::Delete);
        }

        ui.add_space(12.0);
        if can_create_local_wallet
            && ui
                .checkbox(
                    &mut false,
                    "Add a different wallet that will only be used for this account",
                )
                .clicked()
        {
            break 's Some(WalletAction::AddLocalOnly);
        }

        None
    })
    .inner
}

fn show_balance(ui: &mut egui::Ui, msats: u64) -> egui::Response {
    let sats = human_format::Formatter::new()
        .with_decimals(2)
        .format(msats as f64 / 1000.0);

    ui.with_layout(Layout::top_down(egui::Align::Center), |ui| {
        ui.label(egui::RichText::new(format!("{sats} sats")).size(48.0))
    })
    .inner
}
