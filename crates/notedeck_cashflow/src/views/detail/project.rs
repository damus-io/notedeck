//! The detail's Project section: which project each counter leg is in, and
//! the two ways to change that.
//!
//! Projects are set per account, never per transaction (the chart of
//! accounts' `project`, inherited down the tree: [`cashflow::projects`]),
//! so a transaction's project is its counter leg's account's. The section
//! shows, a leg each, the project and the account that puts it there
//! ("damus · from income:damus"), or "Unattributed". The project's chip
//! opens the Projects view on it.
//!
//! Its two writes:
//!
//! - **Move to a project account**: send this one transaction to an
//!   account in a project. It is an override (a split's editor for a
//!   split), so the Categorize section makes it, with the accounts in a
//!   project listed first ([`super::categorize::Event::Move`]) and the
//!   same plan before its Apply.
//! - **Tag the account with a project**: an [`Action::SetProject`] on the
//!   leg's account, which keeps the account's other metadata. It moves
//!   every transaction under the account, so the panel says how many
//!   first. Its plan would have nothing to say (a project changes no
//!   edit derive reads), so it isn't planned: the metadata is checked as
//!   [`cashflow::triage::apply`] will check it, and Apply queues it.
//!
//! Closed, it draws its galleys ([`ProjectGalleys`]), laid out with the
//! rest of the detail's, and allocates nothing a frame.

use std::sync::Arc;

use cashflow::Books;
use cashflow::chart::AccountMeta;
use cashflow::projects::Projects;
use cashflow::triage::Action;
use egui::{Galley, RichText};

use super::model::{LegProject, txns_under};
use super::{Layouter, chip, heading, text};
use crate::ui::account_picker::{self, AccountPicker, Choices, Noun, Pinned};
use crate::ui::palette::Palette;

/// What the section keeps between frames, for the record open; a new
/// record starts it afresh.
#[derive(Default)]
pub(crate) struct ProjectState {
    /// The tag being drafted, if its panel is open.
    tag: Option<TagDraft>,
    picker: AccountPicker,
    /// The books' generation a tag was queued on: "Saving…" until newer
    /// books arrive.
    saving: Option<u64>,
    /// Why the last write failed.
    error: Option<String>,
}

/// A tag being drafted for one leg's account.
struct TagDraft {
    /// The leg it is for, an index of [`super::model::DetailText::projects`].
    leg: usize,
    /// The account it tags.
    account: String,
    /// "Tags income:damus and everything under it: 12 transactions."
    reach: String,
    /// The project picked, `Some(None)` to clear it; `None` until one is.
    project: Option<Option<String>>,
    /// Why the pick can't be written.
    problem: Option<String>,
}

impl ProjectState {
    /// Before a frame of the books at `generation` draws: stop saying
    /// "Saving…" once newer books arrived.
    pub(crate) fn prepare(&mut self, generation: u64) {
        if self.saving.is_some_and(|g| g != generation) {
            self.saving = None;
        }
    }

    /// A write this section queued failed with `err`.
    pub(crate) fn write_failed(&mut self, err: &str) {
        if self.saving.take().is_some() {
            self.error = Some(err.to_owned());
        }
    }
}

/// What the pickers choose from, listed once per load of the books.
#[derive(Default)]
pub(crate) struct ProjectChoices {
    /// The books' generation they were listed for.
    generation: Option<u64>,
    /// Every account in a project, grouped by project, then by name.
    pinned: Vec<Pinned>,
    /// Every other account, sorted.
    others: Vec<String>,
    /// Every project's name, sorted, once each.
    projects: Vec<String>,
}

impl ProjectChoices {
    /// List them again from `names`, every account sorted, placed in
    /// `projects`, if the books at `generation` are new.
    pub(crate) fn refresh(&mut self, generation: u64, names: &[String], projects: &Projects) {
        if self.generation == Some(generation) {
            return;
        }
        self.generation = Some(generation);
        self.pinned.clear();
        self.others.clear();
        for name in names {
            match projects.project_of(name) {
                Some(project) => self.pinned.push(Pinned {
                    account: name.clone(),
                    group: project.to_owned(),
                }),
                None => self.others.push(name.clone()),
            }
        }
        self.pinned.sort_by(|a, b| {
            (a.group.as_str(), a.account.as_str()).cmp(&(b.group.as_str(), b.account.as_str()))
        });
        self.projects = projects.roots().iter().map(|r| r.project.clone()).collect();
        self.projects.sort_unstable();
        self.projects.dedup();
    }

    /// The accounts the Categorize picker chooses from when it moves the
    /// transaction to a project's account: those in a project first, then
    /// `recent`, then the rest.
    pub(crate) fn accounts<'a>(&'a self, recent: &'a [String]) -> Choices<'a> {
        Choices {
            accounts: &self.others,
            recent,
            allow_new: true,
            pinned: &self.pinned,
            noun: Noun::Account,
        }
    }

    /// The projects a tag chooses between, a new one too.
    fn projects(&self) -> Choices<'_> {
        Choices {
            accounts: &self.projects,
            allow_new: true,
            noun: Noun::Project,
            ..Choices::default()
        }
    }
}

/// The section's texts, laid out with the rest of the detail's.
pub(super) struct ProjectGalleys {
    heading: Arc<Galley>,
    legs: Vec<LegGalleys>,
    /// "Move to a project account…"; `None` when it can't be moved.
    move_to: Option<Arc<Galley>>,
}

/// One counter leg's galleys.
struct LegGalleys {
    /// The leg's account, over a split's lines.
    account: Option<Arc<Galley>>,
    /// The project's chip, which opens it; `None` when unattributed.
    project: Option<Arc<Galley>>,
    /// "from income:damus", or "Unattributed".
    from: Arc<Galley>,
    /// "Tag income:damus with a project…"; `None` on the default account.
    tag: Option<Arc<Galley>>,
}

impl ProjectGalleys {
    /// The section's galleys for `legs`, wrapped at `width`; `movable`
    /// when the transaction can be recategorized.
    pub(super) fn lay_out(
        l: &Layouter<'_>,
        legs: &[LegProject],
        movable: bool,
        width: f32,
    ) -> Self {
        let split = legs.len() > 1;
        ProjectGalleys {
            heading: l.bold("Project", 15.0),
            legs: legs
                .iter()
                .map(|leg| LegGalleys {
                    account: split.then(|| l.line(&leg.account, 13.0)),
                    project: leg.project.as_ref().map(|p| l.line(&p.project, 12.0)),
                    from: match &leg.project {
                        Some(p) if p.root == leg.account => l.line("set on this account", 13.0),
                        Some(p) => l.wrapped(&format!("from {}", p.root), 13.0, width - 80.0),
                        None => l.line("Unattributed", 13.0),
                    },
                    tag: (!leg.unknown).then(|| {
                        l.wrapped(
                            &format!("Tag {} with a project…", leg.account),
                            14.0,
                            width - 16.0,
                        )
                    }),
                })
                .collect(),
            move_to: movable.then(|| l.line("Move to a project account…", 14.0)),
        }
    }

    /// Whether the section shows: the transaction has a counter leg.
    pub(super) fn shows(&self) -> bool {
        !self.legs.is_empty()
    }
}

/// Something the section was asked to do this frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// Open the Projects view on the project of the leg at this index.
    OpenProject(usize),
    /// Move the transaction to a project's account, in the Categorize
    /// section.
    Move,
    /// Open the tag panel for the leg at this index, or close it.
    Tag(usize),
    /// The tag's picker chose this project.
    Picked(String),
    /// Clear the project the leg's account sets.
    Clear,
    /// The tag's picker was dismissed.
    PickerCancelled,
    /// Close the tag panel.
    Cancel,
    /// Queue the tag.
    Apply,
}

/// What [`act`] writes with.
pub(crate) struct Cx<'a> {
    /// The books on screen.
    pub(crate) books: &'a Books,
    /// Their generation.
    pub(crate) generation: u64,
    /// Where an applied write is queued.
    pub(crate) outbox: &'a mut Vec<Action>,
}

/// Do what `event` asked of the section, whose legs are `legs`. The
/// detail handles [`Event::OpenProject`] and [`Event::Move`] itself.
pub(crate) fn act(state: &mut ProjectState, event: Event, legs: &[LegProject], cx: Cx<'_>) {
    match event {
        Event::OpenProject(_) | Event::Move => {}
        Event::Tag(at) if state.tag.as_ref().is_some_and(|t| t.leg == at) => state.tag = None,
        Event::Tag(at) => {
            let Some(leg) = legs.get(at) else {
                return;
            };
            state.error = None;
            state.tag = Some(TagDraft {
                leg: at,
                account: leg.account.clone(),
                reach: reach(&leg.account, txns_under(cx.books, &leg.account)),
                project: None,
                problem: None,
            });
            state.picker.open();
        }
        Event::Picked(project) => {
            let Some(tag) = state.tag.as_mut() else {
                return;
            };
            let current = legs.get(tag.leg).and_then(|l| l.project.as_ref());
            tag.problem = match current {
                Some(c) if c.root == tag.account && c.project == project => {
                    Some(format!("{} is already in {project}", tag.account))
                }
                _ => check(&tag.account, Some(&project)),
            };
            tag.project = Some(Some(project));
        }
        Event::Clear => {
            if let Some(tag) = state.tag.as_mut() {
                tag.problem = None;
                tag.project = Some(None);
            }
        }
        Event::PickerCancelled => {
            if state.tag.as_ref().is_some_and(|t| t.project.is_none()) {
                state.tag = None;
            }
        }
        Event::Cancel => state.tag = None,
        Event::Apply => {
            let Some(tag) = state.tag.take() else {
                return;
            };
            let Some(project) = tag.project.clone().filter(|_| tag.problem.is_none()) else {
                state.tag = Some(tag);
                return;
            };
            cx.outbox.push(Action::SetProject {
                account: tag.account,
                project,
            });
            state.saving = Some(cx.generation);
        }
    }
}

/// What tagging `account`, which `txns` transactions post under, reaches.
fn reach(account: &str, txns: usize) -> String {
    let rows = if txns == 1 {
        "transaction"
    } else {
        "transactions"
    };
    format!("Tags {account} and everything under it: {txns} {rows}.")
}

/// Why `account`'s metadata with `project` can't be stored, in the words
/// [`cashflow::triage::apply`] would refuse it with.
fn check(account: &str, project: Option<&str>) -> Option<String> {
    let meta = AccountMeta {
        project,
        ..AccountMeta::default()
    };
    meta.check(account).err().map(|e| e.to_string())
}

/// What [`section_ui`] draws with.
pub(super) struct Section<'a> {
    pub(super) state: &'a mut ProjectState,
    pub(super) choices: &'a ProjectChoices,
    /// The legs it shows.
    pub(super) legs: &'a [LegProject],
}

/// The Project section: a leg's project each, its tag panel when open, and
/// the move.
pub(super) fn section_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    g: &ProjectGalleys,
    section: Section<'_>,
) -> Option<Event> {
    let Section {
        state,
        choices,
        legs,
    } = section;
    heading(ui, palette, &g.heading);
    let mut event = None;
    for (at, leg) in g.legs.iter().enumerate() {
        if let Some(account) = &leg.account {
            ui.add_space(4.0);
            text(ui, account, palette.text);
        }
        ui.horizontal_wrapped(|ui| {
            if let Some(project) = &leg.project {
                let chip = chip(ui, project, palette.accent, egui::Sense::click())
                    .on_hover_text("Open this project");
                if chip.clicked() {
                    event = Some(Event::OpenProject(at));
                }
            }
            text(ui, &leg.from, palette.text_muted);
        });
        if let Some(tag) = &leg.tag {
            let open = state.tag.as_ref().is_some_and(|t| t.leg == at);
            if ui
                .add(egui::Button::new(tag.clone()).selected(open))
                .clicked()
            {
                event = Some(Event::Tag(at));
            }
        }
        if state.tag.as_ref().is_some_and(|t| t.leg == at)
            && let Some(e) = tag_ui(ui, palette, state, choices, legs.get(at))
        {
            event = Some(e);
        }
    }
    if let Some(move_to) = &g.move_to
        && ui
            .add(egui::Button::new(move_to.clone()))
            .on_hover_text("Override this one transaction into an account in a project")
            .clicked()
    {
        event = Some(Event::Move);
    }
    if state.saving.is_some() {
        ui.label(RichText::new("Saving…").color(palette.text_muted));
    }
    if let Some(error) = &state.error {
        ui.label(RichText::new(error.as_str()).color(palette.warning));
    }
    event
}

/// The tag panel: what it reaches, the project picker until one is
/// picked, then what it writes, and Apply and Cancel.
fn tag_ui(
    ui: &mut egui::Ui,
    palette: &Palette,
    state: &mut ProjectState,
    choices: &ProjectChoices,
    leg: Option<&LegProject>,
) -> Option<Event> {
    let mut event = None;
    let ProjectState { tag, picker, .. } = state;
    let tag = tag.as_ref()?;
    ui.label(
        RichText::new(tag.reach.as_str())
            .size(13.0)
            .color(palette.warning),
    );
    let Some(project) = &tag.project else {
        let picked = account_picker::show(ui, palette, picker, choices.projects());
        if let Some(project) = picked.account {
            return Some(Event::Picked(project));
        }
        if picked.cancelled {
            return Some(Event::PickerCancelled);
        }
        // The account itself sets one, so it can be cleared.
        if let Some(p) = leg
            .and_then(|l| l.project.as_ref())
            .filter(|p| p.root == tag.account)
            && ui
                .small_button(format!("Remove it from {}", p.project))
                .clicked()
        {
            event = Some(Event::Clear);
        }
        return event;
    };
    let says = match project {
        Some(project) => format!("{} → {project}", tag.account),
        None => format!("{} → no project", tag.account),
    };
    ui.label(RichText::new(says).size(13.0).color(palette.text));
    if let Some(problem) = &tag.problem {
        ui.label(
            RichText::new(problem.as_str())
                .size(13.0)
                .color(palette.warning),
        );
    }
    ui.horizontal(|ui| {
        let apply = egui::Button::new("Apply tag");
        if ui.add_enabled(tag.problem.is_none(), apply).clicked() {
            event = Some(Event::Apply);
        }
        if ui.button("Cancel").clicked() {
            event = Some(Event::Cancel);
        }
    });
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use cashflow::projects::ProjectRoot;

    /// The accounts in a project come first, by project then name, and
    /// every project is named once.
    #[test]
    fn choices_group_by_project() {
        let names: Vec<String> = [
            "expenses:food",
            "expenses:housing",
            "expenses:housing:rent",
            "income:damus",
            "income:zap",
        ]
        .map(str::to_owned)
        .into();
        let root = |account: &str, project: &str| ProjectRoot {
            account: account.to_owned(),
            project: project.to_owned(),
        };
        let projects = Projects::new([
            root("income:damus", "damus"),
            root("expenses:housing", "home"),
            root("income:zap", "damus"),
        ]);
        let mut choices = ProjectChoices::default();
        choices.refresh(1, &names, &projects);
        let pinned: Vec<(&str, &str)> = choices
            .pinned
            .iter()
            .map(|p| (p.group.as_str(), p.account.as_str()))
            .collect();
        assert_eq!(
            pinned,
            [
                ("damus", "income:damus"),
                ("damus", "income:zap"),
                ("home", "expenses:housing"),
                ("home", "expenses:housing:rent"),
            ]
        );
        assert_eq!(choices.others, ["expenses:food"]);
        assert_eq!(choices.projects, ["damus", "home"]);
    }

    /// The reach counts in the singular too.
    #[test]
    fn reach_says_how_many() {
        assert_eq!(
            reach("income:damus", 1),
            "Tags income:damus and everything under it: 1 transaction."
        );
        assert_eq!(
            reach("income:damus", 12),
            "Tags income:damus and everything under it: 12 transactions."
        );
    }

    /// A blank project is refused as apply would refuse it.
    #[test]
    fn check_refuses_a_blank_project() {
        assert_eq!(check("income:damus", Some("damus")), None);
        assert!(check("income:damus", Some(" damus")).is_some());
    }
}
