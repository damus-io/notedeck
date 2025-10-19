extern crate ffmpeg_next as ffmpeg;
use eframe::NativeOptions;
use egui::{CentralPanel, Grid, Sense, Slider, TextEdit, Window};
use egui_video::{AudioDevice, Player};
fn main() {
    let _ = eframe::run_native(
        "app",
        NativeOptions::default(),
        Box::new(|_| Box::new(App::default())),
    );
}
struct App {
    audio_device: AudioDevice,
    media_path: String,
    stream_size_scale: f32,
    player: Option<Player>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            audio_device: egui_video::init_audio_device(&sdl2::init().unwrap().audio().unwrap())
                .unwrap(),
            media_path: String::new(),
            stream_size_scale: 1.,
            player: None,
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint();
        CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!self.media_path.is_empty(), |ui| {
                    if ui.button("load").clicked() {
                        match Player::new(ctx, &self.media_path.replace("\"", ""))
                            .and_then(|p| p.with_audio(&mut self.audio_device))
                        {
                            Ok(player) => {
                                self.player = Some(player);
                            }
                            Err(e) => println!("failed to make stream: {e}"),
                        }
                    }
                });
                ui.add_enabled_ui(!self.media_path.is_empty(), |ui| {
                    if ui.button("clear").clicked() {
                        self.player = None;
                    }
                });

                let tedit_resp = ui.add_sized(
                    [ui.available_width(), ui.available_height()],
                    TextEdit::singleline(&mut self.media_path)
                        .hint_text("click to set path")
                        .interactive(false),
                );

                if ui
                    .interact(
                        tedit_resp.rect,
                        tedit_resp.id.with("click_sense"),
                        Sense::click(),
                    )
                    .clicked()
                {
                    if let Some(path_buf) = rfd::FileDialog::new().add_filter("videos", &["mp4", "gif", "webm"]).pick_file() {
                        self.media_path = path_buf.as_path().to_string_lossy().to_string();
                    }
                }
            });
            ui.separator();
            if let Some(player) = self.player.as_mut() {
                Window::new("info").show(ctx, |ui| {
                    Grid::new("info_grid").show(ui, |ui| {
                        ui.label("frame rate");
                        ui.label(player.framerate.to_string());
                        ui.end_row();

                        ui.label("size");
                        ui.label(format!("{}x{}", player.width, player.height));
                        ui.end_row();

                        ui.label("elapsed / duration");
                        ui.label(player.duration_text());
                        ui.end_row();

                        ui.label("state");
                        ui.label(format!("{:?}", player.player_state.get()));
                        ui.end_row();

                        ui.label("has audio?");
                        ui.label(player.audio_streamer.is_some().to_string());
                        ui.end_row();
                    });
                });
                Window::new("controls").show(ctx, |ui| {
                    ui.checkbox(&mut player.looping, "loop");
                    ui.horizontal(|ui| {
                        ui.label("size scale");
                        ui.add(Slider::new(&mut self.stream_size_scale, 0.0..=2.));
                    });
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("play").clicked() {
                            player.start()
                        }
                        if ui.button("unpause").clicked() {
                            player.unpause();
                        }
                        if ui.button("pause").clicked() {
                            player.pause();
                        }
                        if ui.button("stop").clicked() {
                            player.stop();
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("volume");
                        let mut volume = player.audio_volume.get();
                        if ui.add(Slider::new(&mut volume, 0.0..=player.max_audio_volume)).changed() {
                            player.audio_volume.set(volume);
                        };

                    });
                });

                player.ui(
                    ui,
                    [
                        player.width as f32 * self.stream_size_scale,
                        player.height as f32 * self.stream_size_scale,
                    ],
                );
            }
        });
    }
}
