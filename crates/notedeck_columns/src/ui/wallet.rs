use egui::{vec2, CornerRadius, Layout};
use egui_winit::clipboard::Clipboard;
use notedeck::{
    get_current_wallet_mut, tr, Accounts, DefaultZapMsats, GlobalWallet, Localization,
    NotedeckTextStyle, PendingDefaultZapState, Wallet, WalletError, WalletUIState, ZapWallet,
};

use crate::{nav::RouterAction, route::Route};

use super::widgets::styled_button;

#[derive(Debug)]
pub enum WalletState<'a> {
    Wallet {
        wallet: &'a mut Wallet,
        default_zap_state: DefaultZapState<'a>,
        can_create_local_wallet: bool,
    },
    NoWallet {
        state: &'a mut WalletUIState,
        show_local_only: bool,
    },
}

type Msats = u64;

#[derive(Debug)]
pub enum DefaultZapState<'a> {
    Pending(&'a mut PendingDefaultZapState), // User input
    Valid(&'a Msats),                        // in milisats
}

pub fn get_default_zap_state(default_zap: &mut DefaultZapMsats) -> DefaultZapState<'_> {
    if default_zap.pending.is_rewriting {
        return DefaultZapState::Pending(&mut default_zap.pending);
    }

    if let Some(user_selection) = &default_zap.msats {
        DefaultZapState::Valid(user_selection)
    } else {
        DefaultZapState::Pending(&mut default_zap.pending)
    }
}

#[derive(Debug)]
pub enum WalletAction {
    SaveURI,
    AddLocalOnly,
    Delete,
    SetDefaultZapSats(String), // in sats
    EditDefaultZaps,
}

impl WalletAction {
    pub fn process(
        &self,
        accounts: &mut Accounts,
        global_wallet: &mut GlobalWallet,
    ) -> Option<RouterAction> {
        let mut action = None;

        match &self {
            WalletAction::SaveURI => {
                let ui_state = &mut global_wallet.ui_state;
                if ui_state.for_local_only {
                    ui_state.for_local_only = false;

                    if accounts.get_selected_wallet_mut().is_some() {
                        return None;
                    }

                    let wallet = try_create_wallet(ui_state)?;

                    accounts.update_current_account(move |acc| {
                        acc.wallet = Some(wallet.into());
                    });
                } else {
                    if global_wallet.wallet.is_some() {
                        return None;
                    }

                    let wallet = try_create_wallet(ui_state)?;

                    global_wallet.wallet = Some(wallet.into());
                    global_wallet.save_wallet();
                }
            }
            WalletAction::AddLocalOnly => {
                action = Some(RouterAction::route_to(Route::Wallet(
                    notedeck::WalletType::Local,
                )));
                global_wallet.ui_state.for_local_only = true;
            }
            WalletAction::Delete => {
                if accounts.get_selected_account().wallet.is_some() {
                    accounts.update_current_account(|acc| {
                        acc.wallet = None;
                    });
                    return None;
                }

                global_wallet.wallet = None;
                global_wallet.save_wallet();
            }
            WalletAction::SetDefaultZapSats(new_default) => 's: {
                let sats = {
                    let Some(wallet) = get_current_wallet_mut(accounts, global_wallet) else {
                        break 's;
                    };

                    let Ok(sats) = new_default.parse::<u64>() else {
                        wallet.default_zap.pending.error_message =
                            Some(notedeck::DefaultZapError::InvalidUserInput);
                        break 's;
                    };
                    sats
                };

                let update_wallet = |wallet: &mut ZapWallet| {
                    wallet.default_zap.set_user_selection(sats * 1000);
                    wallet.default_zap.pending = PendingDefaultZapState::default();
                };

                if accounts.selected_account_has_wallet()
                    && accounts.update_current_account(|acc| {
                        if let Some(wallet) = &mut acc.wallet {
                            update_wallet(wallet);
                        }
                    })
                {
                    break 's;
                }

                let Some(wallet) = &mut global_wallet.wallet else {
                    break 's;
                };

                update_wallet(wallet);
                global_wallet.save_wallet();
            }
            WalletAction::EditDefaultZaps => 's: {
                let Some(wallet) = get_current_wallet_mut(accounts, global_wallet) else {
                    break 's;
                };

                wallet.default_zap.pending.is_rewriting = true;
                wallet.default_zap.pending.amount_sats =
                    (wallet.default_zap.get_default_zap_msats() / 1000).to_string();
            }
        }
        action
    }
}

pub struct WalletView<'a> {
    state: WalletState<'a>,
    i18n: &'a mut Localization,
    clipboard: &'a mut Clipboard,
}

impl<'a> WalletView<'a> {
    pub fn new(
        state: WalletState<'a>,
        i18n: &'a mut Localization,
        clipboard: &'a mut Clipboard,
    ) -> Self {
        Self {
            state,
            i18n,
            clipboard,
        }
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
                default_zap_state,
                can_create_local_wallet,
            } => show_with_wallet(
                ui,
                self.i18n,
                wallet,
                default_zap_state,
                *can_create_local_wallet,
            ),
            WalletState::NoWallet {
                state,
                show_local_only,
            } => show_no_wallet(ui, self.i18n, state, *show_local_only, self.clipboard),
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
    i18n: &mut Localization,
    state: &mut WalletUIState,
    show_local_only: bool,
    clipboard: &mut Clipboard,
) -> Option<WalletAction> {
    ui.horizontal_wrapped(|ui| {
        use notedeck_ui::context_menu::{input_context, PasteBehavior};

        let text_edit = egui::TextEdit::singleline(&mut state.buf)
            .hint_text(
                egui::RichText::new(tr!(
                    i18n,
                    "Paste your NWC URI here...",
                    "Placeholder text for NWC URI input"
                ))
                .text_style(notedeck::NotedeckTextStyle::Body.text_style()),
            )
            .vertical_align(egui::Align::Center)
            .desired_width(f32::INFINITY)
            .min_size(egui::Vec2::new(0.0, 40.0))
            .margin(egui::Margin::same(12))
            .password(true);

        // add paste context menu
        input_context(
            &ui.add(text_edit),
            clipboard,
            &mut state.buf,
            PasteBehavior::Clear,
        );

        let Some(error_msg) = &state.error_msg else {
            return;
        };

        let error_str = match error_msg {
            WalletError::InvalidURI => tr!(
                i18n,
                "Invalid NWC URI",
                "Error message for invalid Nostr Wallet Connect URI"
            ),
            WalletError::NoWallet => tr!(
                i18n,
                "Add a wallet to continue",
                "Error message for missing wallet"
            ),
        };
        ui.colored_label(ui.visuals().warn_fg_color, error_str);
    });

    ui.add_space(8.0);

    if show_local_only {
        ui.checkbox(
            &mut state.for_local_only,
            tr!(
                i18n,
                "Use this wallet for the current account only",
                "Checkbox label for using wallet only for current account"
            ),
        );
        ui.add_space(8.0);
    }

    ui.with_layout(Layout::top_down(egui::Align::Center), |ui| {
        ui.add(styled_button(
            tr!(i18n, "Add Wallet", "Button label to add a wallet").as_str(),
            notedeck_ui::colors::PINK,
        ))
        .clicked()
        .then_some(WalletAction::SaveURI)
    })
    .inner
}

fn show_with_wallet(
    ui: &mut egui::Ui,
    i18n: &mut Localization,
    wallet: &mut Wallet,
    default_zap_state: &mut DefaultZapState,
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

    let mut action = show_default_zap(ui, i18n, default_zap_state);

    ui.with_layout(Layout::bottom_up(egui::Align::Min), |ui| 's: {
        if ui
            .add(styled_button(
                tr!(i18n, "Delete Wallet", "Button label to delete a wallet").as_str(),
                ui.visuals().window_fill,
            ))
            .clicked()
        {
            action = Some(WalletAction::Delete);
            break 's;
        }

        ui.add_space(12.0);
        if can_create_local_wallet
            && ui
                .checkbox(
                    &mut false,
                    tr!(
                        i18n,
                        "Add a different wallet that will only be used for this account",
                        "Button label to add a different wallet"
                    ),
                )
                .clicked()
        {
            action = Some(WalletAction::AddLocalOnly);
        }
    });

    action
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

fn show_default_zap(
    ui: &mut egui::Ui,
    i18n: &mut Localization,
    state: &mut DefaultZapState,
) -> Option<WalletAction> {
    let mut action = None;
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 50.0),
        egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
        |ui| {
            ui.label(tr!(i18n, "Default amount per zap: ", "Label for default zap amount input"));
            match state {
                DefaultZapState::Pending(pending_default_zap_state) => {
                    let text = &mut pending_default_zap_state.amount_sats;

                    let font = NotedeckTextStyle::Body.get_font_id(ui.ctx());
                    let desired_width = {
                        let painter = ui.painter();
                        let galley = painter.layout_no_wrap(
                            text.clone(),
                            font.clone(),
                            ui.visuals().text_color(),
                        );
                        let rect_width = galley.rect.width();
                        if rect_width < 5.0 {
                            10.0
                        } else {
                            rect_width
                        }
                    };

                    let id = ui.id().with("default_zap_amount");
                    ui.add(
                        egui::TextEdit::singleline(text)
                            .desired_width(desired_width)
                            .margin(egui::Margin::same(8))
                            .font(font)
                            .id(id),
                    );

                    ui.memory_mut(|m| m.request_focus(id));

                    ui.label(tr!(i18n, "sats", "Unit label for satoshis (Bitcoin unit) for configuring default zap amount in wallet settings."));

                    if ui
                        .add(styled_button(tr!(i18n, "Save", "Button to save default zap amount").as_str(), ui.visuals().widgets.active.bg_fill))
                        .clicked()
                    {
                        action = Some(WalletAction::SetDefaultZapSats(text.to_string()));
                    }
                }
                DefaultZapState::Valid(msats) => {
                    if let Some(wallet_action) = show_valid_msats(ui, i18n, **msats) {
                        action = Some(wallet_action);
                    }
                    ui.label(tr!(i18n, "sats", "Unit label for satoshis (Bitcoin unit) for configuring default zap amount in wallet settings."));
                }
            }

            if let DefaultZapState::Pending(pending) = state {
                if let Some(error_message) = &pending.error_message {
                    let msg_str = match error_message {
                        notedeck::DefaultZapError::InvalidUserInput => tr!(i18n, "Invalid amount", "Error message for invalid zap amount"),
                    };

                    ui.colored_label(ui.visuals().warn_fg_color, msg_str);
                }
            }
        },
    );

    action
}

fn show_valid_msats(
    ui: &mut egui::Ui,
    i18n: &mut Localization,
    msats: u64,
) -> Option<WalletAction> {
    let galley = {
        let painter = ui.painter();

        let sats_str = (msats / 1000).to_string();
        painter.layout_no_wrap(
            sats_str,
            NotedeckTextStyle::Body.get_font_id(ui.ctx()),
            ui.visuals().text_color(),
        )
    };

    let (rect, resp) = ui.allocate_exact_size(galley.rect.expand(8.0).size(), egui::Sense::click());

    let resp = resp
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text_at_pointer(tr!(
            i18n,
            "Click to edit",
            "Hover text for editable zap amount"
        ));

    let painter = ui.painter_at(resp.rect);

    painter.rect_filled(
        rect,
        CornerRadius::same(8),
        ui.visuals().noninteractive().bg_fill,
    );

    let galley_pos = {
        let mut next_pos = rect.left_top();
        next_pos.x += 8.0;
        next_pos.y += 8.0;
        next_pos
    };

    painter.galley(galley_pos, galley, notedeck_ui::colors::MID_GRAY);

    if resp.clicked() {
        Some(WalletAction::EditDefaultZaps)
    } else {
        None
    }
}
