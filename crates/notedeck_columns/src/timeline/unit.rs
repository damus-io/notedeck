use std::collections::{BTreeMap, HashSet};

use enostr::Pubkey;
use notedeck::NoteRef;

use crate::timeline::note_units::{CompositeKey, CompositeType, UnitKey};

/// A `NoteUnit` represents a cohesive piece of data derived from notes
#[derive(Debug, Clone)]
pub enum NoteUnit {
    Single(NoteRef), // A single note
    Composite(CompositeUnit),
}

impl NoteUnit {
    pub fn key(&self) -> UnitKey {
        match self {
            NoteUnit::Single(note_ref) => UnitKey::Single(note_ref.key),
            NoteUnit::Composite(clustered_entry) => UnitKey::Composite(clustered_entry.key()),
        }
    }

    pub fn get_underlying_noteref(&self) -> &NoteRef {
        match self {
            NoteUnit::Single(note_ref) => note_ref,
            NoteUnit::Composite(clustered) => match clustered {
                CompositeUnit::Reaction(reaction_entry) => &reaction_entry.note_reacted_to,
                CompositeUnit::Repost(repost_unit) => &repost_unit.note_reposted,
            },
        }
    }

    pub fn get_latest_ref(&self) -> &NoteRef {
        match self {
            NoteUnit::Single(note_ref) => note_ref,
            NoteUnit::Composite(composite_unit) => composite_unit.get_latest_ref(),
        }
    }
}

impl Ord for NoteUnit {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.get_latest_ref().cmp(other.get_latest_ref())
    }
}

impl PartialEq for NoteUnit {
    fn eq(&self, other: &Self) -> bool {
        self.get_latest_ref() == other.get_latest_ref()
    }
}

impl Eq for NoteUnit {}

impl PartialOrd for NoteUnit {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Combines potentially many notes into one cohesive piece of data
#[derive(Debug, Clone)]
pub enum CompositeUnit {
    Reaction(ReactionUnit),
    Repost(RepostUnit),
}

impl CompositeUnit {
    pub fn get_latest_ref(&self) -> &NoteRef {
        match self {
            CompositeUnit::Reaction(reaction_unit) => reaction_unit.get_latest_ref(),
            CompositeUnit::Repost(repost_unit) => repost_unit.get_latest_ref(),
        }
    }
}

impl PartialEq for CompositeUnit {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Reaction(l0), Self::Reaction(r0)) => l0 == r0,
            (Self::Repost(l0), Self::Repost(r0)) => l0 == r0,
            _ => false,
        }
    }
}

impl CompositeUnit {
    pub fn key(&self) -> CompositeKey {
        match self {
            CompositeUnit::Reaction(reaction_entry) => CompositeKey {
                key: reaction_entry.note_reacted_to.key,
                composite_type: CompositeType::Reaction,
            },
            CompositeUnit::Repost(repost_unit) => CompositeKey {
                key: repost_unit.note_reposted.key,
                composite_type: CompositeType::Repost,
            },
        }
    }
}

impl From<CompositeFragment> for CompositeUnit {
    fn from(value: CompositeFragment) -> Self {
        match value {
            CompositeFragment::Reaction(reaction_fragment) => {
                CompositeUnit::Reaction(reaction_fragment.into())
            }
            CompositeFragment::Repost(repost_fragment) => {
                CompositeUnit::Repost(repost_fragment.into())
            }
        }
    }
}

/// Represents all the reactions to a specific note `ReactionUnit::note_reacted_to`
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ReactionUnit {
    pub note_reacted_to: NoteRef, // NOTE: this should not be modified after it's created
    pub reactions: BTreeMap<NoteRef, Reaction>,
    pub senders: HashSet<Pubkey>, // useful for making sure the same user can't add more than one reaction to a note
}

impl ReactionUnit {
    pub fn get_latest_ref(&self) -> &NoteRef {
        self.reactions
            .first_key_value()
            .map(|(r, _)| r)
            .unwrap_or(&self.note_reacted_to)
    }
}

impl From<ReactionFragment> for ReactionUnit {
    fn from(frag: ReactionFragment) -> Self {
        let mut senders = HashSet::new();
        senders.insert(frag.reaction.sender);

        let mut reactions = BTreeMap::new();
        reactions.insert(frag.reaction_note_ref, frag.reaction);

        Self {
            note_reacted_to: frag.noteref_reacted_to,
            reactions,
            senders,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct RepostUnit {
    pub note_reposted: NoteRef,
    pub reposts: BTreeMap<NoteRef, Pubkey>, // repost note to sender
    pub senders: HashSet<Pubkey>,
}

impl RepostUnit {
    pub fn get_latest_ref(&self) -> &NoteRef {
        self.reposts
            .first_key_value()
            .map(|(r, _)| r)
            .unwrap_or(&self.note_reposted)
    }
}

impl From<RepostFragment> for RepostUnit {
    fn from(value: RepostFragment) -> Self {
        let mut reposts = BTreeMap::new();
        reposts.insert(value.repost_noteref, value.reposter);

        let mut senders = HashSet::new();
        senders.insert(value.reposter);

        Self {
            note_reposted: value.reposted_noteref,
            reposts,
            senders,
        }
    }
}

#[derive(Clone)]
pub enum NoteUnitFragment {
    Single(NoteRef),
    Composite(CompositeFragment),
}

#[derive(Debug, Clone)]
pub enum CompositeFragment {
    Reaction(ReactionFragment),
    Repost(RepostFragment),
}

impl CompositeFragment {
    pub fn fold_into(self, unit: &mut CompositeUnit) {
        match self {
            CompositeFragment::Reaction(reaction_fragment) => {
                let CompositeUnit::Reaction(reaction_unit) = unit else {
                    tracing::error!("Attempting to fold a reaction fragment into a unit which isn't ReactionUnit. Doing nothing, this should never occur");
                    return;
                };

                reaction_fragment.fold_into(reaction_unit);
            }
            CompositeFragment::Repost(repost_fragment) => {
                let CompositeUnit::Repost(repost_unit) = unit else {
                    tracing::error!("Attempting to fold a repost fragment into a unit which isn't RepostUnit. Doing nothing, this should never occur");
                    return;
                };

                repost_fragment.fold_into(repost_unit);
            }
        }
    }

    pub fn key(&self) -> CompositeKey {
        match self {
            CompositeFragment::Reaction(reaction) => CompositeKey {
                key: reaction.noteref_reacted_to.key,
                composite_type: CompositeType::Reaction,
            },
            CompositeFragment::Repost(repost) => CompositeKey {
                key: repost.reposted_noteref.key,
                composite_type: CompositeType::Repost,
            },
        }
    }

    pub fn get_underlying_noteref(&self) -> &NoteRef {
        match self {
            CompositeFragment::Reaction(reaction_fragment) => &reaction_fragment.noteref_reacted_to,
            CompositeFragment::Repost(repost_fragment) => &repost_fragment.reposted_noteref,
        }
    }

    pub fn get_latest_ref(&self) -> &NoteRef {
        match self {
            CompositeFragment::Reaction(reaction_fragment) => &reaction_fragment.reaction_note_ref,
            CompositeFragment::Repost(repost_fragment) => &repost_fragment.repost_noteref,
        }
    }

    pub fn get_type(&self) -> CompositeType {
        match self {
            CompositeFragment::Reaction(_) => CompositeType::Reaction,
            CompositeFragment::Repost(_) => CompositeType::Repost,
        }
    }
}

/// A singluar reaction to a note
#[derive(Debug, Clone)]
pub struct ReactionFragment {
    pub noteref_reacted_to: NoteRef,
    pub reaction_note_ref: NoteRef,
    pub reaction: Reaction,
}

impl ReactionFragment {
    /// Add all the contents of Self into `CompositeUnit`
    pub fn fold_into(self, unit: &mut ReactionUnit) {
        if self.noteref_reacted_to != unit.note_reacted_to {
            tracing::error!("Attempting to fold a reaction fragment into a ReactionUnit which as a different note reacted to: {:?} != {:?}. This should never occur", self.noteref_reacted_to, unit.note_reacted_to);
            return;
        }

        if unit.senders.contains(&self.reaction.sender) {
            return;
        }

        unit.senders.insert(self.reaction.sender);
        unit.reactions.insert(self.reaction_note_ref, self.reaction);
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Reaction {
    pub reaction: String, // can't use char because some emojis are 'grapheme clusters'
    pub sender: Pubkey,
}

/// Represents a singular repost
#[derive(Debug, Clone)]
pub struct RepostFragment {
    pub reposted_noteref: NoteRef,
    pub repost_noteref: NoteRef,
    pub reposter: Pubkey,
}

impl RepostFragment {
    pub fn fold_into(self, unit: &mut RepostUnit) {
        if self.reposted_noteref != unit.note_reposted {
            tracing::error!("Attempting to fold a repost fragment into a RepostUnit which has a different note reposted: {:?} != {:?}. This should never occur", self.reposted_noteref, unit.note_reposted);
            return;
        }

        if unit.senders.contains(&self.reposter) {
            return;
        }

        unit.senders.insert(self.reposter);
        unit.reposts.insert(self.repost_noteref, self.reposter);
    }
}
