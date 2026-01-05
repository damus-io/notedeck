use bitflags::bitflags;
use egui::{vec2, Checkbox, CornerRadius, Margin, RichText, Sense, UiBuilder};
use enostr::Pubkey;
use hashbrown::{hash_map::RawEntryMut, HashMap};
use nostrdb::{Ndb, ProfileRecord, Transaction};
use notedeck::{
    fonts::get_font_size, get_profile_url, name::get_display_name, tr, Images, Localization,
    MediaJobSender, Nip51Set, Nip51SetCache, NotedeckTextStyle,
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
    jobs: &'a MediaJobSender,
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
        jobs: &'a MediaJobSender,
    ) -> Self {
        Self {
            state,
            ui_state,
            ndb,
            loc,
            images,
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
                visibility_changed: false,
            };
        }

        let pack_resp = egui::Frame::new()
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
                    self.jobs,
                    self.loc,
                    self.flags.contains(Nip51SetWidgetFlags::TRUST_IMAGES),
                )
            })
            .inner;

        Nip51SetWidgetResponse {
            action: pack_resp.action,
            rendered: true,
            visibility_changed: pack_resp.visibility_changed,
        }
    }

    pub fn render_at_index(&mut self, ui: &mut egui::Ui, index: usize) -> Nip51SetWidgetResponse {
        let Some(set) = self.state.at_index(index) else {
            return Nip51SetWidgetResponse {
                action: None,
                rendered: false,
                visibility_changed: false,
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
    pub visibility_changed: bool,
}

fn should_skip(set: &Nip51Set, required: &Nip51SetWidgetFlags) -> bool {
    (required.contains(Nip51SetWidgetFlags::REQUIRES_TITLE) && set.title.is_none())
        || (required.contains(Nip51SetWidgetFlags::REQUIRES_IMAGE) && set.image.is_none())
        || (required.contains(Nip51SetWidgetFlags::REQUIRES_DESCRIPTION)
            && set.description.is_none())
        || (required.contains(Nip51SetWidgetFlags::NON_EMPTY_PKS) && set.pks.is_empty())
}

/// Internal response type from rendering a follow pack.  Tracks user actions and whether the
/// pack's visibility state changed.
struct RenderPackResponse {
    action: Option<Nip51SetWidgetAction>,
    visibility_changed: bool,
}

/// Renders a "Select All" checkbox for a follow pack.
/// When toggled, applies the selection state to all profiles in the pack.
fn select_all_ui(
    pack: &Nip51Set,
    ui_state: &mut Nip51SetUiCache,
    loc: &mut Localization,
    ui: &mut egui::Ui,
) {
    let select_all_resp = ui.checkbox(
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

    let new_select_all_state = if select_all_resp.clicked() {
        Some(*ui_state.get_select_all_state(&pack.identifier))
    } else {
        None
    };

    if let Some(use_state) = new_select_all_state {
        ui_state.apply_select_all_to_pack(&pack.identifier, &pack.pks, use_state);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_pack(
    ui: &mut egui::Ui,
    pack: &Nip51Set,
    ui_state: &mut Nip51SetUiCache,
    ndb: &Ndb,
    images: &mut Images,
    jobs: &MediaJobSender,
    loc: &mut Localization,
    image_trusted: bool,
) -> RenderPackResponse {
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

    ui.add_space(4.0);

    let mut action = None;

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

    let pack_len = pack.pks.len();
    let default_open = pack_len < 6;

    let r = egui::CollapsingHeader::new(format!("{} people", pack_len))
        .default_open(default_open)
        .show(ui, |ui| {
            select_all_ui(pack, ui_state, loc, ui);

            let txn = Transaction::new(ndb).expect("txn");

            for pk in &pack.pks {
                let m_profile = ndb.get_profile_by_pubkey(&txn, pk.bytes()).ok();

                let cur_state = ui_state.get_pk_selected_state(&pack.identifier, pk);

                crate::hline(ui);

                if render_profile_item(ui, images, jobs, m_profile.as_ref(), cur_state).clicked() {
                    action = Some(Nip51SetWidgetAction::ViewProfile(*pk));
                }
            }
        });

    let visibility_changed = r.header_response.clicked();
    RenderPackResponse {
        action,
        visibility_changed,
    }
}

const PFP_SIZE: f32 = 32.0;

fn render_profile_item(
    ui: &mut egui::Ui,
    images: &mut Images,
    jobs: &MediaJobSender,
    profile: Option<&ProfileRecord>,
    checked: &mut bool,
) -> egui::Response {
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

    let pfp_response = ui
        .allocate_new_ui(UiBuilder::new().max_rect(pfp_rect), |ui| {
            ui.add(
                &mut ProfilePic::new(images, jobs, get_profile_url(profile))
                    .sense(Sense::click())
                    .size(PFP_SIZE),
            )
        })
        .inner;

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
                NotedeckTextStyle::Small.get_font_id(ui.ctx()),
                crate::colors::MID_GRAY,
            );

            let pos = {
                let mut pos = name_rect.min;
                pos.x = left_x_pos;

                let padding = name_rect.height() - galley.rect.height();

                pos.y += padding * 2.5;

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
                RichText::new(about)
                    .color(ui.visuals().weak_text_color())
                    .size(get_font_size(ui.ctx(), &NotedeckTextStyle::Small)),
            )
            .selectable(false)
            .truncate(),
        );
    });

    ui.advance_cursor_after_rect(description_rect);

    clicked_response = clicked_response.union(resp.response);

    if clicked_response.clicked() {
        *checked = !*checked;
    }

    pfp_response
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
    /// Gets or creates a mutable reference to the UI state for a given pack identifier.  If the
    /// pack state doesn't exist, it will be initialized with default values.
    fn entry_for_pack(&mut self, identifier: &str) -> &mut Nip51SetUiState {
        match self.state.raw_entry_mut().from_key(identifier) {
            RawEntryMut::Occupied(entry) => entry.into_mut(),
            RawEntryMut::Vacant(entry) => {
                let (_, pack_state) =
                    entry.insert(identifier.to_owned(), Nip51SetUiState::default());

                pack_state
            }
        }
    }

    pub fn get_pk_selected_state(&mut self, identifier: &str, pk: &Pubkey) -> &mut bool {
        let pack_state = self.entry_for_pack(identifier);

        match pack_state.select_pk.raw_entry_mut().from_key(pk) {
            RawEntryMut::Occupied(entry) => entry.into_mut(),
            RawEntryMut::Vacant(entry) => {
                let (_, state) = entry.insert(*pk, false);
                state
            }
        }
    }

    pub fn get_select_all_state(&mut self, identifier: &str) -> &mut bool {
        &mut self.entry_for_pack(identifier).select_all
    }

    /// Applies a selection state to all profiles in a pack.  Updates both the pack's select_all
    /// flag and individual profile selection states.
    pub fn apply_select_all_to_pack(&mut self, identifier: &str, pks: &[Pubkey], value: bool) {
        let pack_state = self.entry_for_pack(identifier);
        pack_state.select_all = value;

        for pk in pks {
            pack_state.select_pk.insert(*pk, value);
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
