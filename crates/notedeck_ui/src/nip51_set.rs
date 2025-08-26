use bitflags::bitflags;
use egui::{vec2, Checkbox, CornerRadius, Layout, Margin, RichText, Sense, UiBuilder};
use enostr::Pubkey;
use hashbrown::{hash_map::RawEntryMut, HashMap};
use nostrdb::{Ndb, ProfileRecord, Transaction};
use notedeck::{
    fonts::get_font_size, get_profile_url, name::get_display_name, tr, Images, JobPool, JobsCache,
    Localization, Nip51Set, Nip51SetCache, NotedeckTextStyle,
};

use crate::{
    note::media::{render_media, ScaledTextureFlags},
    ProfilePic,
};

pub struct Nip51SetWidget<'a> {
    state: &'a Nip51SetCache,
    ui_state: &'a mut Nip51SetUiCache,
    ndb: &'a Ndb,
    images: &'a mut Images,
    loc: &'a mut Localization,
    job_pool: &'a mut JobPool,
    jobs: &'a mut JobsCache,
    flags: Nip51SetWidgetFlags,
}

bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Nip51SetWidgetFlags: u8 {
        const REQUIRES_TITLE = 1u8;
        const REQUIRES_IMAGE = 2u8;
        const REQUIRES_DESCRIPTION = 3u8;
        const NON_EMPTY_PKS = 4u8;
        const TRUST_IMAGES = 5u8;
    }
}

impl Default for Nip51SetWidgetFlags {
    fn default() -> Self {
        Self::empty()
    }
}

pub enum Nip51SetWidgetAction {
    ViewProfile(Pubkey),
}

impl<'a> Nip51SetWidget<'a> {
    pub fn new(
        state: &'a Nip51SetCache,
        ui_state: &'a mut Nip51SetUiCache,
        ndb: &'a Ndb,
        loc: &'a mut Localization,
        images: &'a mut Images,
        job_pool: &'a mut JobPool,
        jobs: &'a mut JobsCache,
    ) -> Self {
        Self {
            state,
            ui_state,
            ndb,
            loc,
            images,
            job_pool,
            jobs,
            flags: Nip51SetWidgetFlags::default(),
        }
    }

    pub fn with_flags(mut self, flags: Nip51SetWidgetFlags) -> Self {
        self.flags = flags;
        self
    }

    fn render_set(&mut self, ui: &mut egui::Ui, set: &Nip51Set) -> Nip51SetWidgetResponse {
        if should_skip(set, &self.flags) {
            return Nip51SetWidgetResponse {
                action: None,
                rendered: false,
            };
        }

        let action = egui::Frame::new()
            .corner_radius(CornerRadius::same(8))
            //.fill(ui.visuals().extreme_bg_color)
            .inner_margin(Margin::same(8))
            .show(ui, |ui| {
                render_pack(
                    ui,
                    set,
                    self.ui_state,
                    self.ndb,
                    self.images,
                    self.job_pool,
                    self.jobs,
                    self.loc,
                    self.flags.contains(Nip51SetWidgetFlags::TRUST_IMAGES),
                )
            })
            .inner;

        Nip51SetWidgetResponse {
            action,
            rendered: true,
        }
    }

    pub fn render_at_index(&mut self, ui: &mut egui::Ui, index: usize) -> Nip51SetWidgetResponse {
        let Some(set) = self.state.at_index(index) else {
            return Nip51SetWidgetResponse {
                action: None,
                rendered: false,
            };
        };

        self.render_set(ui, set)
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<Nip51SetWidgetAction> {
        let mut resp = None;
        for pack in self.state.iter() {
            let res = self.render_set(ui, pack);

            if let Some(action) = res.action {
                resp = Some(action);
            }

            if !res.rendered {
                continue;
            }

            ui.add_space(8.0);
        }

        resp
    }
}

pub struct Nip51SetWidgetResponse {
    pub action: Option<Nip51SetWidgetAction>,
    pub rendered: bool,
}

fn should_skip(set: &Nip51Set, required: &Nip51SetWidgetFlags) -> bool {
    (required.contains(Nip51SetWidgetFlags::REQUIRES_TITLE) && set.title.is_none())
        || (required.contains(Nip51SetWidgetFlags::REQUIRES_IMAGE) && set.image.is_none())
        || (required.contains(Nip51SetWidgetFlags::REQUIRES_DESCRIPTION)
            && set.description.is_none())
        || (required.contains(Nip51SetWidgetFlags::NON_EMPTY_PKS) && set.pks.is_empty())
}

#[allow(clippy::too_many_arguments)]
fn render_pack(
    ui: &mut egui::Ui,
    pack: &Nip51Set,
    ui_state: &mut Nip51SetUiCache,
    ndb: &Ndb,
    images: &mut Images,
    job_pool: &mut JobPool,
    jobs: &mut JobsCache,
    loc: &mut Localization,
    image_trusted: bool,
) -> Option<Nip51SetWidgetAction> {
    let max_img_size = vec2(ui.available_width(), 200.0);

    ui.allocate_new_ui(UiBuilder::new(), |ui| 's: {
        let Some(url) = &pack.image else {
            break 's;
        };
        let Some(media) = images.get_renderable_media(url) else {
            break 's;
        };

        let media_rect = render_media(
            ui,
            images,
            job_pool,
            jobs,
            &media,
            image_trusted,
            loc,
            max_img_size,
            None,
            ScaledTextureFlags::RESPECT_MAX_DIMS,
        )
        .response
        .rect;

        ui.advance_cursor_after_rect(media_rect);
    });

    let (title_rect, _) =
        ui.allocate_at_least(vec2(ui.available_width(), 0.0), egui::Sense::hover());

    let select_all_resp = ui
        .allocate_new_ui(
            UiBuilder::new()
                .max_rect(title_rect)
                .layout(Layout::top_down(egui::Align::Min)),
            |ui| {
                if let Some(title) = &pack.title {
                    ui.add(egui::Label::new(egui::RichText::new(title).size(
                        get_font_size(ui.ctx(), &notedeck::NotedeckTextStyle::Heading),
                    )));
                }
                if let Some(desc) = &pack.description {
                    ui.add(egui::Label::new(
                        egui::RichText::new(desc)
                            .size(get_font_size(
                                ui.ctx(),
                                &notedeck::NotedeckTextStyle::Heading3,
                            ))
                            .color(ui.visuals().weak_text_color()),
                    ));
                }
                let checked = ui.checkbox(
                    ui_state.get_select_all_state(&pack.identifier),
                    format!(
                        "{} ({})",
                        tr!(
                            loc,
                            "Select All",
                            "Button to select all profiles in follow pack"
                        ),
                        pack.pks.len()
                    ),
                );

                checked
            },
        )
        .inner;

    let new_select_all_state = if select_all_resp.clicked() {
        Some(*ui_state.get_select_all_state(&pack.identifier))
    } else {
        None
    };

    let mut resp = None;
    let txn = Transaction::new(ndb).expect("txn");

    for pk in &pack.pks {
        let m_profile = ndb.get_profile_by_pubkey(&txn, pk.bytes()).ok();

        let cur_state = ui_state.get_pk_selected_state(&pack.identifier, pk);
        if let Some(use_state) = new_select_all_state {
            *cur_state = use_state;
        };

        ui.separator();
        if render_profile_item(ui, images, m_profile.as_ref(), cur_state) {
            resp = Some(Nip51SetWidgetAction::ViewProfile(*pk));
        }
    }

    resp
}

const PFP_SIZE: f32 = 32.0;

fn render_profile_item(
    ui: &mut egui::Ui,
    images: &mut Images,
    profile: Option<&ProfileRecord>,
    checked: &mut bool,
) -> bool {
    let (card_rect, card_resp) =
        ui.allocate_exact_size(vec2(ui.available_width(), PFP_SIZE), egui::Sense::click());

    let mut clicked_response = card_resp;

    let checkbox_size = {
        let mut size = egui::Vec2::splat(ui.spacing().interact_size.y);
        size.y = size.y.max(ui.spacing().icon_width);
        size
    };

    let (checkbox_section_rect, remaining_rect) =
        card_rect.split_left_right_at_x(card_rect.left() + checkbox_size.x + 8.0);

    let checkbox_rect = egui::Rect::from_center_size(checkbox_section_rect.center(), checkbox_size);

    let resp = ui.allocate_new_ui(UiBuilder::new().max_rect(checkbox_rect), |ui| {
        ui.add(Checkbox::without_text(checked));
    });
    ui.advance_cursor_after_rect(checkbox_rect);

    clicked_response = clicked_response.union(resp.response);

    let (pfp_rect, body_rect) =
        remaining_rect.split_left_right_at_x(remaining_rect.left() + PFP_SIZE);

    let _ = ui.allocate_new_ui(UiBuilder::new().max_rect(pfp_rect), |ui| {
        let pfp_resp = ui.add(
            &mut ProfilePic::new(images, get_profile_url(profile))
                .sense(Sense::click())
                .size(PFP_SIZE),
        );

        clicked_response = clicked_response.union(pfp_resp);
    });
    ui.advance_cursor_after_rect(pfp_rect);

    let (_, body_rect) = body_rect.split_left_right_at_x(body_rect.left() + 8.0);

    let (name_rect, description_rect) = body_rect.split_top_bottom_at_fraction(0.5);

    let resp = ui.allocate_new_ui(UiBuilder::new().max_rect(name_rect), |ui| {
        let name = get_display_name(profile);

        let painter = ui.painter_at(name_rect);

        let mut left_x_pos = name_rect.left();

        if let Some(disp) = name.display_name {
            let galley = painter.layout_no_wrap(
                disp.to_owned(),
                NotedeckTextStyle::Body.get_font_id(ui.ctx()),
                ui.visuals().text_color(),
            );

            left_x_pos += galley.rect.width() + 4.0;

            painter.galley(name_rect.min, galley, ui.visuals().text_color());
        }

        if let Some(username) = name.username {
            let galley = painter.layout_no_wrap(
                format!("@{username}"),
                NotedeckTextStyle::Body.get_font_id(ui.ctx()),
                crate::colors::MID_GRAY,
            );

            let pos = {
                let mut pos = name_rect.min;
                pos.x = left_x_pos;

                let padding = name_rect.height() - galley.rect.height();

                pos.y += padding / 2.0;

                pos
            };
            painter.galley(pos, galley, ui.visuals().text_color());
        }
    });
    ui.advance_cursor_after_rect(name_rect);
    clicked_response = clicked_response.union(resp.response);

    let resp = ui.allocate_new_ui(UiBuilder::new().max_rect(description_rect), |ui| 's: {
        let Some(record) = profile else {
            break 's;
        };

        let Some(ndb_profile) = record.record().profile() else {
            break 's;
        };

        let Some(about) = ndb_profile.about() else {
            break 's;
        };

        ui.add(
            egui::Label::new(
                RichText::new(about).size(get_font_size(ui.ctx(), &NotedeckTextStyle::Heading4)),
            )
            .selectable(false)
            .truncate(),
        );
    });

    ui.advance_cursor_after_rect(description_rect);

    clicked_response = clicked_response.union(resp.response);

    clicked_response.clicked()
}

#[derive(Default)]
pub struct Nip51SetUiCache {
    state: HashMap<String, Nip51SetUiState>,
}

#[derive(Default)]
struct Nip51SetUiState {
    select_all: bool,
    select_pk: HashMap<Pubkey, bool>,
}

impl Nip51SetUiCache {
    pub fn get_pk_selected_state(&mut self, identifier: &str, pk: &Pubkey) -> &mut bool {
        let pack_state = match self.state.raw_entry_mut().from_key(identifier) {
            RawEntryMut::Occupied(entry) => entry.into_mut(),
            RawEntryMut::Vacant(entry) => {
                let (_, pack_state) =
                    entry.insert(identifier.to_owned(), Nip51SetUiState::default());

                pack_state
            }
        };
        match pack_state.select_pk.raw_entry_mut().from_key(pk) {
            RawEntryMut::Occupied(entry) => entry.into_mut(),
            RawEntryMut::Vacant(entry) => {
                let (_, state) = entry.insert(*pk, false);
                state
            }
        }
    }

    pub fn get_select_all_state(&mut self, identifier: &str) -> &mut bool {
        match self.state.raw_entry_mut().from_key(identifier) {
            RawEntryMut::Occupied(entry) => &mut entry.into_mut().select_all,
            RawEntryMut::Vacant(entry) => {
                let (_, pack_state) =
                    entry.insert(identifier.to_owned(), Nip51SetUiState::default());

                &mut pack_state.select_all
            }
        }
    }

    pub fn get_all_selected(&self) -> Vec<Pubkey> {
        let mut pks = Vec::new();

        for pack in self.state.values() {
            for (pk, select_state) in &pack.select_pk {
                if !*select_state {
                    continue;
                }

                pks.push(*pk);
            }
        }

        pks
    }
}
