//! The HUD widget kit (`notedeck_cashflow::ui::hud`), drawn as one gallery:
//! a tooltip panel, an XP bar, a row of skill tiles, a time-series chart with
//! the HUD tooltip on hover, and a level-up toast.
//!
//! The behavioural tests run under a plain `cargo test`. The snapshots — the
//! gallery at rest and with each hoverable widget hovered, in the dark and
//! light themes — are `#[ignore]`d and run via `scripts/snapshot-test`
//! (lavapipe).

use egui::{Rect, Vec2, pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use notedeck_cashflow::ui::hud::{self, Grouped, Rarity, Toast, Toasts, XpProgress};
use notedeck_cashflow::ui::palette::Palette;
use notedeck_ui::charts::{Series, TimeSeries, TimeSeriesStyle, area_chart};

/// The gallery's window.
const SIZE: Vec2 = Vec2::new(760.0, 640.0);

/// The XP bar's skill, a project skill that no tile shares, so its label is
/// unique in the accessibility tree.
const BAR_SKILL: &str = "Damus";

/// The skills on the tiles: icon, name, level.
const TILES: [(&str, &str, u8); 6] = [
    ("💰", "Earning", 61),
    ("🐷", "Saving", 42),
    ("✂", "Frugality", 27),
    ("🪙", "Stacking", 55),
    ("📒", "Bookkeeping", 33),
    ("🛠", "Projects", 99),
];

/// A year of net cashflow, in dollars, with one bad month.
const NET: [f64; 12] = [
    1_200.0, 2_400.0, 1_800.0, -900.0, 3_100.0, 2_700.0, 3_900.0, 4_200.0, 2_100.0, 5_000.0,
    4_400.0, 6_100.0,
];

/// Month names for the chart's buckets.
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// What the gallery holds between frames.
struct GalleryState {
    /// The palette, refreshed from the host theme every frame.
    palette: Palette,
    /// The toast queue under test.
    toasts: Toasts,
    /// Where the chart's plot was drawn last frame, so a test can hover it.
    plot_rect: Option<Rect>,
    /// Where the toast was drawn last frame, if it was.
    toast_rect: Option<Rect>,
}

/// Progress for a skill at `level`, on a made-up curve: the HUD draws the
/// numbers it is handed, so any monotonic curve will do here.
fn progress(level: u8) -> XpProgress {
    let at = |level: u64| level * level * 1_300;
    let level_xp = at(level as u64);
    let next_level_xp = if level >= hud::MAX_LEVEL {
        level_xp
    } else {
        at(level as u64 + 1)
    };
    XpProgress {
        level,
        xp: level_xp + (next_level_xp - level_xp) * 3 / 5,
        level_xp,
        next_level_xp,
        month_xp: 4_210,
        rested_xp: (next_level_xp - level_xp) / 8,
    }
}

/// One gallery frame, in the `Ui` notedeck's chrome hands an app.
fn render_gallery(ui: &mut egui::Ui, state: &mut GalleryState) {
    notedeck::test_harness::app_frame(ui, |ui| {
        state.palette.refresh(ui.ctx());
        let palette = &state.palette;
        egui::Frame::new()
            .fill(palette.background)
            .inner_margin(24)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                // Clear of the toast, which rests at the top of the window.
                ui.add_space(90.0);
                ui.horizontal_top(|ui| {
                    hud::tooltip_frame(&palette.hud).show(ui, |ui| {
                        hud::tooltip_ui(ui, palette, |t| {
                            t.title("Saving — April");
                            t.text("Net cashflow this month");
                            t.value("Common", "CAD 12", Rarity::Common);
                            t.value("Uncommon", "CAD 120", Rarity::Uncommon);
                            t.value("Rare", "CAD 1,200", Rarity::Rare);
                            t.value("Epic", "CAD 12,000", Rarity::Epic);
                            t.value("Legendary", "CAD 120,000", Rarity::Legendary);
                            t.separator();
                            t.note("Saving +4,210 XP: net CAD 210.50 × 20");
                        });
                    });
                    ui.add_space(16.0);
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width());
                        let data = TimeSeries {
                            series: &[Series {
                                label: "Net",
                                values: &NET,
                                color: palette.income,
                            }],
                            bucket_label: &|i| MONTHS[i].to_owned(),
                            format_value: &|v| format!("${v:.0}"),
                        };
                        let style = TimeSeriesStyle {
                            negative_color: Some(palette.expense),
                            tooltip_frame: Some(hud::tooltip_frame(&palette.hud)),
                            ..TimeSeriesStyle::default()
                        };
                        let size = egui::vec2(ui.available_width(), 200.0);
                        let chart = area_chart(ui, size, &data, &style, |ui, hover| {
                            hud::tooltip_ui(ui, palette, |t| {
                                t.title(MONTHS[hover.bucket]);
                                for point in hover.values() {
                                    let Some(v) = point.value else { continue };
                                    let rarity =
                                        Rarity::tier(v, &[1_000.0, 3_000.0, 5_000.0, 6_000.0]);
                                    t.value(point.series.label, format_args!("${v:.0}"), rarity);
                                }
                            });
                        });
                        state.plot_rect = Some(chart.plot_rect);
                    });
                });

                ui.add_space(24.0);
                hud::xp_bar(ui, palette, BAR_SKILL, &progress(48));

                ui.add_space(16.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                    for (icon, name, level) in TILES {
                        hud::skill_tile(ui, palette, icon, name, &progress(level));
                    }
                });
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!(
                        "Total level {}",
                        Grouped(TILES.iter().map(|t| t.2 as u64).sum())
                    ))
                    .color(palette.text_muted),
                );

                state.toast_rect = state.toasts.show(ui, palette);
            });
    });
}

/// A gallery in `theme` with `toasts` queued, settled.
fn gallery(theme: egui::Theme, toasts: Toasts, renderer: bool) -> Harness<'static, GalleryState> {
    let state = GalleryState {
        palette: Palette::default(),
        toasts,
        plot_rect: None,
        toast_rect: None,
    };
    let mut builder = notedeck::test_harness::app_builder()
        .with_size(SIZE)
        .with_theme(theme);
    if renderer {
        builder = builder.renderer(notedeck::software_renderer());
    }
    let mut harness = builder.build_ui_state(render_gallery, state);
    harness.run();
    harness
}

/// A queue holding one level-up toast.
fn level_up() -> Toasts {
    let mut toasts = Toasts::new();
    toasts.push(Toast::level_up("Saving", 42));
    toasts
}

/// Hover the middle of `skill`'s tile or the XP bar and let its tooltip open.
fn hover_label(harness: &mut Harness<'static, GalleryState>, label: &str) {
    harness.get_by_label(label).hover();
    harness.run_steps(4);
}

/// Hover the chart's plot at bucket `bucket` of 12 and let its tooltip open.
fn hover_chart(harness: &mut Harness<'static, GalleryState>, bucket: usize) {
    let plot = harness.state().plot_rect.expect("chart drawn");
    let x = plot.left() + plot.width() * (bucket as f32 + 0.5) / NET.len() as f32;
    harness.hover_at(pos2(x, plot.center().y));
    harness.run_steps(4);
}

/// Hovering the XP bar opens its HUD tooltip with the XP to the next level,
/// this month's gain and the rested bonus.
#[test]
fn xp_bar_tooltip_opens_on_hover() {
    let mut harness = gallery(egui::Theme::Dark, Toasts::new(), false);
    assert!(harness.query_by_label("To next level").is_none());

    hover_label(&mut harness, BAR_SKILL);

    harness.get_by_label("Damus — level 48");
    harness.get_by_label("To next level");
    harness.get_by_label("Rested");
}

/// Hovering a skill tile opens its tooltip, showing its level out of 99.
#[test]
fn skill_tile_tooltip_opens_on_hover() {
    let mut harness = gallery(egui::Theme::Dark, Toasts::new(), false);

    hover_label(&mut harness, "Saving");

    harness.get_by_label("42/99");
    harness.get_by_label("This month");
}

/// A maxed skill's tooltip says so instead of counting XP to a next level.
#[test]
fn max_level_tile_has_no_next_level() {
    let mut harness = gallery(egui::Theme::Dark, Toasts::new(), false);

    hover_label(&mut harness, "Projects");

    harness.get_by_label("Maximum level");
    assert!(harness.query_by_label("To next level").is_none());
}

/// The chart takes the HUD tooltip's frame and contents: hovering a month
/// shows its title and value.
#[test]
fn chart_hover_shows_the_hud_tooltip() {
    let mut harness = gallery(egui::Theme::Dark, Toasts::new(), false);

    hover_chart(&mut harness, 3);

    harness.get_by_label("Apr");
    harness.get_by_label("$-900");
}

/// A toast slides in and then holds without repainting every frame: the
/// harness's `run` settles (it panics if a frame keeps asking for an
/// immediate repaint), with the toast on screen.
#[test]
fn toast_holds_without_spinning() {
    let harness = gallery(egui::Theme::Dark, level_up(), false);

    harness.get_by_label("You've advanced a Saving level! You are now level 42.");
    assert!(harness.state().toast_rect.is_some());
}

/// Once its hold and fade are over, a toast leaves, and the next in the
/// queue takes its place.
#[test]
fn toasts_show_one_after_another() {
    let mut toasts = level_up();
    toasts.push(Toast::achievement("Inbox zero", 10));
    let mut harness = gallery(egui::Theme::Dark, toasts, false);
    harness.get_by_label("Level up!");

    // Each step advances the clock by the harness's step_dt (1/4 s).
    harness.run_steps(24);

    assert!(harness.query_by_label("Level up!").is_none());
    harness.get_by_label("Achievement earned: Inbox zero");
}

/// Snapshot `name` of a gallery in `theme`, after `prepare`.
fn snapshot(
    theme: egui::Theme,
    name: &str,
    prepare: impl FnOnce(&mut Harness<'static, GalleryState>),
) {
    let mut harness = gallery(theme, level_up(), true);
    prepare(&mut harness);
    harness.snapshot(name);
}

/// The gallery at rest, with the level-up toast on screen.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_hud_gallery_dark() {
    snapshot(egui::Theme::Dark, "hud_gallery_dark", |_| {});
}

/// [`snapshot_hud_gallery_dark`] in the light theme.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_hud_gallery_light() {
    snapshot(egui::Theme::Light, "hud_gallery_light", |_| {});
}

/// The XP bar hovered: its tooltip, with the rested bonus.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_hud_xp_bar_hover_dark() {
    snapshot(egui::Theme::Dark, "hud_xp_bar_hover_dark", |h| {
        hover_label(h, BAR_SKILL)
    });
}

/// [`snapshot_hud_xp_bar_hover_dark`] in the light theme.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_hud_xp_bar_hover_light() {
    snapshot(egui::Theme::Light, "hud_xp_bar_hover_light", |h| {
        hover_label(h, BAR_SKILL)
    });
}

/// A skill tile hovered: its border lights up and its tooltip opens.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_hud_skill_tile_hover_dark() {
    snapshot(egui::Theme::Dark, "hud_skill_tile_hover_dark", |h| {
        hover_label(h, "Saving")
    });
}

/// [`snapshot_hud_skill_tile_hover_dark`] in the light theme.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_hud_skill_tile_hover_light() {
    snapshot(egui::Theme::Light, "hud_skill_tile_hover_light", |h| {
        hover_label(h, "Saving")
    });
}

/// The chart hovered on its best month: the HUD tooltip, legendary orange.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_hud_chart_hover_dark() {
    snapshot(egui::Theme::Dark, "hud_chart_hover_dark", |h| {
        hover_chart(h, 11)
    });
}

/// [`snapshot_hud_chart_hover_dark`] in the light theme.
#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_hud_chart_hover_light() {
    snapshot(egui::Theme::Light, "hud_chart_hover_light", |h| {
        hover_chart(h, 11)
    });
}
