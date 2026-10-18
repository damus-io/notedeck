//! The Adventure's news: level-up and achievement toasts in any view.
//!
//! Each time the Adventure is folded again ([`AdventureState`]), what it
//! stands at is compared with the vault's seen note
//! ([`cashflow::adventure::seen`]); whatever the note never announced is
//! queued as a toast, and the note is written with it, so the same news
//! never toasts twice, at this desk or any other the note syncs to.
//!
//! The dashboard is not gamified until the person asks for the game: with
//! no seen note, nothing is announced, and the Adventure is not folded
//! outside its own view, until the Adventure view is first opened. That
//! first opening sums up what the history had already unlocked in one
//! toast ("You unlocked 14 achievements from your history!") rather than a
//! toast per achievement, and writes the note that turns the toasts on.
//!
//! The note this app writes is kept and merged with whatever the vault
//! later says ([`Seen::merge`] only grows), so a fold that lands before
//! the write has come back through nostrdb still sees it.

use cashflow::adventure::achievements;
use cashflow::adventure::seen::{self, News, Seen};
use notedeck::Renderer3d;

use crate::ui::hud::{Toast, Toasts};
use crate::ui::palette::Palette;
use crate::views::adventure::AdventureState;

/// More achievements than this unlocked at once are summed up in one
/// toast rather than one each.
pub const MAX_ACHIEVEMENT_TOASTS: usize = 3;

/// What the vault's seen note says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Announced {
    /// The vault hasn't been read yet: announce nothing, since a missing
    /// note can't be told from one not read.
    #[default]
    Unknown,
    /// There is no seen note: the Adventure was never opened.
    Never,
    /// What the note says was announced.
    Seen(Seen),
}

/// The toasts, and what the news is compared with.
#[derive(Default)]
pub struct AdventureNews {
    /// The toasts waiting, drawn over every view.
    toasts: Toasts,
    /// The seen note, merged with what this app wrote since.
    announced: Announced,
    /// The [`AdventureState::generation`] last compared, so a fold is
    /// announced once.
    checked: Option<u64>,
    /// A seen note to write, once a frame has the host's account.
    pending: Option<Seen>,
    /// The last seen note written's `created_at`, which the next write is
    /// stamped past.
    written_at: u64,
}

impl AdventureNews {
    /// Take what the vault's seen note says, merged with what this app
    /// already announced.
    pub fn set_announced(&mut self, from_vault: Announced) {
        self.announced = match (from_vault, std::mem::take(&mut self.announced)) {
            (Announced::Seen(mut note), Announced::Seen(ours)) => {
                note.merge(&ours);
                Announced::Seen(note)
            }
            // The vault hasn't caught up with this app's own write.
            (Announced::Never | Announced::Unknown, ours @ Announced::Seen(_)) => ours,
            (from_vault, _) => from_vault,
        };
    }

    /// What the seen note says, as far as this app knows.
    pub fn announced(&self) -> &Announced {
        &self.announced
    }

    /// Whether the Adventure should be folded while another view shows:
    /// once it has been opened, so its news reaches every view.
    pub fn wants_fold(&self) -> bool {
        matches!(self.announced, Announced::Seen(_))
    }

    /// Compare `adventure`'s latest fold with the seen note, once per fold,
    /// and queue what is news. `opened` says the Adventure view is showing,
    /// which a first run waits for.
    #[profiling::function]
    pub(crate) fn check(&mut self, adventure: &AdventureState, opened: bool) {
        let Some(report) = adventure.report() else {
            return;
        };
        if self.checked == Some(adventure.generation()) {
            return;
        }
        let seen = match &self.announced {
            Announced::Unknown => return,
            Announced::Never if !opened => return,
            Announced::Never => None,
            Announced::Seen(seen) => Some(seen),
        };
        self.checked = Some(adventure.generation());
        let news = seen::news(seen, &report.standing);
        if news.is_empty() {
            return;
        }
        let mut merged = seen.cloned().unwrap_or_default();
        merged.merge(&report.standing);
        for toast in toasts(&news) {
            self.toasts.push(toast);
        }
        self.announced = Announced::Seen(merged.clone());
        self.pending = Some(merged);
    }

    /// The seen note to write, taken, and the `created_at` to stamp past.
    pub fn take_write(&mut self) -> Option<SeenWrite> {
        let seen = self.pending.take()?;
        Some(SeenWrite {
            seen,
            after: self.written_at,
        })
    }

    /// The seen note was written, stamped `created_at`.
    pub fn written(&mut self, created_at: u64) {
        self.written_at = created_at;
    }

    /// The toasts waiting.
    pub fn toasts(&self) -> &Toasts {
        &self.toasts
    }

    /// Draw the front toast, if any, over whatever view is showing.
    pub fn show(&mut self, ui: &egui::Ui, palette: &Palette, renderer: Option<&Renderer3d>) {
        self.toasts.show(ui, palette, renderer);
    }
}

/// A seen note waiting to be written, and the `created_at` to stamp it
/// past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenWrite {
    /// The note.
    pub seen: Seen,
    /// The last write's `created_at`.
    pub after: u64,
}

/// The toasts that announce `news`: the first run's summary, a level-up
/// per skill, then each achievement, or one summing them up when there
/// are more than [`MAX_ACHIEVEMENT_TOASTS`].
pub fn toasts(news: &News) -> Vec<Toast> {
    let points: u32 = news
        .achievements
        .iter()
        .filter_map(|id| achievements::get(id))
        .map(|a| a.points)
        .sum();
    if news.first_run {
        if news.achievements.is_empty() {
            return Vec::new();
        }
        return vec![Toast::history(news.achievements.len(), points)];
    }
    let mut out: Vec<Toast> = news
        .level_ups
        .iter()
        .map(|l| Toast::level_up(l.skill.name(), l.level))
        .collect();
    if news.achievements.len() > MAX_ACHIEVEMENT_TOASTS {
        out.push(Toast::achievements(news.achievements.len(), points));
        return out;
    }
    out.extend(
        news.achievements
            .iter()
            .filter_map(|id| achievements::get(id))
            .map(|a| Toast::achievement(a.name, a.points)),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use cashflow::adventure::SkillId;
    use cashflow::adventure::seen::LevelUp;

    /// The first run is one summary, however much the history unlocked,
    /// and nothing when it unlocked nothing.
    #[test]
    fn first_run_is_one_summary() {
        let news = News {
            first_run: true,
            level_ups: Vec::new(),
            achievements: vec!["first-import", "inbox-zero", "income-1k", "streak-3"],
        };
        let toasts = toasts(&news);
        assert_eq!(toasts.len(), 1);
        assert_eq!(
            toasts[0].body,
            "You unlocked 4 achievements from your history!"
        );
        let quiet = News {
            first_run: true,
            ..News::default()
        };
        assert!(super::toasts(&quiet).is_empty());
    }

    /// A level-up each, then each achievement while there are few.
    #[test]
    fn level_ups_then_achievements() {
        let news = News {
            first_run: false,
            level_ups: vec![LevelUp {
                skill: SkillId::Bookkeeping,
                level: 12,
            }],
            achievements: vec!["inbox-zero"],
        };
        let bodies: Vec<String> = toasts(&news).into_iter().map(|t| t.body).collect();
        assert_eq!(bodies, ["Bookkeeping is now level 12", "Inbox Zero"]);
    }

    /// A note this app wrote isn't lost to a vault that hasn't caught up,
    /// and merges with one that has more.
    #[test]
    fn our_write_survives_a_stale_vault() {
        let ours = Seen {
            levels: Vec::new(),
            achievements: vec!["first-import".to_owned()],
        };
        let mut news = AdventureNews::default();
        news.set_announced(Announced::Seen(ours.clone()));
        news.set_announced(Announced::Never);
        assert_eq!(news.announced(), &Announced::Seen(ours));
        news.set_announced(Announced::Seen(Seen {
            levels: Vec::new(),
            achievements: vec!["inbox-zero".to_owned()],
        }));
        let Announced::Seen(seen) = news.announced() else {
            panic!("still seen");
        };
        assert!(seen.has("first-import") && seen.has("inbox-zero"));
    }
}
