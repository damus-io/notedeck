use egui::{
    Align, Button, Frame, InnerResponse, Layout, RichText, ScrollArea, Ui, UiBuilder, Vec2,
};
use enostr::ToBech32;
use nostrdb::{Ndb, Transaction};
use nostrdb_net::{Keypair, Pubkey};
use notedeck::{tr, Accounts, DragResponse, Images, Localization, MediaJobSender};
use notedeck_ui::colors::PINK;
use notedeck_ui::profile::preview::SimpleProfilePreview;
use tracing::error;

use notedeck_ui::app_images;

pub struct AccountsView<'a> {
    ndb: &'a Ndb,
    accounts: &'a Accounts,
    img_cache: &'a mut Images,
    jobs: &'a MediaJobSender,
    i18n: &'a mut Localization,
}

#[derive(Clone, Debug)]
pub enum AccountsViewResponse {
    SelectAccount(Pubkey),
    RemoveAccount(Pubkey),
    RouteToLogin,
}

#[derive(Debug)]
enum ProfilePreviewAction {
    RemoveAccount,
    SwitchTo,
}

impl<'a> AccountsView<'a> {
    pub fn new(
        ndb: &'a Ndb,
        accounts: &'a Accounts,
        jobs: &'a MediaJobSender,
        img_cache: &'a mut Images,
        i18n: &'a mut Localization,
    ) -> Self {
        AccountsView {
            ndb,
            accounts,
            img_cache,
            i18n,
            jobs,
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> DragResponse<AccountsViewResponse> {
        let mut out = DragResponse::none();
        Frame::new().outer_margin(12.0).show(ui, |ui| {
            if let Some(resp) = Self::top_section_buttons_widget(ui, self.i18n).inner {
                out.set_output(resp);
            }

            ui.add_space(8.0);
            let scroll_out = scroll_area()
                .id_salt(AccountsView::scroll_id())
                .show(ui, |ui| {
                    Self::show_accounts(
                        ui,
                        self.accounts,
                        self.ndb,
                        self.img_cache,
                        self.jobs,
                        self.i18n,
                    )
                });

            out.set_scroll_id(&scroll_out);
            if let Some(scroll_output) = scroll_out.inner {
                out.set_output(scroll_output);
            }
        });
        out
    }

    pub fn scroll_id() -> egui::Id {
        egui::Id::new("accounts")
    }

    fn show_accounts(
        ui: &mut Ui,
        accounts: &Accounts,
        ndb: &Ndb,
        img_cache: &mut Images,
        jobs: &MediaJobSender,
        i18n: &mut Localization,
    ) -> Option<AccountsViewResponse> {
        let mut return_op: Option<AccountsViewResponse> = None;
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_size_before_wrap().x, 32.0),
            Layout::top_down(egui::Align::Min),
            |ui| {
                let txn = if let Ok(txn) = Transaction::new(ndb) {
                    txn
                } else {
                    return;
                };

                let selected = accounts.cache.selected();
                for (pk, account) in &accounts.cache {
                    let profile = ndb.get_profile_by_pubkey(&txn, pk).ok();
                    let is_selected = *pk == selected.key.pubkey;
                    let has_nsec = account.key.secret_key.is_some();

                    let profile_peview_view = {
                        let max_size = egui::vec2(ui.available_width(), 77.0);
                        let resp = ui.allocate_response(max_size, egui::Sense::click());
                        account_context_menu(&resp, &account.key, i18n);
                        ui.allocate_new_ui(UiBuilder::new().max_rect(resp.rect), |ui| {
                            let preview = SimpleProfilePreview::new(
                                profile.as_ref(),
                                img_cache,
                                jobs,
                                i18n,
                                has_nsec,
                            );
                            show_profile_card(ui, preview, max_size, is_selected, resp)
                        })
                        .inner
                    };

                    if let Some(op) = profile_peview_view {
                        return_op = Some(match op {
                            ProfilePreviewAction::SwitchTo => {
                                AccountsViewResponse::SelectAccount(*pk)
                            }
                            ProfilePreviewAction::RemoveAccount => {
                                AccountsViewResponse::RemoveAccount(*pk)
                            }
                        });
                    }
                }
            },
        );
        return_op
    }

    fn top_section_buttons_widget(
        ui: &mut egui::Ui,
        i18n: &mut Localization,
    ) -> InnerResponse<Option<AccountsViewResponse>> {
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_size_before_wrap().x, 32.0),
            Layout::left_to_right(egui::Align::Center),
            |ui| {
                if ui.add(add_account_button(i18n)).clicked() {
                    Some(AccountsViewResponse::RouteToLogin)
                } else {
                    None
                }
            },
        )
    }
}

/// The `nsec1…` form of an account's secret key, or `None` for a pubkey-only
/// (read-only) account.
fn account_nsec(key: &Keypair) -> Option<String> {
    key.secret_key.as_ref()?.to_bech32().ok()
}

/// Right-click menu on an account card: copy the account's `npub`, and — for
/// accounts we hold the secret key for — its `nsec`, which otherwise has no way
/// back out of the app once it's been added.
fn account_context_menu(card_resp: &egui::Response, key: &Keypair, i18n: &mut Localization) {
    notedeck_ui::context_menu::context_menu(card_resp, |ui| {
        let copy_npub = ui.button(tr!(
            i18n,
            "Copy npub",
            "Context menu item to copy an account's public key"
        ));

        if copy_npub.clicked() {
            match key.pubkey.npub() {
                Some(npub) => ui.ctx().copy_text(npub),
                None => error!("could not encode pubkey as npub"),
            }
            ui.close_menu();
        }

        // Read-only accounts have no secret key to hand back.
        if key.secret_key.is_none() {
            return;
        }

        let copy_nsec = ui.button(
            RichText::new(tr!(
                i18n,
                "Copy nsec",
                "Context menu item to copy an account's secret key"
            ))
            .color(ui.visuals().warn_fg_color),
        );

        if copy_nsec.clicked() {
            match account_nsec(key) {
                Some(nsec) => ui.ctx().copy_text(nsec),
                None => error!("could not encode secret key as nsec"),
            }
            ui.close_menu();
        }
    });
}

fn show_profile_card(
    ui: &mut egui::Ui,
    preview: SimpleProfilePreview,
    max_size: egui::Vec2,
    is_selected: bool,
    card_resp: egui::Response,
) -> Option<ProfilePreviewAction> {
    let mut op: Option<ProfilePreviewAction> = None;

    ui.add_sized(max_size, |ui: &mut egui::Ui| {
        let mut frame = Frame::new();
        if is_selected || card_resp.hovered() {
            frame = frame.fill(ui.visuals().noninteractive().weak_bg_fill);
        }
        if is_selected {
            frame = frame.stroke(ui.visuals().noninteractive().fg_stroke);
        }
        frame
            .corner_radius(8.0)
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let btn = sign_out_button(preview.i18n);
                    ui.add(preview);

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if card_resp.clicked() {
                            op = Some(ProfilePreviewAction::SwitchTo);
                        }
                        if ui.add_sized(egui::Vec2::new(84.0, 32.0), btn).clicked() {
                            op = Some(ProfilePreviewAction::RemoveAccount)
                        }
                    });
                });
            })
            .response
    });
    ui.add_space(8.0);
    op
}

fn scroll_area() -> ScrollArea {
    egui::ScrollArea::vertical()
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .auto_shrink([false; 2])
}

fn add_account_button(i18n: &mut Localization) -> Button<'static> {
    Button::image_and_text(
        app_images::add_account_image().fit_to_exact_size(Vec2::new(48.0, 48.0)),
        RichText::new(tr!(
            i18n,
            "Add account",
            "Button label to add a new account"
        ))
        .size(16.0)
        // TODO: this color should not be hard coded. Find some way to add it to the visuals
        .color(PINK),
    )
    .frame(false)
}

fn sign_out_button(i18n: &mut Localization) -> egui::Button<'static> {
    egui::Button::new(RichText::new(tr!(
        i18n,
        "Sign out",
        "Button label to sign out of account"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::nips::nip19::FromBech32;
    use nostrdb_net::{FullKeypair, SecretKey};

    /// The nsec menu item hands back a bech32 secret key that round-trips to the
    /// same account, and is only offered for accounts we hold the key for.
    #[test]
    fn nsec_is_bech32_and_signer_only() {
        let full = FullKeypair::generate();
        let key = full.to_keypair();

        let nsec = account_nsec(&key).expect("nsec");
        assert!(nsec.starts_with("nsec1"), "got {nsec}");
        assert_eq!(
            Keypair::from_secret(SecretKey::from_bech32(&nsec).expect("parse")).pubkey,
            key.pubkey
        );

        // A pubkey-only (read-only) account has nothing secret to copy.
        assert_eq!(account_nsec(&Keypair::only_pubkey(key.pubkey)), None);
    }

    /// Right-clicking an account card opens the menu and copies the account's
    /// keys — the whole point of the feature, and the part a unit test on
    /// [`account_nsec`] can't see (the menu hangs off a bare
    /// `allocate_response`, which is easy to wire up so it never opens at all).
    #[test]
    fn right_click_card_copies_keys() {
        use egui_kittest::{kittest::Queryable, Harness};
        use std::cell::RefCell;
        use std::rc::Rc;

        struct Shared {
            i18n: Localization,
            /// The card's rect, so the test can aim a real pointer at it.
            card: egui::Rect,
            /// Text handed to the clipboard so far this run.
            copied: Vec<String>,
        }

        let key = FullKeypair::generate().to_keypair();
        let shared = Rc::new(RefCell::new(Shared {
            i18n: Localization::default(),
            card: egui::Rect::NOTHING,
            copied: Vec::new(),
        }));

        let render_shared = shared.clone();
        let menu_key = key.clone();
        let mut harness = Harness::new_ui(move |ui| {
            let mut sh = render_shared.borrow_mut();
            let resp = ui.allocate_response(egui::vec2(200.0, 77.0), egui::Sense::click());
            sh.card = resp.rect;
            account_context_menu(&resp, &menu_key, &mut sh.i18n);

            // Menu clicks land inside the call above, so drain the copy commands
            // here while they're still on this frame's output.
            ui.ctx().output(|out| {
                sh.copied
                    .extend(out.commands.iter().filter_map(|cmd| match cmd {
                        egui::OutputCommand::CopyText(text) => Some(text.clone()),
                        _ => None,
                    }));
            });
        });

        harness.run_ok();
        assert!(
            harness.query_by_label("Copy nsec").is_none(),
            "the menu should stay closed until it's asked for"
        );

        // Right-click the card, as a user reaching for their nsec would, then
        // pick `item` out of the menu that opens.
        let center = shared.borrow().card.center();
        let copy_via_menu = |harness: &mut Harness<'_>, item: &str| {
            harness
                .input_mut()
                .events
                .push(egui::Event::PointerMoved(center));
            for pressed in [true, false] {
                harness.input_mut().events.push(egui::Event::PointerButton {
                    pos: center,
                    button: egui::PointerButton::Secondary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                });
            }
            harness.run_ok();
            harness.get_by_label(item).click();
            harness.run_ok();
        };

        copy_via_menu(&mut harness, "Copy npub");
        copy_via_menu(&mut harness, "Copy nsec");

        assert_eq!(
            shared.borrow().copied,
            vec![
                key.pubkey.npub().expect("npub"),
                account_nsec(&key).expect("nsec"),
            ],
            "each menu item should put its own key on the clipboard"
        );
    }
}
