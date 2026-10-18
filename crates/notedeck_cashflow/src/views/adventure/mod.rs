//! The Adventure view: skills levelled by the books, drawn in the game HUD
//! ([`crate::ui::hud`]).
//!
//! - **Skills**: a skills-tab grid of [`hud::skill_tile`]s, three across
//!   (fewer on a phone too narrow for three), the fixed skills then one
//!   per project, with a skill not started yet locked and hinting how to
//!   start it; the total level beneath.
//! - **Character**: the total level, the streak, the Well Rested buff or
//!   the Overspent debuff, what each skill earned this month, and the XP
//!   bar of the skill nearest its next level.
//! - **Quests**: this week's, this month's and the epic quests (the
//!   Headway goals) with their progress. An epic quest's title opens its
//!   card, as the Goals view's do.
//! - **Achievements**: the unlocked ones dated, then the locked ones
//!   greyed out with what reaching them takes, and the points.
//! - **XP log**: the recent entries, corrections first.
//!
//! Every number has a HUD tooltip saying where it came from. In the wide
//! layout the skills and the character sheet stand in a column on the
//! left and the rest on the right; on a phone they stack.
//!
//! The fold ([`model::report`]) runs only when the books, the goals, the
//! synced time zone or today change, like the Goals view's measuring;
//! [`AdventureState`] keeps it, and hands it to the next fold so a
//! correction is logged.

pub mod model;

use std::fmt::Display;

use cashflow::adventure::{QuestKind, XpTuning};
use cashflow::zone::Zone;
use egui::{Align, Color32, FontId, Layout, Margin, RichText, Sense, Stroke, Ui, vec2};
use ledgerdb::Date;

use super::goals::status_color;
use super::{GoalsCx, LedgerCx, ViewCx};
use crate::View;
use crate::ui::hud::{self, Rarity, SKILL_TILE_SIZE, show_tooltip};
use crate::ui::palette::Palette;
use model::{Inputs, QuestRow, Report};

/// Skill tiles per row, where they fit.
const COLUMNS: usize = 3;

/// The gap between skill tiles.
const TILE_GAP: f32 = 6.0;

/// A panel's inner margin.
const PANEL_MARGIN: i8 = 12;

/// The left column's width in the wide layout: three tiles and their gaps,
/// inside a panel.
const LEFT_WIDTH: f32 = COLUMNS as f32 * SKILL_TILE_SIZE.x
    + (COLUMNS - 1) as f32 * TILE_GAP
    + 2.0 * PANEL_MARGIN as f32;

/// The narrowest the wide layout's right column gets before the view
/// stacks.
const RIGHT_MIN: f32 = 380.0;

/// The gap between panels.
const GAP: f32 = 12.0;

/// A panel title's size.
const TITLE_SIZE: f32 = 16.0;

/// A quest's progress bar's height.
const QUEST_BAR_HEIGHT: f32 = 6.0;

/// The XP log's month column.
const LOG_MONTH_WIDTH: f32 = 64.0;

/// The XP log's skill column.
const LOG_SKILL_WIDTH: f32 = 96.0;

/// The XP log's delta column.
const LOG_DELTA_WIDTH: f32 = 64.0;

/// The Adventure view's state: the fold, cached. The app's toasts read it
/// too ([`crate::news`]), and fold it while another view shows once the
/// Adventure has been opened.
#[derive(Default)]
pub(crate) struct AdventureState {
    /// What [`Self::report`] was folded from; a frame with a different key
    /// folds again.
    key: Option<Key>,
    /// The fold written out, once there are books with a home commodity.
    report: Option<Report>,
    /// Bumped every fold, so a reader knows [`Self::report`] changed.
    generation: u64,
}

/// What the fold depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Key {
    books: u64,
    goals: u64,
    today: Date,
    zone: Zone,
}

impl AdventureState {
    /// Fold again if the books, the goals, the zone or today moved, at the
    /// rates `tuning` sets.
    #[profiling::function]
    pub(crate) fn refresh(
        &mut self,
        ledger: LedgerCx<'_>,
        goals: GoalsCx<'_>,
        projects: &cashflow::projects::Projects,
        zone: Zone,
        today: Date,
        tuning: &XpTuning,
    ) {
        let key = Key {
            books: ledger.generation,
            goals: goals.generation,
            today,
            zone,
        };
        if self.key == Some(key) {
            return;
        }
        self.key = Some(key);
        self.generation += 1;
        let previous = self.report.take().map(|r| r.adventure);
        self.report = model::report(
            Inputs {
                books: ledger.books,
                derive: &ledger.triage.inputs,
                projects,
                zone,
                today,
                goals: goals.goals,
                tuning,
            },
            previous.as_ref(),
        );
    }

    /// The fold written out, if there are books with a home commodity.
    pub(crate) fn report(&self) -> Option<&Report> {
        self.report.as_ref()
    }

    /// Bumped every time [`Self::report`] is folded again.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
}

/// Draw the Adventure view.
#[profiling::function]
pub(crate) fn ui(cx: &mut ViewCx<'_>, ui: &mut Ui) {
    let palette = cx.palette;
    ui.label(
        RichText::new(View::Adventure.label())
            .heading()
            .color(palette.text),
    );
    ui.add_space(GAP);

    let Some(ledger) = cx.ledger else {
        muted(ui, palette, "No journal loaded yet.");
        return;
    };
    let zone = cx.journal.as_ref().map_or(Zone::Local, |j| j.zone());
    let state = &mut cx.states.adventure;
    state.refresh(ledger, cx.goals, cx.projects, zone, cx.today, cx.tuning);
    let Some(report) = &state.report else {
        muted(
            ui,
            palette,
            "The books have nothing in their home commodity to count XP in.",
        );
        return;
    };

    let narrow = notedeck::ui::is_narrow(ui.ctx());
    let mut open = None;
    egui::ScrollArea::vertical()
        .id_salt("cashflow_adventure")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let wide = !narrow && ui.available_width() >= LEFT_WIDTH + GAP + RIGHT_MIN;
            if !wide {
                ui.spacing_mut().item_spacing.y = GAP;
                skills_ui(ui, palette, report);
                sheet_ui(ui, palette, report);
                open = quests_ui(ui, palette, report);
                achievements_ui(ui, palette, report);
                log_ui(ui, palette, report);
                return;
            }
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    vec2(LEFT_WIDTH, 0.0),
                    Layout::top_down(Align::Min),
                    |ui| {
                        ui.set_width(LEFT_WIDTH);
                        ui.spacing_mut().item_spacing.y = GAP;
                        skills_ui(ui, palette, report);
                        sheet_ui(ui, palette, report);
                    },
                );
                ui.add_space(GAP);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = GAP;
                    open = quests_ui(ui, palette, report);
                    achievements_ui(ui, palette, report);
                    log_ui(ui, palette, report);
                });
            });
        });
    if let Some(card) = open {
        cx.open_card = Some(card.to_owned());
    }
}

/// A HUD panel titled `title`, filling the width it is given.
fn panel<R>(
    ui: &mut Ui,
    palette: &Palette,
    title: &str,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> R {
    let hud = &palette.hud;
    egui::Frame::new()
        .fill(hud.panel)
        .stroke(Stroke::new(notedeck::tokens::STROKE_THIN, hud.border))
        .corner_radius(notedeck::tokens::RADIUS_SM)
        .inner_margin(Margin::same(PANEL_MARGIN))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing = vec2(8.0, 4.0);
            ui.label(
                RichText::new(title)
                    .font(FontId::new(TITLE_SIZE, palette.faces.bold.clone()))
                    .color(hud.title),
            );
            ui.add_space(4.0);
            add_contents(ui)
        })
        .inner
}

/// The skills grid and the total level beneath it.
fn skills_ui(ui: &mut Ui, palette: &Palette, report: &Report) {
    panel(ui, palette, "Skills", |ui| {
        ui.spacing_mut().item_spacing = vec2(TILE_GAP, TILE_GAP);
        // Three across, or as many as fit a phone.
        let fit = ((ui.available_width() + TILE_GAP) / (SKILL_TILE_SIZE.x + TILE_GAP)) as usize;
        for row in report.skills.chunks(fit.clamp(1, COLUMNS)) {
            ui.horizontal(|ui| {
                for skill in row {
                    match skill.unlock {
                        Some(hint) => {
                            hud::locked_skill_tile(ui, palette, skill.icon, &skill.name, hint)
                        }
                        None => {
                            hud::skill_tile(ui, palette, skill.icon, &skill.name, &skill.progress)
                        }
                    };
                }
            });
        }
        ui.add_space(2.0);
        let sheet = &report.sheet;
        let total = stat_row(
            ui,
            palette,
            "Total level",
            sheet.total_level,
            palette.hud.title,
        );
        show_tooltip(&total, palette, |t| {
            t.title("Total level");
            t.value("Level", sheet.total_level, Rarity::Common);
            t.note(&sheet.total_level_detail);
        });
    });
}

/// The character sheet.
fn sheet_ui(ui: &mut Ui, palette: &Palette, report: &Report) {
    let hud = &palette.hud;
    let sheet = &report.sheet;
    panel(ui, palette, "Character", |ui| {
        let total = stat_row(ui, palette, "Total level", sheet.total_level, hud.title);
        show_tooltip(&total, palette, |t| {
            t.title("Total level");
            t.note(&sheet.total_level_detail);
        });
        let streak = stat_row(ui, palette, "Streak", &sheet.streak_text, hud.text);
        show_tooltip(&streak, palette, |t| {
            t.title("Streak");
            t.value("Positive months", &sheet.streak_text, Rarity::Common);
            t.note(&sheet.streak_detail);
        });
        for buff in &sheet.buffs {
            let color = if buff.good {
                hud.uncommon
            } else {
                palette.expense
            };
            let row = ui
                .horizontal(|ui| {
                    ui.label(RichText::new(buff.icon).color(color));
                    ui.label(
                        RichText::new(buff.name)
                            .font(FontId::new(14.0, palette.faces.medium.clone()))
                            .color(color),
                    );
                    ui.label(RichText::new(&buff.text).small().color(hud.text_muted));
                })
                .response;
            show_tooltip(&row, palette, |t| {
                t.title(buff.name);
                t.line(
                    &buff.text,
                    if buff.good {
                        Rarity::Uncommon
                    } else {
                        Rarity::Common
                    },
                );
                t.note(&buff.detail);
            });
        }

        ui.add_space(4.0);
        caption(ui, palette, "This month");
        if sheet.gains.is_empty() {
            ui.label(RichText::new("No XP yet this month.").color(hud.text_muted));
        }
        for &at in &sheet.gains {
            let Some(skill) = report.skills.get(at) else {
                continue;
            };
            let row = stat_row(ui, palette, &skill.name, &skill.gain_text, hud.uncommon);
            show_tooltip(&row, palette, |t| {
                t.title(format_args!("{} {}", skill.name, skill.gain_text));
                for reason in &skill.gain_reasons {
                    t.text(reason);
                }
            });
        }

        let Some(skill) = sheet.closest.and_then(|at| report.skills.get(at)) else {
            return;
        };
        ui.add_space(4.0);
        caption(ui, palette, "Closest to a level");
        ui.label(
            RichText::new(&skill.name)
                .font(FontId::new(14.0, palette.faces.medium.clone()))
                .color(hud.text),
        );
        hud::xp_bar(ui, palette, &skill.name, &skill.progress);
    });
}

/// The quest log. Returns the card of an epic quest whose title was
/// clicked.
fn quests_ui<'r>(ui: &mut Ui, palette: &Palette, report: &'r Report) -> Option<&'r str> {
    let mut open = None;
    panel(ui, palette, "Quests", |ui| {
        if report.quests.is_empty() {
            ui.label(RichText::new("No quests running.").color(palette.hud.text_muted));
            return;
        }
        let kinds = [
            (QuestKind::Weekly, "This week"),
            (QuestKind::Monthly, "This month"),
            (QuestKind::Epic, "Epic"),
        ];
        for (kind, name) in kinds {
            let mut quests = report.quests.iter().filter(|q| q.kind == kind).peekable();
            if quests.peek().is_none() {
                continue;
            }
            caption(ui, palette, name);
            for quest in quests {
                if quest_ui(ui, palette, quest) {
                    open = quest.card_ref.as_deref();
                }
                ui.add_space(4.0);
            }
        }
    });
    open
}

/// One quest: its title (a link, for an epic quest), its reward, a bar
/// and its progress. Returns whether its title was clicked.
fn quest_ui(ui: &mut Ui, palette: &Palette, quest: &QuestRow) -> bool {
    let hud = &palette.hud;
    let mut clicked = false;
    ui.horizontal(|ui| {
        if quest.done {
            ui.label(RichText::new("✔").color(hud.uncommon));
        }
        let title = RichText::new(&quest.title).color(hud.text);
        if quest.card_ref.is_some() {
            let link = ui
                .add(egui::Button::new(title.underline()).frame(false))
                .on_hover_text("Open the card in Headway");
            if link.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            clicked = link.clicked();
        } else {
            ui.label(title);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(&quest.reward_text)
                    .small()
                    .color(hud.uncommon),
            );
        });
    });
    let color = match (quest.done, quest.status) {
        (true, _) => hud.uncommon,
        (false, Some(status)) => status_color(palette, status),
        (false, None) => hud.xp_fill,
    };
    let progress = ui
        .vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            bar(ui, palette, quest.fraction, color);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&quest.progress_text)
                        .small()
                        .color(hud.text_muted),
                );
                if let Some(status) = quest.status {
                    ui.label(RichText::new(status.name()).small().color(color));
                }
            });
        })
        .response;
    show_tooltip(&progress, palette, |t| {
        t.title(&quest.title);
        t.value("Progress", &quest.progress_text, Rarity::Common);
        if let Some(status) = quest.status {
            t.value("Goal", status.name(), Rarity::Common);
        }
        t.separator();
        t.value("Reward", &quest.reward_text, Rarity::Uncommon);
        if quest.done {
            t.note("Complete: the books met it.");
        }
    });
    clicked
}

/// The achievements, unlocked first, and the points.
fn achievements_ui(ui: &mut Ui, palette: &Palette, report: &Report) {
    let hud = &palette.hud;
    panel(ui, palette, "Achievements", |ui| {
        let points = stat_row(
            ui,
            palette,
            "Points",
            format_args!("{} / {}", report.points, report.total_points),
            hud.title,
        );
        show_tooltip(&points, palette, |t| {
            t.title("Achievement points");
            t.value(
                "Unlocked",
                format_args!("{} of {}", report.unlocked, report.achievements.len()),
                Rarity::Common,
            );
            t.value("Points", report.points, Rarity::Rare);
            t.note("Each achievement's points, summed over the unlocked ones.");
        });
        ui.add_space(4.0);
        for a in &report.achievements {
            let unlocked = a.unlocked.is_some();
            let (name_color, icon_color) = if unlocked {
                (hud.text, Color32::WHITE)
            } else {
                (hud.text_muted, Color32::WHITE.gamma_multiply(0.3))
            };
            let row = ui
                .horizontal(|ui| {
                    ui.label(RichText::new(a.icon).color(icon_color));
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new(a.name).strong().color(name_color));
                        ui.label(RichText::new(a.description).small().color(hud.text_muted));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let points = RichText::new(format!("{} pts", a.points)).small();
                        match &a.unlocked {
                            Some(date) => {
                                ui.label(points.color(hud.rare));
                                ui.label(RichText::new(date).small().color(hud.text_muted));
                            }
                            None => {
                                ui.label(points.color(hud.text_muted));
                            }
                        }
                    });
                })
                .response;
            show_tooltip(&row, palette, |t| {
                t.title(a.name);
                t.text(a.description);
                t.separator();
                t.value("Points", a.points, Rarity::Rare);
                match &a.unlocked {
                    Some(date) => t.value("Unlocked", date, Rarity::Uncommon),
                    None => t.line("Locked", Rarity::Common),
                };
            });
        }
    });
}

/// The recent XP log.
fn log_ui(ui: &mut Ui, palette: &Palette, report: &Report) {
    let hud = &palette.hud;
    panel(ui, palette, "XP log", |ui| {
        if report.log.is_empty() {
            ui.label(RichText::new("No XP earned yet.").color(hud.text_muted));
            return;
        }
        for entry in &report.log {
            let delta_color = if entry.adjustment {
                palette.expense
            } else {
                hud.uncommon
            };
            let row = ui
                .horizontal(|ui| {
                    cell(ui, LOG_MONTH_WIDTH, &entry.month, hud.text_muted);
                    cell(ui, LOG_SKILL_WIDTH, &entry.skill, hud.text);
                    cell(ui, LOG_DELTA_WIDTH, &entry.delta, delta_color);
                    ui.add(
                        egui::Label::new(
                            RichText::new(&entry.reason).small().color(hud.text_muted),
                        )
                        .truncate(),
                    );
                })
                .response;
            show_tooltip(&row, palette, |t| {
                t.title(format_args!("{} {} XP", entry.skill, entry.delta));
                t.text(&entry.reason);
                t.note(&entry.month);
                if entry.adjustment {
                    t.note("A correction: the books changed since this machine last folded them.");
                }
            });
        }
    });
}

/// A row with `label` on the left and `value` on the right. Returns the
/// row, for its tooltip.
fn stat_row(
    ui: &mut Ui,
    palette: &Palette,
    label: &str,
    value: impl Display,
    color: Color32,
) -> egui::Response {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).color(palette.hud.text));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(value.to_string())
                    .font(FontId::new(14.0, palette.faces.medium.clone()))
                    .color(color),
            );
        });
    })
    .response
}

/// A small caption over a group of rows.
fn caption(ui: &mut Ui, palette: &Palette, text: &str) {
    ui.label(
        RichText::new(text)
            .small()
            .strong()
            .color(palette.hud.text_muted),
    );
}

/// `text` in a column `width` wide, truncated to it.
fn cell(ui: &mut Ui, width: f32, text: &str, color: Color32) {
    let height = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(
        vec2(width, height),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_width(width);
            ui.add(egui::Label::new(RichText::new(text).color(color)).truncate());
        },
    );
}

/// A full-width bar, `fraction` of it filled in `color`.
fn bar(ui: &mut Ui, palette: &Palette, fraction: f32, color: Color32) {
    let size = vec2(ui.available_width(), QUEST_BAR_HEIGHT);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    let radius = QUEST_BAR_HEIGHT / 2.0;
    painter.rect_filled(rect, radius, palette.hud.track);
    if fraction > 0.0 {
        let mut filled = rect;
        filled.set_width((rect.width() * fraction).max(QUEST_BAR_HEIGHT));
        painter.rect_filled(filled, radius, color);
    }
}

/// A line of muted text: an empty state.
fn muted(ui: &mut Ui, palette: &Palette, text: &'static str) {
    ui.label(RichText::new(text).color(palette.text_muted));
}
