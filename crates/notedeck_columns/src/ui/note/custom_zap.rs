use egui::{
    emath::GuiRounding, pos2, vec2, Color32, CornerRadius, FontId, Frame, Label, Layout, Slider,
    Stroke,
};
use enostr::Pubkey;
use nostrdb::{Ndb, ProfileRecord, Transaction};
use notedeck::{
    fonts::get_font_size, get_profile_url, name::get_display_name, tr, Images, Localization,
    NotedeckTextStyle,
};
use notedeck_ui::{
    app_images, colors, profile::display_name_widget, widgets::styled_button_toggleable,
    AnimationHelper, ProfilePic,
};

pub struct CustomZapView<'a> {
    images: &'a mut Images,
    ndb: &'a Ndb,
    txn: &'a Transaction,
    target_pubkey: &'a Pubkey,
    default_msats: u64,
    i18n: &'a mut Localization,
}

#[allow(clippy::new_without_default)]
impl<'a> CustomZapView<'a> {
    pub fn new(
        i18n: &'a mut Localization,
        images: &'a mut Images,
        ndb: &'a Ndb,
        txn: &'a Transaction,
        target_pubkey: &'a Pubkey,
        default_msats: u64,
    ) -> Self {
        Self {
            target_pubkey,
            images,
            ndb,
            txn,
            default_msats,
            i18n,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> Option<u64> {
        egui::Frame::NONE
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| self.ui_internal(ui))
            .inner
    }

    fn ui_internal(&mut self, ui: &mut egui::Ui) -> Option<u64> {
        show_title(ui, self.i18n);

        ui.add_space(16.0);

        let profile = self
            .ndb
            .get_profile_by_pubkey(self.txn, self.target_pubkey.bytes())
            .ok();
        let profile = profile.as_ref();
        show_profile(ui, self.images, profile);

        ui.add_space(8.0);

        let slider_width = {
            let desired_slider_width = ui.available_width() * 0.6;
            if desired_slider_width < 224.0 {
                224.0
            } else {
                desired_slider_width
            }
        };

        let id = ui.id().with(("CustomZap", self.target_pubkey));

        let default_sats = self.default_msats / 1000;
        ui.with_layout(Layout::top_down(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 16.0);
            ui.spacing_mut().slider_width = slider_width;

            let mut cur_amount = if let Some(input) = ui.data(|d| d.get_temp(id)) {
                input
            } else {
                (self.default_msats / 1000).to_string()
            };
            show_amount(ui, self.i18n, id, &mut cur_amount, slider_width);
            let mut maybe_sats = cur_amount.parse::<u64>().ok();

            let prev_slider_sats = maybe_sats.unwrap_or(default_sats).clamp(1, 100000);
            let mut slider_sats = prev_slider_sats;
            ui.allocate_new_ui(egui::UiBuilder::new(), |ui| {
                ui.set_width(slider_width);
                ui.add(
                    Slider::new(&mut slider_sats, 1..=100000)
                        .logarithmic(true)
                        .trailing_fill(true)
                        .show_value(false),
                );
            });

            if slider_sats != prev_slider_sats {
                cur_amount = slider_sats.to_string();
                maybe_sats = Some(slider_sats);
            }

            if let Some(selection) = show_selection_buttons(ui, maybe_sats, self.i18n) {
                cur_amount = selection.to_string();
                maybe_sats = Some(selection);
            }

            ui.data_mut(|d| d.insert_temp(id, cur_amount));

            let resp = ui.add(styled_button_toggleable(
                &tr!(self.i18n, "Send", "Button label to send a zap"),
                colors::PINK,
                is_valid_zap(maybe_sats),
            ));

            if resp.clicked() {
                maybe_sats.map(|i| i * 1000)
            } else {
                None
            }
        })
        .inner
    }
}

fn is_valid_zap(amount: Option<u64>) -> bool {
    amount.is_some_and(|sats| sats > 0)
}

fn show_title(ui: &mut egui::Ui, i18n: &mut Localization) {
    let max_size = 32.0;
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), max_size),
        Layout::left_to_right(egui::Align::Center),
        |ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(max_size, max_size), egui::Sense::hover());
            let painter = ui.painter_at(rect);
            let circle_color = lerp_color(
                egui::Color32::from_rgb(0xFF, 0xB7, 0x57),
                ui.visuals().noninteractive().bg_fill,
                0.5,
            );
            painter.circle_filled(rect.center(), max_size / 2.0, circle_color);

            let zap_max_width = 25.16;
            let zap_max_height = 29.34;
            let img = app_images::filled_zap_image()
                .max_width(zap_max_width)
                .max_height(zap_max_height);

            let img_rect = rect
                .shrink2(vec2(max_size - zap_max_width, max_size - zap_max_height))
                .round_to_pixel_center(ui.pixels_per_point());
            img.paint_at(ui, img_rect);

            ui.add_space(8.0);

            ui.add(egui::Label::new(
                egui::RichText::new(tr!(i18n, "Zap", "Heading for zap (tip) action"))
                    .text_style(NotedeckTextStyle::Heading2.text_style()),
            ));
        },
    );
}

fn show_profile(ui: &mut egui::Ui, images: &mut Images, profile: Option<&ProfileRecord>) {
    let max_size = 24.0;
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), max_size),
        Layout::left_to_right(egui::Align::Center).with_main_wrap(true),
        |ui| {
            ui.add(&mut ProfilePic::new(images, get_profile_url(profile)).size(max_size));
            ui.vertical(|ui| {
                ui.add(display_name_widget(&get_display_name(profile), false));
            });
        },
    );
}

fn show_amount(
    ui: &mut egui::Ui,
    i18n: &mut Localization,
    id: egui::Id,
    user_input: &mut String,
    width: f32,
) {
    let user_input_font = NotedeckTextStyle::Heading.get_bolded_font(ui.ctx());

    let user_input_id = id.with("sats_amount");

    let user_input_galley = ui.painter().layout_no_wrap(
        user_input.to_owned(),
        user_input_font.clone(),
        ui.visuals().text_color(),
    );

    let painter = ui.painter();

    let sats_galley = painter.layout_no_wrap(
        tr!(
            i18n,
            "SATS",
            "Label for satoshis (Bitcoin unit) for custom zap amount input field"
        ),
        NotedeckTextStyle::Heading4.get_font_id(ui.ctx()),
        ui.visuals().noninteractive().text_color(),
    );

    let user_input_rect = {
        let mut rect = user_input_galley.rect;
        rect.extend_with_x(user_input_galley.rect.left() - 8.0);
        rect
    };
    let sats_width = sats_galley.rect.width() + 8.0;

    Frame::NONE
        .fill(ui.visuals().noninteractive().weak_bg_fill)
        .corner_radius(8)
        .show(ui, |ui| {
            ui.set_width(width);
            ui.add_space(8.0);
            ui.with_layout(Layout::top_down(egui::Align::Center), |ui| {
                let textedit = egui::TextEdit::singleline(user_input)
                    .frame(false)
                    .id(user_input_id)
                    .font(user_input_font);

                let amount_resp = ui.add(Label::new(
                    egui::RichText::new(tr!(i18n, "Amount", "Label for zap amount input field"))
                        .text_style(NotedeckTextStyle::Heading3.text_style())
                        .color(ui.visuals().noninteractive().text_color()),
                ));

                let user_input_padding = {
                    let available_width = ui.available_width();
                    if user_input_rect.width() + sats_width > available_width {
                        0.0
                    } else if (user_input_rect.width() / 2.0) + sats_width > (available_width / 2.0)
                    {
                        available_width - sats_width - user_input_rect.width()
                    } else {
                        (available_width / 2.0) - (user_input_rect.width() / 2.0)
                    }
                };

                let user_input_rect = {
                    let max_input_width = ui.available_width() - sats_width;

                    let user_input_size = if user_input_rect.width() > max_input_width {
                        vec2(max_input_width, user_input_rect.height())
                    } else {
                        user_input_rect.size()
                    };

                    let user_input_pos = pos2(
                        ui.available_rect_before_wrap().left() + user_input_padding,
                        amount_resp.rect.bottom(),
                    );
                    egui::Rect::from_min_size(user_input_pos, user_input_size)
                        .intersect(ui.available_rect_before_wrap())
                };

                let textout = ui
                    .allocate_new_ui(
                        egui::UiBuilder::new()
                            .max_rect(user_input_rect)
                            .layout(Layout::centered_and_justified(egui::Direction::TopDown)),
                        |ui| textedit.show(ui),
                    )
                    .inner;

                let out_rect = textout.text_clip_rect;

                ui.advance_cursor_after_rect(out_rect);

                let sats_pos = pos2(
                    out_rect.right() + 8.0,
                    out_rect.center().y - (sats_galley.rect.height() / 2.0),
                );

                let sats_rect = egui::Rect::from_min_size(sats_pos, sats_galley.size());
                ui.painter()
                    .galley(sats_pos, sats_galley, ui.visuals().text_color());

                ui.advance_cursor_after_rect(sats_rect);

                if !is_valid_zap(user_input.parse::<u64>().ok()) {
                    ui.colored_label(ui.visuals().warn_fg_color, "Please enter valid amount.");
                }
                ui.add_space(8.0);
            });
        });
}

const SELECTION_BUTTONS: [ZapSelectionButton; 8] = [
    ZapSelectionButton::First,
    ZapSelectionButton::Second,
    ZapSelectionButton::Third,
    ZapSelectionButton::Fourth,
    ZapSelectionButton::Fifth,
    ZapSelectionButton::Sixth,
    ZapSelectionButton::Seventh,
    ZapSelectionButton::Eighth,
];

fn show_selection_buttons(
    ui: &mut egui::Ui,
    sats_selection: Option<u64>,
    i18n: &mut Localization,
) -> Option<u64> {
    let mut our_selection = None;
    ui.allocate_ui_with_layout(
        vec2(224.0, 116.0),
        Layout::left_to_right(egui::Align::Min).with_main_wrap(true),
        |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);

            for button in SELECTION_BUTTONS {
                our_selection =
                    our_selection.or(show_selection_button(ui, sats_selection, button, i18n));
            }
        },
    );

    our_selection
}

fn show_selection_button(
    ui: &mut egui::Ui,
    sats_selection: Option<u64>,
    button: ZapSelectionButton,
    i18n: &mut Localization,
) -> Option<u64> {
    let (rect, _) = ui.allocate_exact_size(vec2(50.0, 50.0), egui::Sense::click());
    let helper = AnimationHelper::new_from_rect(ui, ("zap_selection_button", &button), rect);
    let painter = ui.painter();

    let corner = CornerRadius::same(8);
    painter.rect_filled(rect, corner, ui.visuals().noninteractive().weak_bg_fill);

    let amount = button.sats();
    let current_selected = if let Some(selection) = sats_selection {
        selection == amount
    } else {
        false
    };

    if current_selected {
        painter.rect_stroke(
            rect,
            corner,
            Stroke {
                width: 1.0,
                color: colors::PINK,
            },
            egui::StrokeKind::Inside,
        );
    }

    let fontid = FontId::new(
        helper.scale_1d_pos(get_font_size(ui.ctx(), &NotedeckTextStyle::Body)),
        NotedeckTextStyle::Body.font_family(),
    );

    let galley = painter.layout_no_wrap(
        button.to_desc_string(i18n),
        fontid,
        ui.visuals().text_color(),
    );
    let text_rect = {
        let mut galley_rect = galley.rect;
        galley_rect.set_center(rect.center());
        galley_rect
    };

    painter.galley(text_rect.min, galley, ui.visuals().text_color());

    if helper.take_animation_response().clicked() {
        return Some(amount);
    }

    None
}

#[derive(Hash)]
enum ZapSelectionButton {
    First,
    Second,
    Third,
    Fourth,
    Fifth,
    Sixth,
    Seventh,
    Eighth,
}

impl ZapSelectionButton {
    pub fn sats(&self) -> u64 {
        match self {
            ZapSelectionButton::First => 69,
            ZapSelectionButton::Second => 100,
            ZapSelectionButton::Third => 420,
            ZapSelectionButton::Fourth => 5_000,
            ZapSelectionButton::Fifth => 10_000,
            ZapSelectionButton::Sixth => 20_000,
            ZapSelectionButton::Seventh => 50_000,
            ZapSelectionButton::Eighth => 100_000,
        }
    }

    pub fn to_desc_string(&self, i18n: &mut Localization) -> String {
        match self {
            ZapSelectionButton::First => "69".to_string(),
            ZapSelectionButton::Second => "100".to_string(),
            ZapSelectionButton::Third => "420".to_string(),
            ZapSelectionButton::Fourth => tr!(i18n, "5K", "Zap amount button for 5000 sats. Abbreviated because the button is too small to display the full amount."),
            ZapSelectionButton::Fifth => tr!(i18n, "10K", "Zap amount button for 10000 sats. Abbreviated because the button is too small to display the full amount."),
            ZapSelectionButton::Sixth => tr!(i18n, "20K", "Zap amount button for 20000 sats. Abbreviated because the button is too small to display the full amount."),
            ZapSelectionButton::Seventh => tr!(i18n, "50K", "Zap amount button for 50000 sats. Abbreviated because the button is too small to display the full amount."),
            ZapSelectionButton::Eighth => tr!(i18n, "100K", "Zap amount button for 100000 sats. Abbreviated because the button is too small to display the full amount."),
        }
    }
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    Color32::from_rgba_premultiplied(
        egui::lerp(a.r() as f32..=b.r() as f32, t) as u8,
        egui::lerp(a.g() as f32..=b.g() as f32, t) as u8,
        egui::lerp(a.b() as f32..=b.b() as f32, t) as u8,
        egui::lerp(a.a() as f32..=b.a() as f32, t) as u8,
    )
}
