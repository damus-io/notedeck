use nostrdb::ProfileRecord;

pub mod context;
pub mod name;
pub mod picture;
pub mod preview;

pub use picture::ProfilePic;
pub use preview::ProfilePreview;

use egui::{Label, RichText, TextureHandle};
use notedeck::media::images::ImageType;
use notedeck::media::AnimationMode;
use notedeck::{
    Images, IsFollowing, MediaJobSender, NostrName, NotedeckTextStyle, PointDimensions,
};

use crate::{app_images, colors, widgets::styled_button_toggleable};

pub fn display_name_widget<'a>(
    name: &'a NostrName<'a>,
    add_placeholder_space: bool,
) -> impl egui::Widget + 'a {
    move |ui: &mut egui::Ui| -> egui::Response {
        let disp_resp = name.display_name.map(|disp_name| {
            ui.add(
                Label::new(
                    RichText::new(disp_name).text_style(NotedeckTextStyle::Heading3.text_style()),
                )
                .selectable(false),
            )
        });

        let (username_resp, nip05_resp) = ui
            .horizontal_wrapped(|ui| {
                let username_resp = name.username.map(|username| {
                    ui.add(
                        Label::new(
                            RichText::new(format!("@{username}"))
                                .size(16.0)
                                .color(crate::colors::MID_GRAY),
                        )
                        .selectable(false),
                    )
                });

                if name.username.is_some() && name.nip05.is_some() {
                    ui.end_row();
                }

                let nip05_resp = name.nip05.map(|nip05| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;

                        ui.add(app_images::verified_image());

                        ui.label(RichText::new(nip05).size(16.0).color(crate::colors::TEAL))
                            .on_hover_text(nip05)
                    })
                    .inner
                });

                (username_resp, nip05_resp)
            })
            .inner;

        let resp = match (disp_resp, username_resp, nip05_resp) {
            (Some(disp), Some(username), Some(nip05)) => disp.union(username).union(nip05),
            (Some(disp), Some(username), None) => disp.union(username),
            (Some(disp), None, None) => disp,
            (None, Some(username), Some(nip05)) => username.union(nip05),
            (None, Some(username), None) => username,
            _ => ui.add(Label::new(RichText::new(name.name()))),
        };

        if add_placeholder_space {
            ui.add_space(16.0);
        }

        resp
    }
}

pub fn about_section_widget<'a>(profile: Option<&'a ProfileRecord<'a>>) -> impl egui::Widget + 'a {
    move |ui: &mut egui::Ui| {
        if let Some(about) = profile
            .map(|p| p.record().profile())
            .and_then(|p| p.and_then(|p| p.about()))
        {
            let resp = ui.label(about);
            ui.add_space(8.0);
            resp
        } else {
            // need any Response so we dont need an Option
            ui.allocate_response(egui::Vec2::ZERO, egui::Sense::hover())
        }
    }
}

/// Loads a banner texture using the shared media cache to prevent blocking.
#[profiling::function]
pub fn banner_texture<'a>(
    ui: &mut egui::Ui,
    cache: &'a mut Images,
    jobs: &MediaJobSender,
    banner_url: &str,
    size: PointDimensions,
) -> Option<&'a TextureHandle> {
    if banner_url.is_empty() {
        return None;
    }

    cache.latest_texture(
        jobs,
        ui,
        banner_url,
        ImageType::Content(Some(size.to_pixels(ui))),
        AnimationMode::NoAnimation,
    )
}

/// Renders a profile banner via the cached loader so we avoid egui_extras overhead.
#[profiling::function]
pub fn banner(
    ui: &mut egui::Ui,
    cache: &mut Images,
    jobs: &MediaJobSender,
    banner_url: Option<&str>,
    height: f32,
) -> egui::Response {
    let x = ui.available_size().x;
    ui.add_sized([x, height], |ui: &mut egui::Ui| {
        banner_url
            .and_then(|url| banner_texture(ui, cache, jobs, url, PointDimensions { x, y: height }))
            .map(|texture| {
                let size = texture.size_vec2();
                let aspect_ratio = if size.y == 0.0 { 1.0 } else { size.x / size.y };

                notedeck::media::images::aspect_fill(
                    ui,
                    egui::Sense::hover(),
                    texture.id(),
                    aspect_ratio,
                )
            })
            .unwrap_or_else(|| empty_banner(ui))
    })
}

/// Draws an empty banner placeholder while the image loads or is missing.
fn empty_banner(ui: &mut egui::Ui) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
    response
}

pub fn follow_button(following: IsFollowing) -> impl egui::Widget + 'static {
    move |ui: &mut egui::Ui| -> egui::Response {
        let (bg_color, text) = match following {
            IsFollowing::Unknown => (ui.visuals().noninteractive().bg_fill, "Unknown"),
            IsFollowing::Yes => (ui.visuals().widgets.inactive.bg_fill, "Unfollow"),
            IsFollowing::No => (colors::PINK, "Follow"),
        };

        let enabled = following != IsFollowing::Unknown;
        ui.add(styled_button_toggleable(text, bg_color, enabled))
    }
}
