//! The Files card, part one: the session's Claude Code temp dir — the
//! agent's `scratchpad/` and the `tasks/*.output` logs its background tasks
//! write — newest first, each with Open, and Tail for an output log.
//!
//! The listing is read off the UI thread by
//! [`FilesState`](crate::inspector::FilesState) every few seconds while the
//! card is drawn; its strings are built into [`FileRows`] once a second at
//! most, since ages move with the clock. Only a local Claude Code session has
//! a temp dir this host can read.

use crate::backend::BackendType;
use crate::inspector::{
    FileRow, FileRows, FilesParts, FilesState, GroupRows, TempGroup, TempListing,
};
use crate::ui::DaveAction;
use notedeck::{tr, Localization};
use std::time::Instant;

use super::background::short_duration;
use super::{heading_ui, weak_ui, SessionFacts, MONO_SIZE, VALUE_SIZE};

/// The Files card's body. The session's cwd and CLI session id locate the
/// temp dir; without either there is none yet.
pub fn files_ui(
    state: &mut FilesState,
    facts: &SessionFacts<'_>,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) -> Option<DaveAction> {
    let details = facts.details;
    if facts.is_remote {
        let host = details.map_or("", |d| d.hostname.as_str());
        weak_ui(
            ui,
            &tr!(
                i18n,
                "Files live on {host}",
                "Dave session inspector files: a remote session's temp files are on the host running it",
                host = host
            ),
        );
        return None;
    }
    if facts.backend_type != BackendType::Claude {
        weak_ui(
            ui,
            &tr!(
                i18n,
                "Only Claude Code sessions keep a scratchpad",
                "Dave session inspector files: this session's backend has no Claude Code temp dir"
            ),
        );
        return None;
    }
    let cwd = details.and_then(|d| d.cwd.as_deref());
    let (Some(cwd), Some(cli_session_id)) = (cwd, facts.cli_session_id) else {
        weak_ui(
            ui,
            &tr!(
                i18n,
                "No Claude session yet",
                "Dave session inspector files: the CLI hasn't reported its session id, so its temp dir is unknown"
            ),
        );
        return None;
    };
    let now_unix = facts.now_unix;

    let listing_landed = state.poll(cwd, cli_session_id, Instant::now()).is_some();
    // Ages tick each second; the listing and tails refresh every few.
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_secs(1));
    if !listing_landed {
        weak_ui(ui, "…");
        return None;
    }

    let FilesParts {
        listing,
        generation,
        rows,
        tails,
        tail_open,
    } = state.parts_mut();
    let listing = listing?;
    let rows = rows.get((generation, now_unix), i18n, |_, i18n| {
        build_rows(listing, now_unix, i18n)
    });

    let mut action = None;
    ui.horizontal(|ui| {
        if open_button_ui(ui, &rows.open).clicked() {
            action = Some(DaveAction::OpenPath(listing.dir.clone()));
        }
        ui.add(
            egui::Label::new(
                egui::RichText::new(&rows.dir)
                    .font(egui::FontId::monospace(MONO_SIZE))
                    .weak(),
            )
            .truncate(),
        )
        // The path is cut to the card; the whole of it on hover.
        .on_hover_ui(|ui| {
            ui.label(egui::RichText::new(&rows.dir).font(egui::FontId::monospace(MONO_SIZE)));
        });
    });

    let groups = [
        GroupView {
            group: &listing.scratchpad,
            rows: &rows.scratchpad,
        },
        GroupView {
            group: &listing.tasks,
            rows: &rows.tasks,
        },
    ];
    let now = Instant::now();
    for view in groups {
        heading_ui(ui, &view.rows.heading);
        if view.group.files.is_empty() {
            weak_ui(ui, &rows.empty);
            continue;
        }
        for (file, row) in view.group.files.iter().zip(&view.rows.rows) {
            let is_output = !file.is_dir && file.name.ends_with(".output");
            let showing_tail = is_output && tail_open.as_deref() == Some(file.path.as_path());
            match file_row_ui(row, is_output, showing_tail, rows, ui) {
                Some(RowClick::Open) => action = Some(DaveAction::OpenPath(file.path.clone())),
                Some(RowClick::Tail) => FilesState::toggle_tail(tail_open, &file.path),
                None => {}
            }
            if showing_tail {
                if let Some(path) = file.path.to_str() {
                    super::background::tail_ui(ui, tails.tail(path, now));
                }
            }
        }
        if let Some(more) = &view.rows.more {
            weak_ui(ui, more);
        }
    }
    action
}

/// One group as the card draws it: its entries beside their strings.
struct GroupView<'a> {
    group: &'a TempGroup,
    rows: &'a GroupRows,
}

/// What a click on a file row asked for.
enum RowClick {
    Open,
    Tail,
}

/// One entry: its name in mono, then its size and age, Tail for an output
/// log, and Open, right-aligned.
fn file_row_ui(
    row: &FileRow,
    is_output: bool,
    showing_tail: bool,
    labels: &FileRows,
    ui: &mut egui::Ui,
) -> Option<RowClick> {
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if open_button_ui(ui, &labels.open).clicked() {
                clicked = Some(RowClick::Open);
            }
            if is_output {
                let label = if showing_tail {
                    &labels.hide
                } else {
                    &labels.tail
                };
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new(label).font(egui::FontId::proportional(VALUE_SIZE)),
                        )
                        .selected(showing_tail),
                    )
                    .clicked()
                {
                    clicked = Some(RowClick::Tail);
                }
            }
            weak_ui(ui, &row.detail);
            // The name takes what the rest leaves, from the left.
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&row.name)
                            .font(egui::FontId::monospace(MONO_SIZE + 1.0)),
                    )
                    .truncate(),
                );
            });
        });
    });
    clicked
}

/// A small Open button.
fn open_button_ui(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.add(egui::Button::new(
        egui::RichText::new(label).font(egui::FontId::proportional(VALUE_SIZE)),
    ))
}

/// The card's strings for `listing`, as of `now` (Unix seconds).
fn build_rows(listing: &TempListing, now: u64, i18n: &mut Localization) -> FileRows {
    FileRows {
        dir: format!("{}{}", listing.dir.display(), std::path::MAIN_SEPARATOR),
        scratchpad: group_rows("scratchpad", &listing.scratchpad, now, i18n),
        tasks: group_rows("tasks", &listing.tasks, now, i18n),
        empty: tr!(
            i18n,
            "empty",
            "Dave session inspector files: a temp-dir folder with nothing in it"
        ),
        open: tr!(
            i18n,
            "Open",
            "Dave session inspector files: open a file or folder with the system's default app"
        ),
        tail: tr!(
            i18n,
            "Tail",
            "Dave session inspector files: show the last lines of a background task's output log"
        ),
        hide: tr!(
            i18n,
            "Hide",
            "Dave session inspector files: hide an output log's last lines"
        ),
    }
}

/// A group's heading (`name/ · count`), rows, and the count it didn't list.
fn group_rows(name: &str, group: &TempGroup, now: u64, i18n: &mut Localization) -> GroupRows {
    let hidden = group.total.saturating_sub(group.files.len());
    GroupRows {
        // A directory name and a count, not prose: nothing to translate.
        heading: format!("{name}{} · {}", std::path::MAIN_SEPARATOR, group.total),
        rows: group.files.iter().map(|file| file_row(file, now)).collect(),
        more: (hidden > 0).then(|| {
            tr!(
                i18n,
                "{count} older not shown",
                "Dave session inspector files: entries past the card's cap, the oldest ones",
                count = hidden
            )
        }),
    }
}

/// One entry's name and "12.3 KB · 4m" line.
fn file_row(file: &crate::inspector::TempFile, now: u64) -> FileRow {
    let age = (file.modified > 0).then(|| short_duration(now.saturating_sub(file.modified), false));
    let (name, detail) = if file.is_dir {
        (
            format!("{}{}", file.name, std::path::MAIN_SEPARATOR),
            age.unwrap_or_default(),
        )
    } else {
        let size = format_size(file.size);
        let detail = match age {
            Some(age) => format!("{size} · {age}"),
            None => size,
        };
        (file.name.clone(), detail)
    };
    FileRow { name, detail }
}

/// A byte count, compactly: "512 B", "4.2 KB", "38 KB", "1.5 MB".
fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspector::TempFile;

    #[test]
    fn sizes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(4_300), "4.2 KB");
        assert_eq!(format_size(38_000), "37 KB");
        assert_eq!(format_size(1_572_864), "1.5 MB");
    }

    /// A file's row carries its size and age; a directory's, a trailing
    /// separator and its age alone.
    #[test]
    fn rows_read_size_and_age() {
        let file = TempFile {
            name: "notes.md".into(),
            path: "/x/notes.md".into(),
            size: 4_300,
            modified: 1_000,
            is_dir: false,
        };
        let row = file_row(&file, 1_000 + 4 * 60);
        assert_eq!(row.name, "notes.md");
        assert_eq!(row.detail, "4.2 KB · 4m");

        let dir = TempFile {
            name: "diffs".into(),
            is_dir: true,
            ..file
        };
        let row = file_row(&dir, 1_000 + 45);
        assert_eq!(row.name, format!("diffs{}", std::path::MAIN_SEPARATOR));
        assert_eq!(row.detail, "45s");
    }
}
