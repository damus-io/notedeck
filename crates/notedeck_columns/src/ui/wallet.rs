use egui::Layout;
use notedeck::{NoWallet, Wallet, WalletAction, WalletError, WalletState};

use super::add_column::sized_button;

pub struct WalletView<'a> {
    state: &'a mut WalletState,
}

impl<'a> WalletView<'a> {
    pub fn new(state: &'a mut WalletState) -> Self {
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
            WalletState::Wallet(wallet) => show_with_wallet(ui, wallet),
            WalletState::NoWallet(no_wallet) => show_no_wallet(ui, no_wallet),
        }
    }
}

fn show_no_wallet(ui: &mut egui::Ui, no_wallet: &mut NoWallet) -> Option<WalletAction> {
    ui.horizontal_wrapped(|ui| 's: {
        let text_edit = egui::TextEdit::singleline(&mut no_wallet.buf)
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

        let Some(error_msg) = &no_wallet.error_msg else {
            break 's;
        };

        match error_msg {
            WalletError::InvalidURI => {
                ui.colored_label(ui.visuals().warn_fg_color, "Invalid NWC URI")
            }
        };
    });

    ui.add_space(8.0);

    ui.with_layout(Layout::top_down(egui::Align::Center), |ui| {
        ui.add(sized_button("Add Wallet"))
            .clicked()
            .then_some(WalletAction::SaveURI)
    })
    .inner
}

fn show_with_wallet(ui: &mut egui::Ui, wallet: &mut Wallet) -> Option<WalletAction> {
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

    None
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
