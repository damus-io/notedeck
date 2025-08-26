use std::mem;

use egui::{Layout, ScrollArea};
use nostrdb::Ndb;
use notedeck::{Images, JobPool, JobsCache, Localization};
use notedeck_ui::{
    colors,
    nip51_set::{Nip51SetUiCache, Nip51SetWidget, Nip51SetWidgetFlags, Nip51SetWidgetResponse},
};

use crate::{onboarding::Onboarding, ui::widgets::styled_button};

/// Display Follow Packs for the user to choose from authors trusted by the Damus team
pub struct FollowPackOnboardingView<'a> {
    onboarding: &'a mut Onboarding,
    ui_state: &'a mut Nip51SetUiCache,
    ndb: &'a Ndb,
    images: &'a mut Images,
    loc: &'a mut Localization,
    job_pool: &'a mut JobPool,
    jobs: &'a mut JobsCache,
}

pub enum OnboardingResponse {
    FollowPacks(FollowPacksResponse),
    ViewProfile(enostr::Pubkey),
}

pub enum FollowPacksResponse {
    NoFollowPacks,
    UserSelectedPacks(Nip51SetUiCache),
}

impl<'a> FollowPackOnboardingView<'a> {
    pub fn new(
        onboarding: &'a mut Onboarding,
        ui_state: &'a mut Nip51SetUiCache,
        ndb: &'a Ndb,
        images: &'a mut Images,
        loc: &'a mut Localization,
        job_pool: &'a mut JobPool,
        jobs: &'a mut JobsCache,
    ) -> Self {
        Self {
            onboarding,
            ui_state,
            ndb,
            images,
            loc,
            job_pool,
            jobs,
        }
    }

    pub fn scroll_id() -> egui::Id {
        egui::Id::new("follow_pack_onboarding")
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<OnboardingResponse> {
        let Some(follow_pack_state) = self.onboarding.get_follow_packs() else {
            return Some(OnboardingResponse::FollowPacks(
                FollowPacksResponse::NoFollowPacks,
            ));
        };

        let max_height = ui.available_height() - 48.0;

        let mut action = None;
        ScrollArea::vertical()
            .id_salt(Self::scroll_id())
            .max_height(max_height)
            .show(ui, |ui| {
                egui::Frame::new().inner_margin(8.0).show(ui, |ui| {
                    if let Some(resp) = Nip51SetWidget::new(
                        follow_pack_state,
                        self.ui_state,
                        self.ndb,
                        self.loc,
                        self.images,
                        self.job_pool,
                        self.jobs,
                    )
                    .with_flags(Nip51SetWidgetFlags::TRUST_IMAGES)
                    .ui(ui)
                    {
                        match resp {
                            Nip51SetWidgetResponse::ViewProfile(pubkey) => {
                                action = Some(OnboardingResponse::ViewProfile(pubkey));
                            }
                        }
                    }
                })
            });

        ui.with_layout(Layout::top_down(egui::Align::Center), |ui| {
            ui.add_space(4.0);
            if ui.add(styled_button("Done", colors::PINK)).clicked() {
                action = Some(OnboardingResponse::FollowPacks(
                    FollowPacksResponse::UserSelectedPacks(mem::take(self.ui_state)),
                ));
            }
        });

        action
    }
}
