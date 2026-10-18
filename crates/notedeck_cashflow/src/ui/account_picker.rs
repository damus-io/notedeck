//! The account picker: a search box over the chart of accounts, recent
//! picks first, that can also make a new account by typing its path.
//!
//! It draws inline, under whatever opened it, rather than as a popup, so a
//! phone shows it full width and a snapshot shows it where it is. Nothing
//! is collected per frame: the matches are drawn as the account list is
//! walked, and a name is copied only when one is picked.

use egui::RichText;

use super::palette::Palette;

/// How many accounts the picker lists at once.
const SHOWN: usize = 8;

/// How many recent picks it remembers.
const RECENT: usize = 6;

/// What the picker keeps while it is open.
#[derive(Default)]
pub struct AccountPicker {
    /// What has been typed.
    query: String,
    /// Whether the search box still needs focus: set when it opens.
    focus: bool,
}

impl AccountPicker {
    /// Clear the search and focus the box: the picker was just opened.
    pub fn open(&mut self) {
        self.query.clear();
        self.focus = true;
    }
}

/// The accounts the picker chooses between.
#[derive(Clone, Copy)]
pub struct Choices<'a> {
    /// Every account, sorted.
    pub accounts: &'a [String],
    /// Recent picks, newest first.
    pub recent: &'a [String],
    /// Whether typing a path that isn't an account offers to make it.
    pub allow_new: bool,
}

/// What a frame of the picker did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Picked {
    /// The account chosen, if one was.
    pub account: Option<String>,
    /// Whether it was dismissed without a choice.
    pub cancelled: bool,
}

/// Remember `account` as the newest of `recent`.
pub fn remember(recent: &mut Vec<String>, account: &str) {
    recent.retain(|a| a != account);
    recent.insert(0, account.to_owned());
    recent.truncate(RECENT);
}

/// Whether `hay` contains `needle`, ignoring ASCII case, folding neither.
fn contains(hay: &str, needle: &str) -> bool {
    let needle = needle.as_bytes();
    needle.is_empty()
        || hay
            .as_bytes()
            .windows(needle.len())
            .any(|w| w.eq_ignore_ascii_case(needle))
}

/// Draw the picker for `choices` into `ui`.
pub fn show(
    ui: &mut egui::Ui,
    palette: &Palette,
    picker: &mut AccountPicker,
    choices: Choices<'_>,
) -> Picked {
    let mut picked = Picked::default();
    egui::Frame::new()
        .fill(palette.surface)
        .stroke(egui::Stroke::new(1.0, palette.border))
        .corner_radius(6.0)
        .inner_margin(8.0)
        .show(ui, |ui| {
            let hint = if choices.allow_new {
                "Search, or type a new account path"
            } else {
                "Search accounts"
            };
            let edit = ui.add(
                egui::TextEdit::singleline(&mut picker.query)
                    .hint_text(hint)
                    .desired_width(f32::INFINITY),
            );
            if std::mem::take(&mut picker.focus) {
                edit.request_focus();
            }
            let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                picked.cancelled = true;
            }
            let query = picker.query.trim();

            let mut first: Option<&str> = None;
            let mut shown = 0;
            let recent = choices.recent.iter().filter(|a| contains(a, query));
            let rest = choices
                .accounts
                .iter()
                .filter(|a| contains(a, query) && !choices.recent.contains(a));
            for (n, account) in recent.map(|a| (true, a)).chain(rest.map(|a| (false, a))) {
                if shown == SHOWN {
                    break;
                }
                shown += 1;
                first.get_or_insert(account);
                let row = ui.horizontal(|ui| {
                    let clicked = ui
                        .add(egui::Button::selectable(
                            false,
                            RichText::new(account.as_str()).color(palette.text),
                        ))
                        .clicked();
                    if n {
                        ui.label(RichText::new("recent").small().color(palette.text_muted));
                    }
                    clicked
                });
                if row.inner {
                    picked.account = Some(account.clone());
                }
            }
            let exists = choices
                .accounts
                .binary_search_by(|a| a.as_str().cmp(query))
                .is_ok();
            let new = choices.allow_new && !query.is_empty() && !exists;
            if new {
                let create = ui.add(egui::Button::selectable(
                    false,
                    RichText::new(format!("+ New account {query}")).color(palette.accent),
                ));
                if create.clicked() {
                    picked.account = Some(query.to_owned());
                }
            } else if shown == 0 {
                ui.label(RichText::new("No account matches").color(palette.text_muted));
            }
            if enter && picked.account.is_none() {
                picked.account = first
                    .map(str::to_owned)
                    .or_else(|| new.then(|| query.to_owned()));
            }
            if ui
                .add(egui::Button::new(RichText::new("Cancel").color(palette.text_muted)).small())
                .clicked()
            {
                picked.cancelled = true;
            }
        });
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pick moves to the front, and the list stays short.
    #[test]
    fn remember_keeps_the_newest_first() {
        let mut recent = Vec::new();
        for account in ["a", "b", "c", "a", "d", "e", "f", "g"] {
            remember(&mut recent, account);
        }
        assert_eq!(recent, ["g", "f", "e", "d", "a", "c"]);
    }

    #[test]
    fn contains_ignores_ascii_case() {
        assert!(contains("expenses:Food", "FOOD"));
        assert!(contains("anything", ""));
        assert!(!contains("expenses", "income"));
    }
}
