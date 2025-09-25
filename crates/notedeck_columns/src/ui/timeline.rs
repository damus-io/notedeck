use egui::containers::scroll_area::ScrollBarVisibility;
use egui::{vec2, Color32, Direction, Layout, Margin, Pos2, RichText, ScrollArea, Sense, Stroke};
use egui_tabs::TabColor;
use enostr::Pubkey;
use nostrdb::{Note, ProfileRecord, Transaction};
use notedeck::fonts::get_font_size;
use notedeck::name::get_display_name;
use notedeck::ui::is_narrow;
use notedeck::{tr_plural, JobsCache, Muted, NotedeckTextStyle};
use notedeck_ui::app_images::{like_image, repost_image};
use notedeck_ui::{ProfilePic, ProfilePreview};
use std::f32::consts::PI;
use tracing::{error, warn};

use crate::nav::BodyResponse;
use crate::timeline::{
    CompositeType, CompositeUnit, NoteUnit, ReactionUnit, RepostUnit, TimelineCache, TimelineKind,
    TimelineTab, ViewFilter,
};
use notedeck::{
    note::root_note_id_from_selected_id, tr, Localization, NoteAction, NoteContext, ScrollInfo,
};
use notedeck_ui::{
    anim::{AnimationHelper, ICON_EXPANSION_MULTIPLE},
    NoteOptions, NoteView,
};

pub struct TimelineView<'a, 'd> {
    timeline_id: &'a TimelineKind,
    timeline_cache: &'a mut TimelineCache,
    note_options: NoteOptions,
    note_context: &'a mut NoteContext<'d>,
    jobs: &'a mut JobsCache,
    col: usize,
    scroll_to_top: bool,
}

impl<'a, 'd> TimelineView<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        timeline_id: &'a TimelineKind,
        timeline_cache: &'a mut TimelineCache,
        note_context: &'a mut NoteContext<'d>,
        note_options: NoteOptions,
        jobs: &'a mut JobsCache,
        col: usize,
    ) -> Self {
        let scroll_to_top = false;
        TimelineView {
            timeline_id,
            timeline_cache,
            note_options,
            note_context,
            jobs,
            col,
            scroll_to_top,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> BodyResponse<NoteAction> {
        timeline_ui(
            ui,
            self.timeline_id,
            self.timeline_cache,
            self.note_options,
            self.note_context,
            self.jobs,
            self.col,
            self.scroll_to_top,
        )
    }

    pub fn scroll_to_top(mut self, enable: bool) -> Self {
        self.scroll_to_top = enable;
        self
    }

    pub fn scroll_id(
        timeline_cache: &TimelineCache,
        timeline_id: &TimelineKind,
        col: usize,
    ) -> Option<egui::Id> {
        let timeline = timeline_cache.get(timeline_id)?;
        Some(egui::Id::new(("tlscroll", timeline.view_id(col))))
    }
}

#[allow(clippy::too_many_arguments)]
fn timeline_ui(
    ui: &mut egui::Ui,
    timeline_id: &TimelineKind,
    timeline_cache: &mut TimelineCache,
    mut note_options: NoteOptions,
    note_context: &mut NoteContext,
    jobs: &mut JobsCache,
    col: usize,
    scroll_to_top: bool,
) -> BodyResponse<NoteAction> {
    //padding(4.0, ui, |ui| ui.heading("Notifications"));
    /*
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let row_height = ui.fonts(|f| f.row_height(&font_id)) + ui.spacing().item_spacing.y;

    */

    let Some(scroll_id) = TimelineView::scroll_id(timeline_cache, timeline_id, col) else {
        return BodyResponse::none();
    };

    {
        let timeline = if let Some(timeline) = timeline_cache.get_mut(timeline_id) {
            timeline
        } else {
            error!("tried to render timeline in column, but timeline was missing");
            // TODO (jb55): render error when timeline is missing?
            // this shouldn't happen...
            return BodyResponse::none();
        };

        timeline.selected_view = tabs_ui(
            ui,
            note_context.i18n,
            timeline.selected_view,
            &timeline.views,
        )
        .inner;

        // need this for some reason??
        ui.add_space(3.0);
    };

    let show_top_button_id = ui.id().with((scroll_id, "at_top"));

    let show_top_button = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(show_top_button_id))
        .unwrap_or(false);

    let goto_top_resp = if show_top_button {
        let top_button_pos_x = if is_narrow(ui.ctx()) { 28.0 } else { 48.0 };
        let top_button_pos =
            ui.available_rect_before_wrap().right_top() - vec2(top_button_pos_x, -24.0);
        egui::Area::new(ui.id().with("foreground_area"))
            .order(egui::Order::Middle)
            .fixed_pos(top_button_pos)
            .show(ui.ctx(), |ui| Some(ui.add(goto_top_button(top_button_pos))))
            .inner
            .map(|r| r.on_hover_cursor(egui::CursorIcon::PointingHand))
    } else {
        None
    };

    let mut scroll_area = egui::ScrollArea::vertical()
        .id_salt(scroll_id)
        .animated(false)
        .auto_shrink([false, false])
        .scroll_bar_visibility(ScrollBarVisibility::AlwaysVisible);

    if goto_top_resp.is_some_and(|r| r.clicked()) {
        scroll_area = scroll_area.vertical_scroll_offset(0.0);
    }

    // chrome can ask to scroll to top as well via an app option
    if scroll_to_top {
        scroll_area = scroll_area.vertical_scroll_offset(0.0);
    }

    let scroll_output = scroll_area.show(ui, |ui| {
        let timeline = if let Some(timeline) = timeline_cache.get(timeline_id) {
            timeline
        } else {
            error!("tried to render timeline in column, but timeline was missing");
            // TODO (jb55): render error when timeline is missing?
            // this shouldn't happen...
            //
            // NOTE (jb55): it can easily happen if you add a timeline column without calling
            // add_new_timeline_column, since that sets up the initial subs, etc
            return None;
        };

        let txn = Transaction::new(note_context.ndb).expect("failed to create txn");

        if matches!(timeline_id, TimelineKind::Notifications(_)) {
            note_options.set(NoteOptions::Notification, true)
        }

        TimelineTabView::new(
            timeline.current_view(),
            note_options,
            &txn,
            note_context,
            jobs,
        )
        .show(ui)
    });

    let at_top_after_scroll = scroll_output.state.offset.y == 0.0;
    let cur_show_top_button = ui.ctx().data(|d| d.get_temp::<bool>(show_top_button_id));

    if at_top_after_scroll {
        if cur_show_top_button != Some(false) {
            ui.ctx()
                .data_mut(|d| d.insert_temp(show_top_button_id, false));
        }
    } else if cur_show_top_button == Some(false) {
        ui.ctx()
            .data_mut(|d| d.insert_temp(show_top_button_id, true));
    }

    let scroll_id = scroll_output.id;

    let action = scroll_output.inner.or_else(|| {
        // if we're scrolling, return that as a response. We need this
        // for auto-closing the side menu

        let velocity = scroll_output.state.velocity();
        let offset = scroll_output.state.offset;
        if velocity.length_sq() > 0.0 {
            Some(NoteAction::Scroll(ScrollInfo { velocity, offset }))
        } else {
            None
        }
    });

    BodyResponse::output(action).scroll_raw(scroll_id)
}

fn goto_top_button(center: Pos2) -> impl egui::Widget {
    move |ui: &mut egui::Ui| -> egui::Response {
        let radius = 12.0;
        let max_size = vec2(
            ICON_EXPANSION_MULTIPLE * 2.0 * radius,
            ICON_EXPANSION_MULTIPLE * 2.0 * radius,
        );
        let helper = AnimationHelper::new_from_rect(ui, "goto_top", {
            let painter = ui.painter();
            #[allow(deprecated)]
            let center = painter.round_pos_to_pixel_center(center);
            egui::Rect::from_center_size(center, max_size)
        });

        let painter = ui.painter();
        painter.circle_filled(
            center,
            helper.scale_1d_pos(radius),
            notedeck_ui::colors::PINK,
        );

        let create_pt = |angle: f32| {
            let side = radius / 2.0;
            let x = side * angle.cos();
            let mut y = side * angle.sin();

            let height = (side * (3.0_f32).sqrt()) / 2.0;
            y += height / 2.0;
            Pos2 { x, y }
        };

        #[allow(deprecated)]
        let left_pt =
            painter.round_pos_to_pixel_center(helper.scale_pos_from_center(create_pt(-PI)));
        #[allow(deprecated)]
        let center_pt =
            painter.round_pos_to_pixel_center(helper.scale_pos_from_center(create_pt(-PI / 2.0)));
        #[allow(deprecated)]
        let right_pt =
            painter.round_pos_to_pixel_center(helper.scale_pos_from_center(create_pt(0.0)));

        let line_width = helper.scale_1d_pos(4.0);
        let line_color = ui.visuals().text_color();
        painter.line_segment([left_pt, center_pt], Stroke::new(line_width, line_color));
        painter.line_segment([center_pt, right_pt], Stroke::new(line_width, line_color));

        let end_radius = (line_width - 1.0) / 2.0;
        painter.circle_filled(left_pt, end_radius, line_color);
        painter.circle_filled(center_pt, end_radius, line_color);
        painter.circle_filled(right_pt, end_radius, line_color);

        helper.take_animation_response()
    }
}

pub fn tabs_ui(
    ui: &mut egui::Ui,
    i18n: &mut Localization,
    selected: usize,
    views: &[TimelineTab],
) -> egui::InnerResponse<usize> {
    ui.spacing_mut().item_spacing.y = 0.0;

    let tab_res = egui_tabs::Tabs::new(views.len() as i32)
        .selected(selected as i32)
        .hover_bg(TabColor::none())
        .selected_fg(TabColor::none())
        .selected_bg(TabColor::none())
        .hover_bg(TabColor::none())
        //.hover_bg(TabColor::custom(egui::Color32::RED))
        .height(32.0)
        .layout(Layout::centered_and_justified(Direction::TopDown))
        .show(ui, |ui, state| {
            ui.spacing_mut().item_spacing.y = 0.0;

            let ind = state.index();

            let txt = match views[ind as usize].filter {
                ViewFilter::Notes => tr!(i18n, "Notes", "Label for notes-only filter"),
                ViewFilter::NotesAndReplies => {
                    tr!(
                        i18n,
                        "Notes & Replies",
                        "Label for notes and replies filter"
                    )
                }
            };

            let res = ui.add(egui::Label::new(txt.clone()).selectable(false));

            // underline
            if state.is_selected() {
                let rect = res.rect;
                let underline =
                    shrink_range_to_width(rect.x_range(), get_label_width(ui, &txt) * 1.15);
                #[allow(deprecated)]
                let underline_y = ui.painter().round_to_pixel(rect.bottom()) - 1.5;
                return (underline, underline_y);
            }

            (egui::Rangef::new(0.0, 0.0), 0.0)
        });

    //ui.add_space(0.5);
    notedeck_ui::hline(ui);

    let sel = tab_res.selected().unwrap_or_default();

    let res_inner = &tab_res.inner()[sel as usize];

    let (underline, underline_y) = res_inner.inner;
    let underline_width = underline.span();

    let tab_anim_id = ui.id().with("tab_anim");
    let tab_anim_size = tab_anim_id.with("size");

    let stroke = egui::Stroke {
        color: ui.visuals().hyperlink_color,
        width: 2.0,
    };

    let speed = 0.1f32;

    // animate underline position
    let x = ui
        .ctx()
        .animate_value_with_time(tab_anim_id, underline.min, speed);

    // animate underline width
    let w = ui
        .ctx()
        .animate_value_with_time(tab_anim_size, underline_width, speed);

    let underline = egui::Rangef::new(x, x + w);

    ui.painter().hline(underline, underline_y, stroke);

    egui::InnerResponse::new(sel as usize, res_inner.response.clone())
}

fn get_label_width(ui: &mut egui::Ui, text: &str) -> f32 {
    let font_id = egui::FontId::default();
    let galley = ui.fonts(|r| r.layout_no_wrap(text.to_string(), font_id, egui::Color32::WHITE));
    galley.rect.width()
}

fn shrink_range_to_width(range: egui::Rangef, width: f32) -> egui::Rangef {
    let midpoint = (range.min + range.max) / 2.0;
    let half_width = width / 2.0;

    let min = midpoint - half_width;
    let max = midpoint + half_width;

    egui::Rangef::new(min, max)
}

pub struct TimelineTabView<'a, 'd> {
    tab: &'a TimelineTab,
    note_options: NoteOptions,
    txn: &'a Transaction,
    note_context: &'a mut NoteContext<'d>,
    jobs: &'a mut JobsCache,
}

impl<'a, 'd> TimelineTabView<'a, 'd> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tab: &'a TimelineTab,
        note_options: NoteOptions,
        txn: &'a Transaction,
        note_context: &'a mut NoteContext<'d>,
        jobs: &'a mut JobsCache,
    ) -> Self {
        Self {
            tab,
            note_options,
            txn,
            note_context,
            jobs,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<NoteAction> {
        let mut action: Option<NoteAction> = None;
        let len = self.tab.units.len();

        let mute = self.note_context.accounts.mute();

        self.tab
            .list
            .borrow_mut()
            .ui_custom_layout(ui, len, |ui, index| {
                // tracing::info!("rendering index: {index}");
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.spacing_mut().item_spacing.x = 4.0;

                let Some(entry) = self.tab.units.get(index) else {
                    return 0;
                };

                match self.render_entry(ui, entry, &mute) {
                    RenderEntryResponse::Unsuccessful => return 0,

                    RenderEntryResponse::Success(note_action) => {
                        if let Some(cur_action) = note_action {
                            action = Some(cur_action);
                        }
                    }
                }

                1
            });

        action
    }

    fn render_entry(
        &mut self,
        ui: &mut egui::Ui,
        entry: &NoteUnit,
        mute: &std::sync::Arc<Muted>,
    ) -> RenderEntryResponse {
        let underlying_note = {
            let underlying_note_key = match entry {
                NoteUnit::Single(note_ref) => note_ref.key,
                NoteUnit::Composite(composite_unit) => match composite_unit {
                    CompositeUnit::Reaction(reaction_unit) => reaction_unit.note_reacted_to.key,
                    CompositeUnit::Repost(repost_unit) => repost_unit.note_reposted.key,
                },
            };

            let Ok(note) = self
                .note_context
                .ndb
                .get_note_by_key(self.txn, underlying_note_key)
            else {
                warn!("failed to query note {:?}", underlying_note_key);
                return RenderEntryResponse::Unsuccessful;
            };

            note
        };

        let muted = root_note_id_from_selected_id(
            self.note_context.ndb,
            self.note_context.note_cache,
            self.txn,
            underlying_note.id(),
        )
        .is_ok_and(|root_id| mute.is_muted(&underlying_note, root_id.bytes()));

        if muted {
            return RenderEntryResponse::Success(None);
        }

        match entry {
            NoteUnit::Single(_) => render_note(
                ui,
                self.note_context,
                self.note_options,
                self.jobs,
                &underlying_note,
            ),
            NoteUnit::Composite(composite) => match composite {
                CompositeUnit::Reaction(reaction_unit) => render_reaction_cluster(
                    ui,
                    self.note_context,
                    self.note_options,
                    self.jobs,
                    mute,
                    self.txn,
                    &underlying_note,
                    reaction_unit,
                ),
                CompositeUnit::Repost(repost_unit) => render_repost_cluster(
                    ui,
                    self.note_context,
                    self.note_options,
                    self.jobs,
                    mute,
                    self.txn,
                    &underlying_note,
                    repost_unit,
                ),
            },
        }
    }
}

enum ReferencedNoteType {
    Tagged,
    Yours,
}

impl CompositeType {
    fn image(&self, darkmode: bool) -> egui::Image<'static> {
        match self {
            CompositeType::Reaction => like_image(),
            CompositeType::Repost => {
                repost_image(darkmode).tint(Color32::from_rgb(0x68, 0xC3, 0x51))
            }
        }
    }

    fn description(
        &self,
        loc: &mut Localization,
        first_name: &str,
        total_count: usize,
        referenced_type: ReferencedNoteType,
        notification: bool,
    ) -> String {
        let count = total_count - 1;

        match self {
            CompositeType::Reaction => {
                reaction_description(loc, first_name, count, referenced_type)
            }
            CompositeType::Repost => repost_description(
                loc,
                first_name,
                count,
                if notification {
                    DescriptionType::Notification(referenced_type)
                } else {
                    DescriptionType::Other
                },
            ),
        }
    }
}

fn reaction_description(
    loc: &mut Localization,
    first_name: &str,
    count: usize,
    referenced_type: ReferencedNoteType,
) -> String {
    match referenced_type {
        ReferencedNoteType::Tagged => {
            if count == 0 {
                tr!(
                    loc,
                    "{name} reacted to a note you were tagged in",
                    "reaction from user to a note you were tagged in",
                    name = first_name
                )
            } else {
                tr_plural!(
                    loc,
                    "{name} and {count} other reacted to a note you were tagged in",
                    "{name} and {count} others reacted to a note you were tagged in",
                    "amount of reactions a note you were tagged in received",
                    count,
                    name = first_name
                )
            }
        }
        ReferencedNoteType::Yours => {
            if count == 0 {
                tr!(
                    loc,
                    "{name} reacted to your note",
                    "reaction from user to your note",
                    name = first_name
                )
            } else {
                tr_plural!(
                    loc,
                    "{name} and {count} other reacted to your note",
                    "{name} and {count} others reacted to your note",
                    "describing the amount of reactions your note received",
                    count,
                    name = first_name
                )
            }
        }
    }
}

enum DescriptionType {
    Notification(ReferencedNoteType),
    Other,
}

fn repost_description(
    loc: &mut Localization,
    first_name: &str,
    count: usize,
    description_type: DescriptionType,
) -> String {
    match description_type {
        DescriptionType::Notification(referenced_type) => match referenced_type {
            ReferencedNoteType::Tagged => {
                if count == 0 {
                    tr!(
                        loc,
                        "{name} reposted a note you were tagged in",
                        "repost from user",
                        name = first_name
                    )
                } else {
                    tr_plural!(
                        loc,
                        "{name} and {count} other reposted a note you were tagged in",
                        "{name} and {count} others reposted a note you were tagged in",
                        "describing the amount of reposts a note you were tagged in received",
                        count,
                        name = first_name
                    )
                }
            }
            ReferencedNoteType::Yours => {
                if count == 0 {
                    tr!(
                        loc,
                        "{name} reposted your note",
                        "repost from user",
                        name = first_name
                    )
                } else {
                    tr_plural!(
                        loc,
                        "{name} and {count} other reposted your note",
                        "{name} and {count} others reposted your note",
                        "describing the amount of reposts your note received",
                        count,
                        name = first_name
                    )
                }
            }
        },
        DescriptionType::Other => {
            if count == 0 {
                tr!(
                    loc,
                    "{name} reposted",
                    "repost from user",
                    name = first_name
                )
            } else {
                tr_plural!(
                    loc,
                    "{name} and {count} other reposted",
                    "{name} and {count} others reposted",
                    "describing the amount of reposts a note has",
                    count,
                    name = first_name
                )
            }
        }
    }
}

fn render_note(
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    note_options: NoteOptions,
    jobs: &mut JobsCache,
    note: &Note,
) -> RenderEntryResponse {
    let mut action = None;
    notedeck_ui::padding(8.0, ui, |ui| {
        let resp = NoteView::new(note_context, note, note_options, jobs).show(ui);

        if let Some(note_action) = resp.action {
            action = Some(note_action);
        }
    });

    notedeck_ui::hline(ui);

    RenderEntryResponse::Success(action)
}

#[allow(clippy::too_many_arguments)]
fn render_reaction_cluster(
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    note_options: NoteOptions,
    jobs: &mut JobsCache,
    mute: &std::sync::Arc<Muted>,
    txn: &Transaction,
    underlying_note: &Note,
    reaction: &ReactionUnit,
) -> RenderEntryResponse {
    let profiles_to_show: Vec<ProfileEntry> = reaction
        .reactions
        .values()
        .filter(|r| !mute.is_pk_muted(r.sender.bytes()))
        .map(|r| &r.sender)
        .map(|p| ProfileEntry {
            record: note_context.ndb.get_profile_by_pubkey(txn, p.bytes()).ok(),
            pk: p,
        })
        .collect();

    render_composite_entry(
        ui,
        note_context,
        note_options | NoteOptions::Notification,
        jobs,
        underlying_note,
        profiles_to_show,
        CompositeType::Reaction,
    )
}

#[allow(clippy::too_many_arguments)]
fn render_composite_entry(
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    mut note_options: NoteOptions,
    jobs: &mut JobsCache,
    underlying_note: &nostrdb::Note<'_>,
    profiles_to_show: Vec<ProfileEntry>,
    composite_type: CompositeType,
) -> RenderEntryResponse {
    let first_name = get_display_name(profiles_to_show.iter().find_map(|opt| opt.record.as_ref()))
        .name()
        .to_string();
    let num_profiles = profiles_to_show.len();

    let mut action = None;

    let referenced_type = if note_context
        .accounts
        .get_selected_account()
        .key
        .pubkey
        .bytes()
        != underlying_note.pubkey()
    {
        ReferencedNoteType::Tagged
    } else {
        ReferencedNoteType::Yours
    };

    if !note_options.contains(NoteOptions::TrustMedia) {
        let acc = note_context.accounts.get_selected_account();
        for entry in &profiles_to_show {
            if matches!(acc.is_following(entry.pk), notedeck::IsFollowing::Yes) {
                note_options = note_options.union(NoteOptions::TrustMedia);
                break;
            }
        }
    }

    egui::Frame::new()
        .inner_margin(Margin::symmetric(8, 4))
        .show(ui, |ui| {
            let show_label_newline = ui
                .horizontal_wrapped(|ui| {
                    let pfps_resp = ui
                        .allocate_ui_with_layout(
                            vec2(ui.available_width(), 32.0),
                            Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                render_profiles(
                                    ui,
                                    profiles_to_show,
                                    &composite_type,
                                    note_context.img_cache,
                                    note_options.contains(NoteOptions::Notification),
                                )
                            },
                        )
                        .inner;

                    if let Some(cur_action) = pfps_resp.action {
                        action = Some(cur_action);
                    }

                    let description = composite_type.description(
                        note_context.i18n,
                        &first_name,
                        num_profiles,
                        referenced_type,
                        note_options.contains(NoteOptions::Notification),
                    );
                    let galley = ui.painter().layout_no_wrap(
                        description.clone(),
                        NotedeckTextStyle::Small.get_font_id(ui.ctx()),
                        ui.visuals().text_color(),
                    );

                    ui.add_space(4.0);

                    let galley_pos = {
                        let mut galley_pos = ui.next_widget_position();
                        galley_pos.y = pfps_resp.resp.rect.right_center().y;
                        galley_pos.y -= galley.rect.height() / 2.0;
                        galley_pos
                    };

                    let fits_no_wrap = {
                        let mut rightmost_pos = galley_pos;
                        rightmost_pos.x += galley.rect.width();

                        ui.available_rect_before_wrap().contains(rightmost_pos)
                    };

                    if fits_no_wrap {
                        ui.painter()
                            .galley(galley_pos, galley, ui.visuals().text_color());
                        None
                    } else {
                        Some(description)
                    }
                })
                .inner;

            if let Some(desc) = show_label_newline {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(48.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.add(egui::Label::new(
                            RichText::new(desc)
                                .size(get_font_size(ui.ctx(), &NotedeckTextStyle::Small)),
                        ));
                    });
                });
            }

            ui.add_space(16.0);

            let resp = ui
                .horizontal(|ui| {
                    if note_options.contains(NoteOptions::Notification) {
                        note_options = note_options
                            .difference(NoteOptions::ActionBar | NoteOptions::OptionsButton)
                            .union(NoteOptions::NotificationPreview);

                        ui.add_space(48.0);
                    };
                    NoteView::new(note_context, underlying_note, note_options, jobs).show(ui)
                })
                .inner;

            if let Some(note_action) = resp.action {
                action.get_or_insert(note_action);
            }
        });

    notedeck_ui::hline(ui);
    RenderEntryResponse::Success(action)
}

fn render_profiles(
    ui: &mut egui::Ui,
    profiles_to_show: Vec<ProfileEntry>,
    composite_type: &CompositeType,
    img_cache: &mut notedeck::Images,
    notification: bool,
) -> PfpsResponse {
    let mut action = None;
    if notification {
        ui.add_space(8.0);
    }

    ui.vertical(|ui| {
        ui.add_space(9.0);
        ui.add_sized(
            vec2(20.0, 20.0),
            composite_type.image(ui.visuals().dark_mode),
        );
    });

    if notification {
        ui.add_space(16.0);
    } else {
        ui.add_space(2.0);
    }

    let resp = ui.horizontal(|ui| {
        ScrollArea::horizontal()
            .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                let mut last_pfp_resp = None;
                for entry in profiles_to_show {
                    let mut resp = ui.add(
                        &mut ProfilePic::from_profile_or_default(img_cache, entry.record.as_ref())
                            .size(24.0)
                            .sense(Sense::click()),
                    );

                    if let Some(record) = entry.record.as_ref() {
                        resp = resp.on_hover_ui_at_pointer(|ui| {
                            ui.set_max_width(300.0);
                            ui.add(ProfilePreview::new(record, img_cache));
                        });
                    }

                    last_pfp_resp = Some(resp.clone());

                    if resp.clicked() {
                        action = Some(NoteAction::Profile(*entry.pk))
                    }
                }

                last_pfp_resp
            })
            .inner
    });

    let resp = if let Some(r) = resp.inner {
        r
    } else {
        resp.response
    };

    PfpsResponse { action, resp }
}

struct PfpsResponse {
    action: Option<NoteAction>,
    resp: egui::Response,
}

#[allow(clippy::too_many_arguments)]
fn render_repost_cluster(
    ui: &mut egui::Ui,
    note_context: &mut NoteContext,
    note_options: NoteOptions,
    jobs: &mut JobsCache,
    mute: &std::sync::Arc<Muted>,
    txn: &Transaction,
    underlying_note: &Note,
    repost: &RepostUnit,
) -> RenderEntryResponse {
    let profiles_to_show: Vec<ProfileEntry> = repost
        .reposts
        .values()
        .filter(|r| !mute.is_pk_muted(r.bytes()))
        .map(|p| ProfileEntry {
            record: note_context.ndb.get_profile_by_pubkey(txn, p.bytes()).ok(),
            pk: p,
        })
        .collect();

    render_composite_entry(
        ui,
        note_context,
        note_options,
        jobs,
        underlying_note,
        profiles_to_show,
        CompositeType::Repost,
    )
}

enum RenderEntryResponse {
    Unsuccessful,
    Success(Option<NoteAction>),
}

struct ProfileEntry<'a> {
    record: Option<ProfileRecord<'a>>,
    pk: &'a Pubkey,
}
