use std::collections::HashMap;

#[cfg(feature = "inline-playback")]
mod player;

use egui::{self, Color32, RichText, Sense, Vec2};
#[cfg(feature = "inline-playback")]
use egui::{ColorImage, TextureHandle, TextureOptions};
use hex::FromHex;
use nostrdb::{Filter, Note, Transaction};
#[cfg(feature = "inline-playback")]
use notedeck::media::load_texture_checked;
use notedeck::media::{AnimationMode, ImageType};
use notedeck::{
    App, AppContext, AppResponse, UnifiedSubscription, name::get_display_name, note::event_tag,
    time_ago_since, time_format,
};
use opener::open as open_external;
#[cfg(feature = "inline-playback")]
use player::LivestreamPlayer;
use uuid::Uuid;

const LIVESTREAM_KIND: u32 = 30311;
const DEFAULT_LIMIT: usize = 200;
const REFRESH_INTERVAL_SECONDS: f64 = 10.0;

pub struct LivestreamsApp {
    status_filter: StatusFilter,
    search_term: String,
    streams: Vec<LiveEvent>,
    last_query_time: f64,
    refresh_interval: f64,
    manual_refresh: bool,
    query_limit: usize,
    subscription: Option<UnifiedSubscription>,
    filter: Filter,
    error_message: Option<String>,
    selected_key: Option<LiveKey>,
    #[cfg(feature = "inline-playback")]
    player: Option<LivestreamPlayer>,
    #[cfg(feature = "inline-playback")]
    player_init_error: Option<String>,
    #[cfg(feature = "inline-playback")]
    playback_error: Option<String>,
    #[cfg(feature = "inline-playback")]
    player_texture: Option<TextureHandle>,
    #[cfg(feature = "inline-playback")]
    player_texture_size: Option<[usize; 2]>,
    #[cfg(feature = "inline-playback")]
    player_texture_ready: bool,
    #[cfg(feature = "inline-playback")]
    last_frame_version: u64,
    #[cfg(feature = "inline-playback")]
    current_stream_url: Option<String>,
}

impl Default for LivestreamsApp {
    fn default() -> Self {
        let filter = Filter::new()
            .kinds([LIVESTREAM_KIND as u64])
            .limit(DEFAULT_LIMIT as u64)
            .build();

        #[cfg(feature = "inline-playback")]
        let (player, player_init_error) = match LivestreamPlayer::new() {
            Ok(player) => (Some(player), None),
            Err(err) => (None, Some(err.to_string())),
        };

        Self {
            status_filter: StatusFilter::Live,
            search_term: String::new(),
            streams: Vec::new(),
            last_query_time: 0.0,
            refresh_interval: REFRESH_INTERVAL_SECONDS,
            manual_refresh: true,
            query_limit: DEFAULT_LIMIT,
            subscription: None,
            filter,
            error_message: None,
            selected_key: None,
            #[cfg(feature = "inline-playback")]
            player,
            #[cfg(feature = "inline-playback")]
            player_init_error,
            #[cfg(feature = "inline-playback")]
            playback_error: None,
            #[cfg(feature = "inline-playback")]
            player_texture: None,
            #[cfg(feature = "inline-playback")]
            player_texture_size: None,
            #[cfg(feature = "inline-playback")]
            player_texture_ready: false,
            #[cfg(feature = "inline-playback")]
            last_frame_version: 0,
            #[cfg(feature = "inline-playback")]
            current_stream_url: None,
        }
    }
}

impl LivestreamsApp {
    pub fn new() -> Self {
        Self::default()
    }

    fn ensure_subscription(&mut self, ctx: &mut AppContext<'_>) {
        if self.subscription.is_some() {
            return;
        }

        let filters = vec![self.filter.clone()];
        match ctx.ndb.subscribe(&filters) {
            Ok(local) => {
                let subid = format!("livestreams-{}", Uuid::new_v4());
                ctx.pool.subscribe(subid.clone(), filters);
                self.subscription = Some(UnifiedSubscription {
                    local,
                    remote: subid,
                });
                self.manual_refresh = true;
            }
            Err(err) => {
                self.error_message = Some(format!("subscription failed: {err}"));
            }
        }
    }

    fn request_remote_refresh(&mut self, ctx: &mut AppContext<'_>) {
        if let Some(sub) = &self.subscription {
            ctx.pool
                .subscribe(sub.remote.clone(), vec![self.filter.clone()]);
        }
    }

    fn update_streams(&mut self, ctx: &mut AppContext<'_>, now: f64) {
        self.ensure_subscription(ctx);

        if self.manual_refresh {
            self.request_remote_refresh(ctx);
            self.manual_refresh = false;
            self.last_query_time = 0.0;
        }

        if now - self.last_query_time < self.refresh_interval {
            return;
        }

        self.last_query_time = now;

        let txn = match Transaction::new(ctx.ndb) {
            Ok(txn) => txn,
            Err(err) => {
                self.error_message = Some(format!("transaction failed: {err}"));
                return;
            }
        };

        let limit = self.query_limit as i32;
        let query_res = ctx.ndb.query(&txn, &[self.filter.clone()], limit);
        let results = match query_res {
            Ok(res) => res,
            Err(err) => {
                self.error_message = Some(format!("query failed: {err}"));
                return;
            }
        };

        let mut latest: HashMap<LiveKey, LiveEvent> = HashMap::new();
        for qr in results {
            let note = qr.note;
            if note.kind() != LIVESTREAM_KIND {
                continue;
            }

            if let Some(event) = self.build_event(ctx, &txn, &note) {
                latest
                    .entry(event.key.clone())
                    .and_modify(|existing| {
                        if event.updated_at > existing.updated_at {
                            *existing = event.clone();
                        }
                    })
                    .or_insert(event);
            }
        }

        let mut streams: Vec<LiveEvent> = latest.into_values().collect();
        sort_streams(&mut streams);

        if let Some(selected) = &self.selected_key {
            if !streams.iter().any(|ev| &ev.key == selected) {
                self.selected_key = None;
            }
        }

        self.streams = streams;

        #[cfg(feature = "inline-playback")]
        {
            let selected_event = self
                .selected_key
                .as_ref()
                .and_then(|key| self.streams.iter().find(|ev| &ev.key == key))
                .cloned();

            if let Some(event) = selected_event {
                self.sync_selected_stream(&event);
            } else {
                self.stop_stream();
            }
        }
    }

    fn build_event(
        &self,
        ctx: &mut AppContext<'_>,
        txn: &Transaction,
        note: &Note<'_>,
    ) -> Option<LiveEvent> {
        let identifier = event_tag(note, "d")
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{}:{}", hex::encode(note.pubkey()), note.created_at()));
        let key = LiveKey {
            pubkey: *note.pubkey(),
            identifier,
        };

        let title = event_tag(note, "title").map(str::to_owned);
        let summary = event_tag(note, "summary").map(str::to_owned);
        let image_url = event_tag(note, "image").map(str::to_owned);
        let streaming_url = event_tag(note, "streaming").map(str::to_owned);
        let recording_url = event_tag(note, "recording").map(str::to_owned);
        let status = LiveStatus::from_tag(event_tag(note, "status"));
        let starts = parse_u64(event_tag(note, "starts"));
        let ends = parse_u64(event_tag(note, "ends"));
        let current_participants = parse_u64(event_tag(note, "current_participants"));
        let total_participants = parse_u64(event_tag(note, "total_participants"));

        let hashtags = collect_tag_values(note, "t");
        let participants = collect_participants(ctx, txn, note);

        let author_hex = hex::encode(note.pubkey());
        let author_display =
            profile_display_name(ctx, txn, note.pubkey()).unwrap_or_else(|| short_hex(&author_hex));

        let updated_at = note.created_at();

        Some(LiveEvent {
            key,
            title,
            summary,
            image_url,
            streaming_url,
            recording_url,
            status,
            starts,
            ends,
            current_participants,
            total_participants,
            participants,
            hashtags,
            updated_at,
            author_hex,
            author_display,
        })
    }

    fn render_controls(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("livestreams-status-filter")
                .selected_text(self.status_filter.label())
                .show_ui(ui, |ui| {
                    for variant in StatusFilter::all() {
                        ui.selectable_value(&mut self.status_filter, variant, variant.label());
                    }
                });

            let refresh_clicked = ui.button("Refresh").clicked();
            if refresh_clicked {
                self.manual_refresh = true;
            }
        });

        ui.add(
            egui::TextEdit::singleline(&mut self.search_term)
                .hint_text("Search title or summary")
                .desired_width(f32::INFINITY),
        );
    }

    fn render_streams(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) {
        #[cfg(feature = "inline-playback")]
        self.poll_player_errors();

        let filter = self.status_filter;
        let search = self.search_term.trim().to_lowercase();

        let snapshot: Vec<LiveEvent> = self.streams.clone();

        egui::ScrollArea::vertical()
            .id_salt("livestreams-scroll")
            .show(ui, |ui| {
                let mut any = false;
                for event in &snapshot {
                    if !filter.matches(&event.status) {
                        continue;
                    }

                    let matches_search = search.is_empty()
                        || event
                            .title
                            .as_deref()
                            .map(|t| t.to_lowercase().contains(&search))
                            .unwrap_or(false)
                        || event
                            .summary
                            .as_deref()
                            .map(|s| s.to_lowercase().contains(&search))
                            .unwrap_or(false);

                    if !matches_search {
                        continue;
                    }

                    any = true;
                    self.render_event_card(ctx, ui, event);
                    ui.add_space(12.0);
                }

                if !any {
                    ui.centered_and_justified(|ui| {
                        ui.label("No livestreams found.");
                    });
                }
            });
    }

    #[cfg(feature = "inline-playback")]
    fn poll_player_errors(&mut self) {
        if let Some(player) = self.player.as_mut() {
            player.poll_restart();
            if let Some(err) = player.take_error() {
                self.playback_error = Some(err);
                self.stop_stream_internal(false);
            }
        }
    }

    #[cfg(feature = "inline-playback")]
    fn sync_selected_stream(&mut self, event: &LiveEvent) {
        if let Some(url) = event.streaming_url.as_deref() {
            self.start_stream(url);
        } else {
            self.stop_stream();
        }
    }

    #[cfg(feature = "inline-playback")]
    fn start_stream(&mut self, url: &str) {
        if self.player.is_none() {
            if self.player_init_error.is_some() {
                self.playback_error = self.player_init_error.clone();
            } else {
                self.playback_error =
                    Some("Inline playback is unavailable on this platform.".to_owned());
            }
            return;
        }

        if let Some(player) = self.player.as_mut() {
            if self.current_stream_url.as_deref() != Some(url) {
                self.player_texture_ready = false;
                self.player_texture_size = None;
                self.last_frame_version = 0;
                self.playback_error = None;
            }

            match player.play(url) {
                Ok(()) => {
                    self.current_stream_url = Some(url.to_owned());
                    self.playback_error = None;
                }
                Err(err) => {
                    self.playback_error = Some(format!("Unable to start playback: {err}"));
                    self.current_stream_url = None;
                    player.stop();
                    return;
                }
            }

            if let Some(err) = player.take_error() {
                self.playback_error = Some(err);
                self.current_stream_url = None;
                player.stop();
            }
        }
    }

    #[cfg(feature = "inline-playback")]
    fn stop_stream(&mut self) {
        self.stop_stream_internal(true);
    }

    #[cfg(feature = "inline-playback")]
    fn stop_stream_internal(&mut self, clear_error: bool) {
        self.current_stream_url = None;
        self.last_frame_version = 0;
        self.player_texture = None;
        self.player_texture_size = None;
        self.player_texture_ready = false;
        if clear_error {
            self.playback_error = None;
        }

        if let Some(player) = self.player.as_mut() {
            player.stop();
            let _ = player.take_error();
        }
    }

    #[cfg(feature = "inline-playback")]
    fn render_player_section(&mut self, ui: &mut egui::Ui, _event: &LiveEvent) {
        ui.label(RichText::new("Live playback").strong());

        if self.player.is_none() {
            if let Some(msg) = &self.player_init_error {
                ui.colored_label(Color32::from_rgb(200, 80, 80), msg);
            } else {
                ui.label("Inline playback is not available.");
            }
            return;
        }

        if self.current_stream_url.is_none() {
            self.render_player_placeholder(ui, "No active stream");
            if let Some(err) = &self.playback_error {
                ui.add_space(4.0);
                ui.colored_label(Color32::from_rgb(200, 80, 80), err);
            }
            return;
        }

        self.update_video_texture(ui.ctx());
        self.render_player_surface(ui);

        if let Some(err) = &self.playback_error {
            ui.add_space(4.0);
            ui.colored_label(Color32::from_rgb(200, 80, 80), err);
        }
    }

    #[cfg(feature = "inline-playback")]
    fn render_player_surface(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width().max(200.0);

        if let Some(texture) = &self.player_texture {
            let [tw, th] = texture.size();
            if tw > 0 && th > 0 {
                let aspect = th as f32 / tw as f32;
                let height = (width * aspect).max(120.0);
                let image = egui::Image::new(texture).fit_to_exact_size(Vec2::new(width, height));
                ui.add(image);
                return;
            }
        }

        self.render_player_placeholder(ui, "Loading stream…");
    }

    #[cfg(feature = "inline-playback")]
    fn render_player_placeholder(&self, ui: &mut egui::Ui, message: &str) {
        let width = ui.available_width().max(200.0);
        let height = width * (9.0 / 16.0);
        ui.allocate_ui(Vec2::new(width, height.max(120.0)), |ui| {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.add(egui::Spinner::new().size(24.0));
                    ui.add_space(4.0);
                    ui.label(message);
                });
            });
        });
    }

    #[cfg(feature = "inline-playback")]
    fn update_video_texture(&mut self, egui_ctx: &egui::Context) {
        if let Some(player) = self.player.as_mut() {
            if let Some(frame) = player.latest_frame() {
                if frame.version != self.last_frame_version {
                    self.last_frame_version = frame.version;

                    let size = [frame.width, frame.height];
                    let needs_new = self.player_texture.is_none()
                        || self.player_texture_size != Some(size)
                        || !self.player_texture_ready;

                    if needs_new {
                        let image = ColorImage::from_rgba_unmultiplied(size, &frame.pixels);
                        let texture = load_texture_checked(
                            egui_ctx,
                            format!("livestream-frame-{}", frame.version),
                            image,
                            TextureOptions::LINEAR,
                        );
                        self.player_texture = Some(texture);
                        self.player_texture_ready = true;
                    } else if let Some(texture) = &mut self.player_texture {
                        let image = ColorImage::from_rgba_unmultiplied(size, &frame.pixels);
                        texture.set(image, TextureOptions::LINEAR);
                    }

                    self.player_texture_size = Some(size);
                }
            }
        }
    }

    fn render_event_card(
        &mut self,
        ctx: &mut AppContext<'_>,
        ui: &mut egui::Ui,
        event: &LiveEvent,
    ) {
        let is_selected = self
            .selected_key
            .as_ref()
            .map(|key| key == &event.key)
            .unwrap_or(false);

        let fill = if is_selected {
            ui.visuals().selection.bg_fill
        } else {
            ui.visuals().faint_bg_color
        };

        let frame = egui::Frame::group(ui.style()).inner_margin(12.0).fill(fill);

        let inner = frame.show(ui, |ui| {
            ui.vertical(|ui| {
                if is_selected {
                    #[cfg(feature = "inline-playback")]
                    {
                        self.sync_selected_stream(event);
                        if event.streaming_url.is_some() {
                            self.render_player_section(ui, event);
                            ui.add_space(8.0);
                        }
                    }
                }

                self.render_event_header(ctx, ui, event);
                ui.add_space(8.0);
                self.render_event_body(ctx, ui, event);
            });
        });

        let response = inner.response.interact(Sense::click());

        if response.clicked() {
            self.selected_key = Some(event.key.clone());
            #[cfg(feature = "inline-playback")]
            self.sync_selected_stream(event);
        }
    }

    fn render_event_header(
        &mut self,
        ctx: &mut AppContext<'_>,
        ui: &mut egui::Ui,
        event: &LiveEvent,
    ) {
        ui.horizontal(|ui| {
            if let Some(streaming) = &event.streaming_url {
                if ui
                    .small_button("Open external")
                    .on_hover_text("Open the livestream in your default player")
                    .clicked()
                {
                    if let Err(err) = open_external(streaming) {
                        self.error_message = Some(format!("Unable to open stream: {err}"));
                    }
                }
                ui.add_space(8.0);
            }

            if let Some(title) = &event.title {
                ui.heading(title);
            } else {
                ui.heading("Untitled Livestream");
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let status_color = event.status.color();
                let status_label = event.status.label();
                ui.label(RichText::new(status_label).color(status_color).strong());
            });
        });

        ui.horizontal(|ui| {
            ui.label(format!(
                "Host: {} ({})",
                event.author_display,
                short_hex(&event.author_hex)
            ));

            let since = time_ago_since(ctx.i18n, event.updated_at);
            ui.label(format!("Updated {}", since));
        });
    }

    fn render_event_body(&self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui, event: &LiveEvent) {
        ui.horizontal(|ui| {
            if let Some(image_url) = event.image_url.as_deref() {
                self.render_image(ui, ctx, image_url);
                ui.add_space(12.0);
            }

            ui.vertical(|ui| {
                if let Some(summary) = &event.summary {
                    ui.label(summary);
                    ui.add_space(8.0);
                }

                if event.starts.is_some() || event.ends.is_some() {
                    let start_label = event.starts.map(|ts| time_format(ctx.i18n, ts));
                    let end_label = event.ends.map(|ts| time_format(ctx.i18n, ts));
                    ui.horizontal(|ui| {
                        if let Some(start) = start_label {
                            ui.label(format!("Starts: {start}"));
                        }
                        if let Some(end) = end_label {
                            ui.label(format!("Ends: {end}"));
                        }
                    });
                }

                if event.current_participants.is_some() || event.total_participants.is_some() {
                    let current = event
                        .current_participants
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".into());
                    let total = event
                        .total_participants
                        .map(|v| v.to_string())
                        .unwrap_or_else(|| "-".into());

                    ui.label(format!("Participants: {current} / {total}"));
                }

                if !event.participants.is_empty() {
                    ui.add_space(8.0);
                    ui.label("Participants:");
                    for participant in &event.participants {
                        let role = participant.role.as_deref().unwrap_or("Participant");
                        ui.label(format!("- {} ({})", participant.display_name, role));
                    }
                }

                if let Some(streaming) = &event.streaming_url {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label("Stream:");
                        ui.hyperlink(streaming);
                    });
                }

                if let Some(recording) = &event.recording_url {
                    ui.horizontal(|ui| {
                        ui.label("Recording:");
                        ui.hyperlink(recording);
                    });
                }

                if !event.hashtags.is_empty() {
                    ui.add_space(4.0);
                    ui.horizontal_wrapped(|ui| {
                        for tag in &event.hashtags {
                            ui.label(format!("#{}", tag));
                        }
                    });
                }
            });
        });
    }

    fn render_image(&self, ui: &mut egui::Ui, ctx: &mut AppContext<'_>, url: &str) {
        let max_width = 160.0;

        let texture = ctx.img_cache.latest_texture(
            ui,
            url,
            ImageType::Content(None),
            AnimationMode::NoAnimation,
        );

        if let Some(texture) = texture {
            let [w, h] = texture.size();
            if w > 0 && h > 0 {
                let width = max_width;
                let height = width * (h as f32 / w as f32).max(0.1);
                let image = egui::Image::new(&texture)
                    .max_size(Vec2::new(width, height))
                    .corner_radius(egui::CornerRadius::same(6));
                ui.add(image);
                return;
            }
        }

        let placeholder = Vec2::new(max_width, max_width * 0.6);
        ui.allocate_ui(placeholder, |ui| {
            ui.centered_and_justified(|ui| {
                ui.label("No preview");
            });
        });
    }
}

impl App for LivestreamsApp {
    fn update(&mut self, ctx: &mut AppContext<'_>, ui: &mut egui::Ui) -> AppResponse {
        let now = ui.input(|i| i.time);
        self.update_streams(ctx, now);

        ui.vertical(|ui| {
            self.render_controls(ui);
            ui.add_space(8.0);

            if let Some(err) = &self.error_message {
                ui.colored_label(Color32::from_rgb(200, 80, 80), err);
                ui.add_space(8.0);
            }

            self.render_streams(ctx, ui);
        });

        AppResponse::none()
    }
}

#[derive(Clone, Debug)]
struct LiveEvent {
    key: LiveKey,
    title: Option<String>,
    summary: Option<String>,
    image_url: Option<String>,
    streaming_url: Option<String>,
    recording_url: Option<String>,
    status: LiveStatus,
    starts: Option<u64>,
    ends: Option<u64>,
    current_participants: Option<u64>,
    total_participants: Option<u64>,
    participants: Vec<Participant>,
    hashtags: Vec<String>,
    updated_at: u64,
    author_hex: String,
    author_display: String,
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct LiveKey {
    pubkey: [u8; 32],
    identifier: String,
}

#[derive(Clone, Debug)]
struct Participant {
    display_name: String,
    role: Option<String>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum LiveStatus {
    Live,
    Planned,
    Ended,
    Other,
}

impl LiveStatus {
    fn from_tag(tag: Option<&str>) -> Self {
        match tag.map(|s| s.to_ascii_lowercase()) {
            Some(ref s) if s == "live" => Self::Live,
            Some(ref s) if s == "ended" => Self::Ended,
            Some(ref s) if s == "planned" => Self::Planned,
            _ => Self::Other,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Live => "Live",
            Self::Planned => "Planned",
            Self::Ended => "Ended",
            Self::Other => "Unknown",
        }
    }

    fn color(&self) -> Color32 {
        match self {
            Self::Live => Color32::from_rgb(0x3b, 0xc4, 0x5a),
            Self::Planned => Color32::from_rgb(0xf2, 0xb8, 0x2b),
            Self::Ended => Color32::from_rgb(0x9e, 0x9e, 0x9e),
            Self::Other => Color32::from_rgb(0x60, 0x7d, 0x8b),
        }
    }

    fn sort_key(&self) -> u8 {
        match self {
            Self::Live => 0,
            Self::Planned => 1,
            Self::Ended => 2,
            Self::Other => 3,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum StatusFilter {
    All,
    Live,
    Planned,
    Ended,
}

impl StatusFilter {
    const fn all() -> [StatusFilter; 4] {
        [
            StatusFilter::All,
            StatusFilter::Live,
            StatusFilter::Planned,
            StatusFilter::Ended,
        ]
    }

    fn label(&self) -> &'static str {
        match self {
            StatusFilter::All => "All",
            StatusFilter::Live => "Live",
            StatusFilter::Planned => "Planned",
            StatusFilter::Ended => "Ended",
        }
    }

    fn matches(&self, status: &LiveStatus) -> bool {
        match self {
            StatusFilter::All => true,
            StatusFilter::Live => matches!(status, LiveStatus::Live),
            StatusFilter::Planned => matches!(status, LiveStatus::Planned),
            StatusFilter::Ended => matches!(status, LiveStatus::Ended),
        }
    }
}

fn sort_streams(streams: &mut [LiveEvent]) {
    streams.sort_by(|a, b| {
        a.status
            .sort_key()
            .cmp(&b.status.sort_key())
            .then_with(|| match (&a.status, &b.status) {
                (LiveStatus::Live, LiveStatus::Live) => b.updated_at.cmp(&a.updated_at),
                (LiveStatus::Planned, LiveStatus::Planned) => match (a.starts, b.starts) {
                    (Some(s1), Some(s2)) => s1.cmp(&s2),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => a.updated_at.cmp(&b.updated_at),
                },
                _ => b.updated_at.cmp(&a.updated_at),
            })
    });
}

fn parse_u64(value: Option<&str>) -> Option<u64> {
    value
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
}

fn collect_tag_values(note: &Note<'_>, tag_name: &str) -> Vec<String> {
    let mut items = Vec::new();
    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        if tag.get_str(0) != Some(tag_name) {
            continue;
        }

        for idx in 1..tag.count() {
            if let Some(val) = tag.get_str(idx) {
                if !val.is_empty() {
                    items.push(val.to_owned());
                }
            }
        }
    }
    items
}

fn collect_participants(
    ctx: &mut AppContext<'_>,
    txn: &Transaction,
    note: &Note<'_>,
) -> Vec<Participant> {
    let mut participants = Vec::new();
    for tag in note.tags() {
        if tag.count() < 2 {
            continue;
        }

        if tag.get_str(0) != Some("p") {
            continue;
        }

        let pubkey_hex = match tag.get_str(1) {
            Some(pk) => pk,
            None => continue,
        };

        let display = match parse_pubkey_hex(pubkey_hex) {
            Some(pk_bytes) => {
                profile_display_name(ctx, txn, &pk_bytes).unwrap_or_else(|| short_hex(pubkey_hex))
            }
            None => short_hex(pubkey_hex),
        };

        let role = tag.get_str(3).map(str::to_owned);
        participants.push(Participant {
            display_name: display,
            role,
        });
    }

    participants
}

fn parse_pubkey_hex(hex_str: &str) -> Option<[u8; 32]> {
    <[u8; 32]>::from_hex(hex_str).ok()
}

fn profile_display_name(
    ctx: &mut AppContext<'_>,
    txn: &Transaction,
    pubkey: &[u8; 32],
) -> Option<String> {
    ctx.ndb
        .get_profile_by_pubkey(txn, pubkey)
        .ok()
        .map(|profile| get_display_name(Some(&profile)).name().to_owned())
}

fn short_hex(hex_str: &str) -> String {
    if hex_str.len() <= 12 {
        return hex_str.to_owned();
    }
    format!("{}…{}", &hex_str[..6], &hex_str[hex_str.len() - 6..])
}
