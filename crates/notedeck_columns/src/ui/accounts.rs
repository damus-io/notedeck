use egui::{
    Align, Button, Frame, InnerResponse, Layout, RichText, ScrollArea, Ui, UiBuilder, Vec2,
};
use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};
use notedeck::{tr, Accounts, DragResponse, Images, Localization, MediaJobSender};
use notedeck_ui::colors::PINK;
use notedeck_ui::profile::preview::SimpleProfilePreview;

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
