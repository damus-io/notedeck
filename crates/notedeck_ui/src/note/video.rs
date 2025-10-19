use egui::{
    vec2, Align, Align2, Area, Color32, FontId, Key, Layout, Order, Pos2, Rect, Response, RichText,
    Sense, Ui, UiBuilder, WidgetText,
};
use egui_video::AudioDevice;
use egui_video::{ensure_initialized, Player, PlayerState};
use hex::encode;
use poll_promise::Promise;
use sdl2;
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::HashMap,
    env,
    fs::{self, File},
    io::copy,
    path::PathBuf,
};
use ureq;

enum VideoSlot {
    Loading {
        promise: Promise<Result<String, String>>,
    },
    Ready {
        player: Player,
        started: bool,
    },
    Failed(String),
}

enum VideoStatus {
    Loading,
    Error(String),
}

thread_local! {
    static VIDEO_PLAYERS: RefCell<HashMap<String, VideoSlot>> = RefCell::new(HashMap::new());
    static AUDIO_SUPPORT: RefCell<Option<AudioSupport>> = RefCell::new(None);
    static FULLSCREEN_VIDEOS: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new());
}

struct AudioSupport {
    #[allow(dead_code)]
    sdl: sdl2::Sdl,
    device: AudioDevice,
}

/// Render embedded video players for the provided URLs.
pub fn show_video_embeds(ui: &mut Ui, urls: &[String]) {
    for url in urls {
        ui.add_space(6.0);
        ui.vertical(
            |ui| match with_player(url, ui, |player, ui| render_player(ui, url, player)) {
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
                            reset_player(url);
                        }
                        ui.hyperlink_to(
                            WidgetText::RichText(
                                RichText::new("Open original").color(ui.visuals().hyperlink_color),
                            ),
                            url,
                        );
                    });
                }
            },
        );
    }
}

fn render_player(ui: &mut Ui, url: &str, player: &mut Player) -> Response {
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

    sync_audio_with_state(current_state);

    let button_size = vec2(32.0, 32.0);
    let button_pos = video_resp.rect.right_top() - vec2(button_size.x + 8.0, -8.0);
    let button_rect = Rect::from_min_size(button_pos, button_size);
    let fullscreen_label = if is_fullscreen(url) { "⤡" } else { "⤢" };
    let fullscreen_resp = ui.put(
        button_rect,
        egui::Button::new(fullscreen_label)
            .fill(ui.visuals().extreme_bg_color)
            .corner_radius(8.0)
            .frame(true),
    );

    if fullscreen_resp.clicked() {
        toggle_fullscreen(url);
    }

    if is_fullscreen(url) {
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
                    set_fullscreen(url, false);
                }

                let builder = UiBuilder::new().max_rect(overlay_rect);
                let mut overlay_ui = area_ui.new_child(builder);

                overlay_ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                    if ui
                        .add(egui::Button::new("✕").fill(ui.visuals().extreme_bg_color))
                        .clicked()
                    {
                        set_fullscreen(url, false);
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
                    sync_audio_with_state(state);
                });
            });
    }

    response = response.union(fullscreen_resp);

    response
}

fn with_player<F, R>(url: &str, ui: &mut Ui, f: F) -> Result<R, VideoStatus>
where
    F: FnOnce(&mut Player, &mut Ui) -> R,
{
    ensure_initialized();
    VIDEO_PLAYERS.with(|store| {
        let mut map = store.borrow_mut();

        loop {
            let entry = map
                .entry(url.to_string())
                .or_insert_with(|| VideoSlot::Loading {
                    promise: spawn_video_fetch(url),
                });

            match entry {
                VideoSlot::Loading { promise } => {
                    if let Some(result) = promise.ready().cloned() {
                        match result {
                            Ok(local_path) => match prepare_player(ui, &local_path) {
                                Ok(player) => {
                                    *entry = VideoSlot::Ready {
                                        player,
                                        started: false,
                                    };
                                    continue;
                                }
                                Err(err) => {
                                    *entry = VideoSlot::Failed(err);
                                    continue;
                                }
                            },
                            Err(err) => {
                                *entry = VideoSlot::Failed(err);
                                continue;
                            }
                        }
                    } else {
                        return Err(VideoStatus::Loading);
                    }
                }
                VideoSlot::Ready { player, started } => {
                    if !*started {
                        player.start();
                        *started = true;
                        sync_audio_with_state(PlayerState::Playing);
                    }
                    return Ok(f(player, ui));
                }
                VideoSlot::Failed(err) => return Err(VideoStatus::Error(err.clone())),
            }
        }
    })
}

fn reset_player(url: &str) {
    VIDEO_PLAYERS.with(|store| {
        store.borrow_mut().remove(url);
    });
}

fn prepare_player(ui: &Ui, path: &str) -> Result<Player, String> {
    let player = Player::new(ui.ctx(), &path.to_owned()).map_err(|err| format!("{err:?}"))?;
    attach_audio(player)
}

fn attach_audio(player: Player) -> Result<Player, String> {
    AUDIO_SUPPORT.with(|support| {
        let mut support = support.borrow_mut();
        if support.is_none() {
            let sdl = sdl2::init().map_err(|e| e.to_string())?;
            let audio_subsystem = sdl.audio().map_err(|e| e.to_string())?;
            let device = egui_video::init_audio_device(&audio_subsystem).map_err(|e| e)?;
            *support = Some(AudioSupport { sdl, device });
        }
        if let Some(state) = support.as_mut() {
            let player = player
                .with_audio(&mut state.device)
                .map_err(|e| format!("{e:?}"))?;
            return Ok(player);
        }
        Err("Failed to initialize audio support".to_string())
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

fn sync_audio_with_state(state: PlayerState) {
    let paused = matches!(
        state,
        PlayerState::Paused | PlayerState::Stopped | PlayerState::EndOfFile
    );
    set_audio_playback(paused);
}

fn set_audio_playback(paused: bool) {
    AUDIO_SUPPORT.with(|support| {
        if let Some(state) = support.borrow_mut().as_mut() {
            if paused {
                state.device.pause();
            } else {
                state.device.resume();
            }
        }
    });
}

fn toggle_fullscreen(url: &str) {
    let new_state = !is_fullscreen(url);
    set_fullscreen(url, new_state);
}

fn set_fullscreen(url: &str, value: bool) {
    FULLSCREEN_VIDEOS.with(|map| {
        let mut map = map.borrow_mut();
        if value {
            map.insert(url.to_owned(), true);
        } else {
            map.remove(url);
        }
    });
}

fn is_fullscreen(url: &str) -> bool {
    FULLSCREEN_VIDEOS.with(|map| map.borrow().get(url).copied().unwrap_or(false))
}
