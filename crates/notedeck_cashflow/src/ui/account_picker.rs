//! The account picker: a search box over the chart of accounts, recent
//! picks first, that can also make a new account by typing its path.
//!
//! A caller can pin some accounts above the recent ones, each tagged with
//! the group it is in ([`Pinned`]): the detail's "Move to a project
//! account" lists the accounts in a project first, grouped by project. The
//! same picker chooses a project name, worded for one ([`Noun::Project`]).
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
#[derive(Clone, Copy, Default)]
pub struct Choices<'a> {
    /// Every account, sorted, less any of [`Self::pinned`].
    pub accounts: &'a [String],
    /// Recent picks, newest first.
    pub recent: &'a [String],
    /// Whether typing a path that isn't an account offers to make it.
    pub allow_new: bool,
    /// Accounts listed before the recent ones, in this order, each with its
    /// group beside it.
    pub pinned: &'a [Pinned],
    /// What it picks, which its words name.
    pub noun: Noun,
}

/// An account the picker lists first, and the group it is listed under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pinned {
    /// The account.
    pub account: String,
    /// What it is listed under: its project.
    pub group: String,
}

/// What a picker picks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Noun {
    /// An account path.
    #[default]
    Account,
    /// A project name.
    Project,
}

impl Noun {
    /// The search box's hint, with or without making a new one.
    fn hint(self, allow_new: bool) -> &'static str {
        match (self, allow_new) {
            (Noun::Account, true) => "Search, or type a new account path",
            (Noun::Account, false) => "Search accounts",
            (Noun::Project, true) => "Search, or type a new project",
            (Noun::Project, false) => "Search projects",
        }
    }

    /// What making `typed` is offered as.
    fn new_label(self, typed: &str) -> String {
        match self {
            Noun::Account => format!("+ New account {typed}"),
            Noun::Project => format!("+ New project {typed}"),
        }
    }

    /// What shows when nothing matches.
    fn none(self) -> &'static str {
        match self {
            Noun::Account => "No account matches",
            Noun::Project => "No project matches",
        }
    }
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
            let hint = choices.noun.hint(choices.allow_new);
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
            let is_pinned = |a: &String| choices.pinned.iter().any(|p| p.account == *a);
            let pinned = choices
                .pinned
                .iter()
                .filter(|p| contains(&p.account, query))
                .map(|p| (Some(p.group.as_str()), &p.account));
            let recent = choices
                .recent
                .iter()
                .filter(|a| contains(a, query) && !is_pinned(a))
                .map(|a| (Some("recent"), a));
            let rest = choices
                .accounts
                .iter()
                .filter(|a| contains(a, query) && !choices.recent.contains(a))
                .map(|a| (None, a));
            for (tag, account) in pinned.chain(recent).chain(rest) {
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
                    if let Some(tag) = tag {
                        ui.label(RichText::new(tag).small().color(palette.text_muted));
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
                .is_ok()
                || choices.pinned.iter().any(|p| p.account == query);
            let new = choices.allow_new && !query.is_empty() && !exists;
            if new {
                let create = ui.add(egui::Button::selectable(
                    false,
                    RichText::new(choices.noun.new_label(query)).color(palette.accent),
                ));
                if create.clicked() {
                    picked.account = Some(query.to_owned());
                }
            } else if shown == 0 {
                ui.label(RichText::new(choices.noun.none()).color(palette.text_muted));
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
