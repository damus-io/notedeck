use egui::{
    vec2, Align, Align2, Area, Color32, FontId, Key, Layout, Order, Pos2, Rect, Response, RichText,
    Sense, Ui, UiBuilder, WidgetText,
};
use egui_video::{ensure_initialized, Player, PlayerState};
use hex::encode;
use notedeck::{AudioSupport, VideoSlot, VideoStore};
use poll_promise::Promise;
use sdl2;
use sha2::{Digest, Sha256};
use std::{
    env,
    fs::{self, File},
    io::copy,
    path::PathBuf,
};
use ureq;

enum VideoStatus {
    Loading,
    Error(String),
}

/// Render embedded video players for the provided URLs.
pub fn show_video_embeds(ui: &mut Ui, store: &VideoStore, urls: &[String]) {
    for url in urls {
        ui.add_space(6.0);
        ui.vertical(|ui| {
            match with_player(store, url, ui, |player, ui| {
                render_player(store, ui, url, player)
            }) {
                Ok(response) => {
                    let _ = response;
                    ui.horizontal(|ui| {
                        ui.add_space(4.0);
                        ui.hyperlink_to(
                            WidgetText::RichText(
                                RichText::new("Open original").color(ui.visuals().hyperlink_color),
                            ),
                            url,
                        );
                    });
                }
                Err(VideoStatus::Loading) => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Loading video…");
                    });
                    ui.hyperlink_to(
                        WidgetText::RichText(
                            RichText::new("Open original").color(ui.visuals().hyperlink_color),
                        ),
                        url,
                    );
                }
                Err(VideoStatus::Error(err)) => {
                    ui.colored_label(
                        Color32::from_rgb(220, 80, 80),
                        format!("Video failed to load: {err}"),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Retry").clicked() {
                            reset_player(store, url);
                        }
                        ui.hyperlink_to(
                            WidgetText::RichText(
                                RichText::new("Open original").color(ui.visuals().hyperlink_color),
                            ),
                            url,
                        );
                    });
                }
            }
        });
    }
}

fn render_player(store: &VideoStore, ui: &mut Ui, url: &str, player: &mut Player) -> Response {
    let max_height = 360.0;
    let available_width = ui.available_width();

    let aspect = if player.width == 0 {
        16.0 / 9.0
    } else {
        player.height as f32 / player.width as f32
    };

    let mut desired_height = available_width * aspect;
    let mut desired_width = available_width;

    if desired_height > max_height {
        desired_height = max_height;
        desired_width = desired_height / aspect;
    }

    let video_resp = ui
        .vertical_centered(|ui| player.ui(ui, [desired_width, desired_height]))
        .inner;

    let click_resp = ui.interact(
        video_resp.rect,
        video_resp.id.with("video_toggle"),
        egui::Sense::click(),
    );

    let mut response = video_resp.union(click_resp.clone());

    let mut current_state = player.player_state.get();

    if click_resp.clicked() {
        match current_state {
            PlayerState::Stopped | PlayerState::EndOfFile => player.start(),
            PlayerState::Paused => player.unpause(),
            PlayerState::Playing => player.pause(),
            PlayerState::Seeking(_) => {}
        }
        current_state = player.player_state.get();
    }

    if matches!(
        current_state,
        PlayerState::Stopped | PlayerState::Paused | PlayerState::EndOfFile
    ) {
        let icon = match current_state {
            PlayerState::Paused => "⏸",
            PlayerState::EndOfFile => "⟳",
            _ => "▶",
        };
        let icon_pos = response.rect.center();
        ui.painter().text(
            icon_pos,
            Align2::CENTER_CENTER,
            icon,
            FontId::proportional(42.0),
            ui.visuals().strong_text_color(),
        );
    }

    sync_audio_with_state(store, current_state);

    let button_size = vec2(32.0, 32.0);
    let button_pos = video_resp.rect.right_top() - vec2(button_size.x + 8.0, -8.0);
    let button_rect = Rect::from_min_size(button_pos, button_size);
    let fullscreen_label = if is_fullscreen(store, url) {
        "⤡"
    } else {
        "⤢"
    };
    let fullscreen_resp = ui.put(
        button_rect,
        egui::Button::new(fullscreen_label)
            .fill(ui.visuals().extreme_bg_color)
            .corner_radius(8.0)
            .frame(true),
    );

    if fullscreen_resp.clicked() {
        toggle_fullscreen(store, url);
    }

    if is_fullscreen(store, url) {
        let ctx = ui.ctx();
        let screen_rect = ctx.screen_rect();
        Area::new(egui::Id::new(format!("video_fullscreen_{url}")))
            .order(Order::Foreground)
            .fixed_pos(screen_rect.min)
            .show(ctx, |area_ui| {
                let overlay_rect = Rect::from_min_size(Pos2::ZERO, screen_rect.size());
                area_ui.set_min_size(screen_rect.size());
                area_ui
                    .painter()
                    .rect_filled(overlay_rect, 0.0, Color32::from_black_alpha(210));

                if area_ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
                    set_fullscreen(store, url, false);
                }

                let builder = UiBuilder::new().max_rect(overlay_rect);
                let mut overlay_ui = area_ui.new_child(builder);

                overlay_ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                    if ui
                        .add(egui::Button::new("✕").fill(ui.visuals().extreme_bg_color))
                        .clicked()
                    {
                        set_fullscreen(store, url, false);
                    }
                });

                overlay_ui.centered_and_justified(|ui| {
                    let avail = screen_rect.size();
                    let width = avail.x.max(320.0);
                    let height = (width * aspect).min(avail.y.max(180.0));
                    let video_resp = player.ui(ui, [width, height]);
                    let click_resp = ui.interact(
                        video_resp.rect,
                        video_resp.id.with("video_toggle_full"),
                        Sense::click(),
                    );
                    let mut state = player.player_state.get();
                    if click_resp.clicked() {
                        match state {
                            PlayerState::Playing => player.pause(),
                            PlayerState::Paused => player.unpause(),
                            PlayerState::Stopped | PlayerState::EndOfFile => player.start(),
                            PlayerState::Seeking(_) => {}
                        }
                        state = player.player_state.get();
                    }
                    sync_audio_with_state(store, state);
                });
            });
    }

    response = response.union(fullscreen_resp);

    response
}

fn with_player<F, R>(store: &VideoStore, url: &str, ui: &mut Ui, mut f: F) -> Result<R, VideoStatus>
where
    F: FnMut(&mut Player, &mut Ui) -> R,
{
    ensure_initialized();
    #[derive(Debug)]
    enum Action<R> {
        Loading,
        Error(String),
        Use { response: R, state: PlayerState },
        Prepare(String),
    }

    loop {
        let action = store.with_players(|players| {
            let entry = players
                .entry(url.to_string())
                .or_insert_with(|| VideoSlot::Loading {
                    promise: spawn_video_fetch(url),
                });

            match entry {
                VideoSlot::Loading { promise } => {
                    if let Some(result) = promise.ready().cloned() {
                        match result {
                            Ok(local_path) => Action::Prepare(local_path),
                            Err(err) => {
                                *entry = VideoSlot::Failed(err.clone());
                                Action::Error(err)
                            }
                        }
                    } else {
                        Action::Loading
                    }
                }
                VideoSlot::Ready { player, started } => {
                    if !*started {
                        player.start();
                        *started = true;
                    }
                    let response = f(player, ui);
                    let state = player.player_state.get();
                    Action::Use { response, state }
                }
                VideoSlot::Failed(err) => Action::Error(err.clone()),
            }
        });

        match action {
            Action::Loading => return Err(VideoStatus::Loading),
            Action::Error(err) => return Err(VideoStatus::Error(err)),
            Action::Use { response, state } => {
                sync_audio_with_state(store, state);
                return Ok(response);
            }
            Action::Prepare(local_path) => match prepare_player(store, ui, &local_path) {
                Ok(player) => {
                    store.with_players(|players| {
                        players.insert(
                            url.to_string(),
                            VideoSlot::Ready {
                                player,
                                started: false,
                            },
                        );
                    });
                }
                Err(err) => {
                    store.with_players(|players| {
                        players.insert(url.to_string(), VideoSlot::Failed(err.clone()));
                    });
                }
            },
        }
    }
}

fn reset_player(store: &VideoStore, url: &str) {
    store.remove_player(url);
    store.set_fullscreen(url, false);
}

fn prepare_player(store: &VideoStore, ui: &Ui, path: &str) -> Result<Player, String> {
    let player = Player::new(ui.ctx(), &path.to_owned()).map_err(|err| format!("{err:?}"))?;
    attach_audio(store, player)
}

fn attach_audio(store: &VideoStore, player: Player) -> Result<Player, String> {
    store.with_audio(|audio| {
        if audio.is_none() {
            let sdl = sdl2::init().map_err(|e| e.to_string())?;
            let audio_subsystem = sdl.audio().map_err(|e| e.to_string())?;
            let device = egui_video::init_audio_device(&audio_subsystem).map_err(|e| e)?;
            *audio = Some(AudioSupport { sdl, device });
        }

        if let Some(state) = audio.as_mut() {
            player
                .with_audio(&mut state.device)
                .map_err(|e| format!("{e:?}"))
        } else {
            Err("Failed to initialize audio support".to_string())
        }
    })
}

fn spawn_video_fetch(url: &str) -> Promise<Result<String, String>> {
    let url = url.to_owned();
    Promise::spawn_thread("notedeck-video-fetch", move || cache_video(&url))
}

fn cache_video(url: &str) -> Result<String, String> {
    let stripped = url.strip_prefix("file://").unwrap_or(url);
    if !stripped.starts_with("http://") && !stripped.starts_with("https://") {
        return Ok(stripped.to_string());
    }

    let path = video_cache_path(url);

    if !path.exists() {
        let response = ureq::get(url).call().map_err(|e| e.to_string())?;
        if response.status() >= 400 {
            return Err(format!("HTTP {}", response.status()));
        }

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }

        let mut reader = response.into_reader();
        let mut file = File::create(&path).map_err(|e| e.to_string())?;
        copy(&mut reader, &mut file).map_err(|e| e.to_string())?;
    }

    Ok(path.to_string_lossy().into_owned())
}

fn video_cache_path(url: &str) -> PathBuf {
    let mut path = env::temp_dir();
    let hash = Sha256::digest(url.as_bytes());
    path.push(format!("notedeck_video_{}.mp4", encode(hash)));
    path
}

fn sync_audio_with_state(store: &VideoStore, state: PlayerState) {
    let paused = matches!(
        state,
        PlayerState::Paused | PlayerState::Stopped | PlayerState::EndOfFile
    );
    set_audio_playback(store, paused);
}

fn set_audio_playback(store: &VideoStore, paused: bool) {
    store.with_audio(|audio| {
        if let Some(state) = audio.as_mut() {
            if paused {
                state.device.pause();
            } else {
                state.device.resume();
            }
        }
    });
}

fn toggle_fullscreen(store: &VideoStore, url: &str) {
    let new_state = !is_fullscreen(store, url);
    set_fullscreen(store, url, new_state);
}

fn set_fullscreen(store: &VideoStore, url: &str, value: bool) {
    store.set_fullscreen(url, value);
}

fn is_fullscreen(store: &VideoStore, url: &str) -> bool {
    store.is_fullscreen(url)
}
