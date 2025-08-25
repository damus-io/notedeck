use std::collections::{BTreeMap, HashSet};

use enostr::Pubkey;
use nostrdb::NoteKey;
use notedeck::NoteRef;

/// A `NoteUnit` represents a cohesive piece of data derived from notes
#[derive(Debug, Clone)]
pub enum NoteUnit {
    Single(NoteRef), // A single note
    Composite(CompositeUnit),
}

impl NoteUnit {
    pub fn key(&self) -> NoteKey {
        match self {
            NoteUnit::Single(note_ref) => note_ref.key,
            NoteUnit::Composite(clustered_entry) => clustered_entry.key(),
        }
    }

    pub fn get_underlying_noteref(&self) -> &NoteRef {
        match self {
            NoteUnit::Single(note_ref) => note_ref,
            NoteUnit::Composite(clustered) => match clustered {
                CompositeUnit::Reaction(reaction_entry) => &reaction_entry.note_reacted_to,
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
}

impl CompositeUnit {
    pub fn get_latest_ref(&self) -> &NoteRef {
        match self {
            CompositeUnit::Reaction(reaction_unit) => reaction_unit.get_latest_ref(),
        }
    }
}

impl PartialEq for CompositeUnit {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Reaction(l0), Self::Reaction(r0)) => l0 == r0,
        }
    }
}

impl CompositeUnit {
    pub fn key(&self) -> NoteKey {
        match self {
            CompositeUnit::Reaction(reaction_entry) => reaction_entry.note_reacted_to.key,
        }
    }
}

impl From<CompositeFragment> for CompositeUnit {
    fn from(value: CompositeFragment) -> Self {
        match value {
            CompositeFragment::Reaction(reaction_fragment) => {
                CompositeUnit::Reaction(reaction_fragment.into())
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

#[derive(Clone)]
pub enum NoteUnitFragment {
    Single(NoteRef),
    Composite(CompositeFragment),
}

#[derive(Debug, Clone)]
pub enum CompositeFragment {
    Reaction(ReactionFragment),
}

impl CompositeFragment {
    pub fn fold_into(self, unit: &mut CompositeUnit) {
        match self {
            CompositeFragment::Reaction(reaction_fragment) => reaction_fragment.fold_into(unit),
        }
    }

    pub fn key(&self) -> NoteKey {
        match self {
            CompositeFragment::Reaction(reaction_fragment) => {
                reaction_fragment.reaction_note_ref.key
            }
        }
    }

    pub fn get_underlying_noteref(&self) -> &NoteRef {
        match self {
            CompositeFragment::Reaction(reaction_fragment) => &reaction_fragment.noteref_reacted_to,
        }
    }

    pub fn get_latest_ref(&self) -> &NoteRef {
        match self {
            CompositeFragment::Reaction(reaction_fragment) => &reaction_fragment.reaction_note_ref,
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
    pub fn fold_into(self, unit: &mut CompositeUnit) {
        match unit {
            CompositeUnit::Reaction(reaction_unit) => {
                if self.noteref_reacted_to != reaction_unit.note_reacted_to {
                    return;
                }

                if reaction_unit.senders.contains(&self.reaction.sender) {
                    return;
                }

                reaction_unit.senders.insert(self.reaction.sender);
                reaction_unit
                    .reactions
                    .insert(self.reaction_note_ref, self.reaction);
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Reaction {
    pub reaction: String, // can't use char because some emojis are 'grapheme clusters'
    pub sender: Pubkey,
}
