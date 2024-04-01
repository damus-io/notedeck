use crate::colors::{
    ALMOST_WHITE, DARKER_BG, DARK_BG_1, DARK_ISH_BG, GRAY_SECONDARY, RED_700, SEMI_DARKER_BG, WHITE,
};
use crate::key_parsing::{perform_key_retrieval, LoginError};
use crate::login_manager::LoginManager;
use egui::{
    epaint::Shadow, Align, Align2, Button, Color32, Frame, Id, LayerId, Margin, Pos2, Rect,
    RichText, Rounding, Ui, Vec2, Window,
};

pub struct AccountLoginView<'a> {
    ctx: &'a egui::Context,
    manager: &'a mut LoginManager,
    generate_y_intercept: Option<f32>,
}

impl<'a> AccountLoginView<'a> {
    pub fn new(ctx: &'a egui::Context, manager: &'a mut LoginManager) -> Self {
        AccountLoginView {
            ctx,
            manager,
            generate_y_intercept: None,
        }
    }
    pub fn panel(&mut self) {
        let frame = egui::CentralPanel::default().frame(Frame {
            inner_margin: Margin::same(0.0),
            fill: DARKER_BG,
            ..Default::default()
        });

        let screen_width = self.ctx.screen_rect().max.x;
        let screen_height = self.ctx.screen_rect().max.y;

        frame.show(self.ctx, |ui| {
            let title_layer = LayerId::new(egui::Order::Background, Id::new("Title layer"));

            let mut top_panel_height: Option<f32> = None;
            ui.with_layer_id(title_layer, |ui| {
                egui::TopBottomPanel::top("Top")
                    .resizable(false)
                    .default_height(340.0)
                    .frame(Frame::none())
                    .show_separator_line(false)
                    .show_inside(ui, |ui| {
                        top_panel_height = Some(ui.available_rect_before_wrap().bottom());
                        self.top_title_area(ui);
                    });
            });

            egui::TopBottomPanel::bottom("Bottom")
                .resizable(false)
                .frame(Frame::none())
                .show_separator_line(false)
                .show_inside(ui, |ui| {
                    self.window(ui, top_panel_height.unwrap_or(0.0));
                });

            let top_rect = Rect {
                min: Pos2::ZERO,
                max: Pos2::new(
                    screen_width,
                    self.generate_y_intercept.unwrap_or(screen_height * 0.5),
                ),
            };

            ui.painter_at(top_rect)
                .with_layer_id(LayerId::background())
                .rect_filled(top_rect, Rounding::ZERO, DARK_ISH_BG);
        });
    }

    fn window(&mut self, ui: &mut Ui, top_panel_height: f32) {
        let needed_height_over_top = (self.ctx.screen_rect().bottom() / 2.0) - 230.0;
        let y_offset = if top_panel_height > needed_height_over_top {
            top_panel_height - needed_height_over_top
        } else {
            0.0
        };
        Window::new("Account login")
            .movable(false)
            .constrain(true)
            .collapsible(false)
            .drag_to_scroll(false)
            .title_bar(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0f32, y_offset])
            .max_width(538.0)
            .frame(
                egui::Frame::default()
                    .fill(DARK_ISH_BG)
                    .rounding(egui::Rounding::from(32f32))
                    .stroke(egui::Stroke::new(1.0, DARK_BG_1))
                    .shadow(Shadow {
                        offset: [0.0, 8.0].into(),
                        blur: 24.0,
                        spread: 0.0,
                        color: egui::Color32::from_rgba_unmultiplied(0x6D, 0x6D, 0x6D, 0x14),
                    }),
            )
            .show(ui.ctx(), |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.label(
                        RichText::new("Login")
                            .size(24f32)
                            .color(WHITE)
                            .strong()
                            .line_height(Some(36f32)),
                    );

                    ui.add_space(16f32);

                    ui.label(
                        RichText::new("Enter your private key to start using Notedeck")
                            .size(13f32)
                            .color(GRAY_SECONDARY)
                            .line_height(Some(19.5)),
                    );

                    ui.add_space(24.0);

                    Frame::none()
                        .outer_margin(Margin::symmetric(48.0, 0.0))
                        .show(ui, |ui| {
                            self.login_form(ui);
                        });

                    ui.add_space(32.0);

                    let y_margin: f32 = 24.0;
                    let generate_frame = egui::Frame::default()
                        .fill(Color32::from_rgb(0x26, 0x26, 0x26)) // TODO: gradient
                        .rounding(egui::Rounding::from(32f32))
                        .stroke(egui::Stroke::new(1.0, DARK_BG_1))
                        .inner_margin(Margin::symmetric(48.0, y_margin));

                    generate_frame.show(ui, |ui| {
                        self.generate_y_intercept =
                            Some(ui.available_rect_before_wrap().top() - y_margin);
                        self.generate_group(ui);
                    });
                });
            });
    }

    fn top_title_area(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            let logo_gradient_data = egui::include_image!("../assets/Logo-Gradient-2x.png");
            ui.add(egui::Image::new(logo_gradient_data).max_width(232.0));

            ui.add_space(48.0);

            let welcome_data = egui::include_image!("../assets/Welcome to Nostrdeck 2x.png");
            ui.add(egui::Image::new(welcome_data).max_width(528.0));

            ui.add_space(12.0);

            // ui.label(
            //     RichText::new("Welcome to Nostrdeck")
            //         .size(48.0)
            //         .strong()
            //         .line_height(Some(72.0)),
            // );
            ui.label(
                RichText::new("The best alternative to tweetDeck built in nostr protocol")
                    .size(24.0)
                    .line_height(Some(36.0))
                    .color(ALMOST_WHITE),
            );
        });
    }

    fn login_form(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered_justified(|ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Enter your key")
                        .color(WHITE)
                        .strong()
                        .line_height(Some(19.5f32))
                        .size(13f32),
                );
            });

            ui.add_space(8f32);

            ui.add(
                egui::TextEdit::singleline(&mut self.manager.login_key)
                    .hint_text(
                        RichText::new("Your key here...")
                            .size(13f32)
                            .line_height(Some(19.5f32))
                            .color(GRAY_SECONDARY),
                    )
                    .margin(Margin::symmetric(12.0, 12.0))
                    .min_size(Vec2::new(440.0, 40.0))
                    .vertical_align(Align::Center),
            );

            ui.add_space(8.0);

            ui.vertical_centered(|ui| {
                if self.manager.promise.is_some() {
                    ui.add(egui::Spinner::new());
                }
            });

            if let Some(error_key) = &self.manager.key_on_error {
                if self.manager.login_key != *error_key {
                    self.manager.error = None;
                    self.manager.key_on_error = None;
                }
            }
            if let Some(err) = &self.manager.error {
                ui.horizontal(|ui| {
                    let error_label = match err {
                        LoginError::InvalidKey => {
                            egui::Label::new(RichText::new("Invalid key.").color(RED_700))
                        }
                        LoginError::Nip05Failed(e) => {
                            egui::Label::new(RichText::new(e).color(RED_700))
                        }
                    };
                    ui.add(error_label.truncate(true));
                });
            }

            ui.add_space(8.0);

            let login_button = Button::new(
                RichText::new("Login now — let's do this!")
                    .line_height(Some(16.25f32))
                    .strong()
                    .size(13f32)
                    .color(WHITE),
            )
            .rounding(Rounding::same(8f32))
            .min_size(Vec2::new(442.0, 40.0))
            .fill(Color32::from_rgb(0xF8, 0x69, 0xB6)); // TODO: gradient

            if ui.add(login_button).clicked() {
                self.manager.promise = Some(perform_key_retrieval(&self.manager.login_key));
            }
        });
    }

    fn generate_group(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("New in nostr?")
                    .size(20f32)
                    .line_height(Some(30f32))
                    .color(ALMOST_WHITE),
            );

            ui.label(
                RichText::new(" — we got you!")
                    .size(20f32)
                    .line_height(Some(30f32))
                    .color(GRAY_SECONDARY),
            );
        });

        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Quickly generate your keys. Make sure you save them safely.")
                    .color(GRAY_SECONDARY)
                    .size(13f32)
                    .line_height(Some(19.5f32)),
            );
        });

        ui.add_space(16.0);

        let generate_button = Button::new(RichText::new("Generate keys").color(WHITE))
            .fill(SEMI_DARKER_BG)
            .min_size(Vec2::new(442.0, 40.0))
            .rounding(Rounding::same(8.0));
        if ui.add(generate_button).clicked() {
            // TODO: keygen
        }
    }
}
