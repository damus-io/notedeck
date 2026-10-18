//! What the Adventure has already announced: the seen note.
//!
//! A level-up or an achievement is news once. [`Seen`] is the vault record
//! of what has been announced, one note per account (`d = adv:seen`), so a
//! level-up toasted at one desk is not toasted again at another once the
//! note has synced there.
//!
//! The fold is derived, so it can go down as well as up: a correction can
//! lower a level or take an achievement back. The seen note only ever
//! grows ([`Seen::merge`] keeps each skill's highest level and every
//! achievement), so losing a level and earning it back never announces
//! the same level twice.
//!
//! [`news`] compares the fold's standing ([`Seen::of`]) with the note.
//! With no note at all the account has never opened the Adventure, and
//! everything the history already holds arrives at once: that is one
//! summary ([`News::first_run`]), never a toast per achievement.
//!
//! Fields are tags, one per skill (`["level", <skill key>, <level>]`) and
//! one per achievement (`["ach", <id>]`), read with [`tag_text`] since a
//! project's name is the user's and could be anything.

use nostrdb::{Ndb, Tags, Transaction};
use nostrdb_net::Pubkey;

use super::achievements::{self, Unlocked};
use super::{Adventure, SkillId};
use crate::kinds;
use crate::vault::{self, Record, TagWriter, Vault, VaultError, Written, tag_text};

/// The seen note's record id: there is one per account.
pub const ID: &str = "seen";

/// A skill's level, by its [`SkillId::key`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillLevel {
    /// The skill's [`SkillId::key`].
    pub key: String,
    /// Its level.
    pub level: u8,
}

/// What has been announced, or what the fold stands at now: each skill's
/// level and every achievement unlocked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    /// Each skill's level, in the fold's order.
    pub levels: Vec<SkillLevel>,
    /// The achievements unlocked, by id, in [`achievements::ALL`]'s order.
    pub achievements: Vec<String>,
}

impl Seen {
    /// Where `adventure` stands: its skills' levels, and `unlocked` (its
    /// [`achievements::evaluate`]).
    pub fn of(adventure: &Adventure, unlocked: &[Unlocked]) -> Seen {
        Seen {
            levels: adventure
                .skills
                .iter()
                .map(|skill| SkillLevel {
                    key: skill.id.key(),
                    level: skill.level,
                })
                .collect(),
            achievements: unlocked.iter().map(|u| u.id.to_owned()).collect(),
        }
    }

    /// The level seen for the skill `key`, if it was ever seen.
    pub fn level(&self, key: &str) -> Option<u8> {
        self.levels.iter().find(|l| l.key == key).map(|l| l.level)
    }

    /// Whether achievement `id` was seen.
    pub fn has(&self, id: &str) -> bool {
        self.achievements.iter().any(|a| a == id)
    }

    /// Take in `other`: each skill's higher level, and every achievement
    /// either holds. Returns whether anything changed.
    pub fn merge(&mut self, other: &Seen) -> bool {
        let mut changed = false;
        for theirs in &other.levels {
            match self.levels.iter_mut().find(|l| l.key == theirs.key) {
                Some(ours) if ours.level >= theirs.level => {}
                Some(ours) => {
                    ours.level = theirs.level;
                    changed = true;
                }
                None => {
                    self.levels.push(theirs.clone());
                    changed = true;
                }
            }
        }
        for id in &other.achievements {
            if !self.has(id) {
                self.achievements.push(id.clone());
                changed = true;
            }
        }
        changed
    }
}

impl<'a> Record<'a> for Seen {
    const KIND: u32 = kinds::ADVENTURE;
    const PREFIX: &'static str = "adv";

    fn write_tags<'b>(&self, mut tags: TagWriter<'b>) -> TagWriter<'b> {
        for level in &self.levels {
            tags = tags.text_tag("level", &[&level.key, &level.level.to_string()]);
        }
        for id in &self.achievements {
            tags = tags.text_tag("ach", &[id]);
        }
        tags
    }

    fn from_tags(tags: &Tags<'a>) -> Option<Self> {
        let mut seen = Seen::default();
        for tag in tags.iter() {
            match tag.get_str(0) {
                Some("level") if tag.count() >= 3 => {
                    let (Some(key), Some(level)) = (tag_text(&tag, 1), tag_text(&tag, 2)) else {
                        continue;
                    };
                    let Ok(level) = level.parse() else {
                        continue;
                    };
                    seen.levels.push(SkillLevel {
                        key: key.into_owned(),
                        level,
                    });
                }
                Some("ach") if tag.count() >= 2 => {
                    if let Some(id) = tag_text(&tag, 1) {
                        seen.achievements.push(id.into_owned());
                    }
                }
                _ => {}
            }
        }
        Some(seen)
    }
}

/// `author`'s seen note, or `None` when they never opened the Adventure.
pub fn load(ndb: &Ndb, txn: &Transaction, author: &Pubkey) -> Option<Seen> {
    vault::latest::<Seen>(ndb, txn, author, ID).map(|stored| stored.body)
}

/// Write `seen` as the account's seen note, stamped past `after` (0, or
/// the last write's `created_at`).
pub fn write(vault: &Vault, ndb: &Ndb, seen: &Seen, after: u64) -> Result<Written, VaultError> {
    vault.write(ndb, ID, seen, after)
}

/// A skill that reached a level not announced before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LevelUp {
    /// The skill.
    pub skill: SkillId,
    /// The level it reached.
    pub level: u8,
}

/// What to announce.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct News {
    /// There was no seen note: this is the Adventure's first opening, and
    /// [`Self::achievements`] is everything the history unlocked, to be
    /// summed up in one toast. No level-ups come with it.
    pub first_run: bool,
    /// Skills at a level not announced before, in the fold's order.
    pub level_ups: Vec<LevelUp>,
    /// Achievements not announced before, in [`achievements::ALL`]'s
    /// order.
    pub achievements: Vec<&'static str>,
}

impl News {
    /// Whether there is nothing to announce and nothing to write: not a
    /// first run, and nothing new.
    pub fn is_empty(&self) -> bool {
        !self.first_run && self.level_ups.is_empty() && self.achievements.is_empty()
    }
}

/// What `now` (a fold's [`Seen::of`]) holds that `seen` (the seen note, if
/// there is one) never announced.
///
/// A skill missing from the note is taken to have been seen at level 1,
/// where every skill starts, so a new project skill is announced at the
/// level its history reached. An achievement id no definition in
/// [`achievements::ALL`] knows is never news.
pub fn news(seen: Option<&Seen>, now: &Seen) -> News {
    let new_achievements = || {
        achievements::ALL
            .iter()
            .map(|a| a.id)
            .filter(|id| now.has(id) && !seen.is_some_and(|s| s.has(id)))
            .collect()
    };
    let Some(seen) = seen else {
        return News {
            first_run: true,
            level_ups: Vec::new(),
            achievements: new_achievements(),
        };
    };
    let level_ups = now
        .levels
        .iter()
        .filter(|l| l.level > seen.level(&l.key).unwrap_or(1))
        .filter_map(|l| {
            Some(LevelUp {
                skill: SkillId::from_key(&l.key)?,
                level: l.level,
            })
        })
        .collect();
    News {
        first_run: false,
        level_ups,
        achievements: new_achievements(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use nostrdb::Filter;

    use crate::test_util::{INGEST_TIMEOUT, TempNdb};

    const SECRET: [u8; 32] = [21u8; 32];

    fn lvl(key: &str, level: u8) -> SkillLevel {
        SkillLevel {
            key: key.to_owned(),
            level,
        }
    }

    fn standing(levels: &[SkillLevel], achievements: &[&str]) -> Seen {
        Seen {
            levels: levels.to_vec(),
            achievements: achievements.iter().map(|&id| id.to_owned()).collect(),
        }
    }

    /// With no seen note, everything the history unlocked comes as one
    /// first-run summary and no skill levels up.
    #[test]
    fn first_run_sums_up_the_history() {
        let now = standing(
            &[lvl("earning", 40), lvl("project:Damus", 12)],
            &["first-import", "inbox-zero"],
        );
        let news = news(None, &now);
        assert!(news.first_run);
        assert!(news.level_ups.is_empty());
        assert_eq!(news.achievements, ["first-import", "inbox-zero"]);
    }

    /// A level and an achievement past the note are news; one the note has
    /// is not; a level that went down, or an achievement taken back, is not
    /// news either.
    #[test]
    fn only_what_the_note_lacks_is_news() {
        let seen = standing(&[lvl("earning", 40), lvl("saving", 30)], &["first-import"]);
        let now = standing(
            &[
                lvl("earning", 41),
                lvl("saving", 29),
                lvl("project:Damus", 3),
            ],
            &["first-import", "inbox-zero"],
        );
        let news = news(Some(&seen), &now);
        assert!(!news.first_run);
        assert_eq!(
            news.level_ups,
            [
                LevelUp {
                    skill: SkillId::Earning,
                    level: 41
                },
                LevelUp {
                    skill: SkillId::Project("Damus".to_owned()),
                    level: 3
                },
            ]
        );
        assert_eq!(news.achievements, ["inbox-zero"]);
    }

    /// Merging keeps the higher level and every achievement, so a level
    /// lost and won back is never news again.
    #[test]
    fn merge_only_grows() {
        let mut seen = standing(&[lvl("earning", 41)], &["first-import"]);
        assert!(!seen.merge(&standing(&[lvl("earning", 39)], &[])));
        assert_eq!(seen.level("earning"), Some(41));
        assert!(seen.merge(&standing(&[lvl("saving", 5)], &["inbox-zero"])));
        assert_eq!(seen.level("saving"), Some(5));
        assert!(seen.has("inbox-zero") && seen.has("first-import"));
        assert!(news(Some(&seen), &standing(&[lvl("earning", 41)], &[])).is_empty());
    }

    /// The note round-trips through the vault, and once it has, the same
    /// standing is no news: the same level-up never toasts twice, on this
    /// machine or any other that reads the note.
    #[tokio::test]
    async fn no_news_after_a_round_trip() {
        let db = TempNdb::new();
        let ndb = &db.ndb;
        let vault = Vault::new(&SECRET).unwrap();
        vault.register(ndb);

        let now = standing(
            &[lvl("earning", 41), lvl("project:A Project", 3)],
            &["first-import"],
        );
        {
            let txn = Transaction::new(ndb).unwrap();
            assert_eq!(load(ndb, &txn, vault.author()), None);
        }
        let sub = ndb
            .subscribe(&[Filter::new().kinds([kinds::ADVENTURE as u64]).build()])
            .unwrap();
        let mut merged = now.clone();
        assert!(news(None, &now).first_run);
        merged.merge(&now);
        write(&vault, ndb, &merged, 0).unwrap();
        ndb.wait_for_all_notes_within(sub, 1, INGEST_TIMEOUT)
            .await
            .unwrap();

        let txn = Transaction::new(ndb).unwrap();
        let read = load(ndb, &txn, vault.author()).expect("the note landed");
        assert_eq!(read, now);
        assert!(news(Some(&read), &now).is_empty());
    }
}
