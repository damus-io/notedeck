use std::collections::HashSet;

use enostr::Pubkey;
use nostrdb::{Ndb, Note, NoteKey, Transaction};
use notedeck::NoteRef;
use notedeck_ui::note::get_reposted_note;

use crate::timeline::{
    note_units::{InsertManyResponse, NoteUnits},
    unit::{
        CompositeFragment, NoteUnit, NoteUnitFragment, Reaction, ReactionFragment, RepostFragment,
    },
};

#[derive(Debug, Default)]
pub struct TimelineUnits {
    pub units: NoteUnits,
}

impl TimelineUnits {
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            units: NoteUnits::new_with_cap(cap, false),
        }
    }

    pub fn from_refs_single(refs: Vec<NoteRef>) -> Self {
        let mut entries = TimelineUnits::default();
        refs.into_iter().for_each(|r| entries.merge_single_note(r));
        entries
    }

    pub fn len(&self) -> usize {
        self.units.len()
    }

    pub fn is_empty(&self) -> bool {
        self.units.len() == 0
    }

    /// returns number of new entries merged
    pub fn merge_new_notes<'a>(
        &mut self,
        payloads: Vec<&'a NotePayload>,
        ndb: &Ndb,
        txn: &Transaction,
    ) -> MergeResponse<'a> {
        let mut unknown_pks = HashSet::with_capacity(payloads.len());
        let new_fragments = payloads
            .into_iter()
            .filter_map(|p| to_fragment(p, ndb, txn))
            .map(|f| {
                if let Some(pk) = f.unknown_pk {
                    unknown_pks.insert(pk);
                }
                f.fragment
            })
            .collect();

        let tl_response = if unknown_pks.is_empty() {
            None
        } else {
            Some(UnknownPks { unknown_pks })
        };

        MergeResponse {
            insertion_response: self.units.merge_fragments(new_fragments),
            tl_response,
        }
    }

    pub fn latest(&self) -> Option<&NoteRef> {
        self.units.latest_ref()
    }

    pub fn merge_single_note(&mut self, note_ref: NoteRef) {
        self.units.merge_single_unit(note_ref);
    }

    /// Used in the view
    pub fn get(&self, index: usize) -> Option<&NoteUnit> {
        self.units.kth(index)
    }
}

pub struct MergeResponse<'a> {
    pub insertion_response: InsertManyResponse,
    pub tl_response: Option<UnknownPks<'a>>,
}

pub struct UnknownPks<'a> {
    pub(crate) unknown_pks: HashSet<&'a [u8; 32]>,
}

pub struct NoteUnitFragmentResponse<'a> {
    pub fragment: NoteUnitFragment,
    pub unknown_pk: Option<&'a [u8; 32]>,
}

pub struct NotePayload<'a> {
    pub note: Note<'a>,
    pub key: NoteKey,
}

impl<'a> NotePayload<'a> {
    pub fn noteref(&self) -> NoteRef {
        NoteRef {
            key: self.key,
            created_at: self.note.created_at(),
        }
    }
}

fn to_fragment<'a>(
    payload: &'a NotePayload,
    ndb: &Ndb,
    txn: &Transaction,
) -> Option<NoteUnitFragmentResponse<'a>> {
    match payload.note.kind() {
        1 => Some(NoteUnitFragmentResponse {
            fragment: NoteUnitFragment::Single(NoteRef {
                key: payload.key,
                created_at: payload.note.created_at(),
            }),
            unknown_pk: None,
        }),
        7 => to_reaction(payload, ndb, txn).map(|r| NoteUnitFragmentResponse {
            fragment: NoteUnitFragment::Composite(CompositeFragment::Reaction(r.fragment)),
            unknown_pk: Some(r.pk),
        }),
        6 => to_repost(payload, ndb, txn).map(RepostResponse::into),
        _ => None,
    }
}

fn to_reaction<'a>(
    payload: &'a NotePayload,
    ndb: &Ndb,
    txn: &Transaction,
) -> Option<ReactionResponse<'a>> {
    let reaction = payload.note.content();

    let mut note_reacted_to = None;

    for tag in payload.note.tags() {
        if tag.count() < 2 {
            continue;
        }

        let Some("e") = tag.get_str(0) else {
            continue;
        };

        let Some(react_to_id) = tag.get_id(1) else {
            continue;
        };

        note_reacted_to = Some(react_to_id);
    }

    let reacted_to_noteid = note_reacted_to?;

    let reaction_note_ref = payload.noteref();

    let reacted_to_note = ndb.get_note_by_id(txn, reacted_to_noteid).ok()?;

    let noteref_reacted_to = NoteRef {
        key: reacted_to_note.key()?,
        created_at: reacted_to_note.created_at(),
    };

    let sender_profilekey = ndb
        .get_profile_by_pubkey(txn, payload.note.pubkey())
        .ok()
        .and_then(|p| p.key());

    Some(ReactionResponse {
        fragment: ReactionFragment {
            noteref_reacted_to,
            reaction_note_ref,
            reaction: Reaction {
                reaction: reaction.to_string(),
                sender: Pubkey::new(*payload.note.pubkey()),
                sender_profilekey,
            },
        },
        pk: payload.note.pubkey(),
    })
}

pub struct ReactionResponse<'a> {
    fragment: ReactionFragment,
    pk: &'a [u8; 32], // reaction sender
}

pub struct RepostResponse<'a> {
    fragment: RepostFragment,
    reposter_pk: &'a [u8; 32],
}

impl<'a> From<RepostResponse<'a>> for NoteUnitFragmentResponse<'a> {
    fn from(value: RepostResponse<'a>) -> Self {
        Self {
            fragment: NoteUnitFragment::Composite(CompositeFragment::Repost(value.fragment)),
            unknown_pk: Some(value.reposter_pk),
        }
    }
}

fn to_repost<'a>(
    payload: &'a NotePayload,
    ndb: &Ndb,
    txn: &Transaction,
) -> Option<RepostResponse<'a>> {
    let reposted_note = match get_reposted_note(ndb, txn, &payload.note) {
        Some(r) => r,
        None => {
            tracing::debug!(
                "Could not get reposted note for note id {}",
                enostr::NoteId::new(*payload.note.id()).hex()
            );
            return None;
        }
    };

    let reposted_key = match reposted_note.key() {
        Some(r) => r,
        None => {
            tracing::error!(
                "Could not get key of reposted note {}",
                enostr::NoteId::new(*reposted_note.id()).hex()
            );
            return None;
        }
    };

    Some(RepostResponse {
        fragment: RepostFragment {
            reposted_noteref: NoteRef {
                key: reposted_key,
                created_at: reposted_note.created_at(),
            },
            repost_noteref: payload.noteref(),
            reposter: Pubkey::new(*payload.note.pubkey()),
        },
        reposter_pk: payload.note.pubkey(),
    })
}
