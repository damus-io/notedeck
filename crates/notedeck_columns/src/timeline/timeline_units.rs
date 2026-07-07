use enostr::Pubkey;
use nostrdb::{Ndb, Note, NoteKey, Transaction};
use notedeck::NoteRef;
use notedeck_ui::note::get_reposted_note;

use crate::timeline::{
    note_units::{InsertManyResponse, NoteUnits},
    unit::{
        CompositeFragment, NoteUnit, NoteUnitFragment, Reaction, ReactionFragment, RepostFragment,
        ZapFragment, ZapInfo,
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
    #[profiling::function]
    pub fn merge_new_notes(
        &mut self,
        payloads: Vec<&NotePayload>,
        ndb: &Ndb,
        txn: &Transaction,
    ) -> MergeResponse {
        let new_fragments = payloads
            .into_iter()
            .filter_map(|p| to_fragment(p, ndb, txn))
            .collect();

        MergeResponse {
            insertion_response: self.units.merge_fragments(new_fragments),
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

pub struct MergeResponse {
    pub insertion_response: InsertManyResponse,
}

impl MergeResponse {
    pub fn empty() -> Self {
        Self {
            insertion_response: InsertManyResponse::Zero,
        }
    }
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

fn to_fragment(payload: &NotePayload, ndb: &Ndb, txn: &Transaction) -> Option<NoteUnitFragment> {
    match payload.note.kind() {
        1 => Some(NoteUnitFragment::Single(NoteRef {
            key: payload.key,
            created_at: payload.note.created_at(),
        })),
        7 => to_reaction(payload, ndb, txn)
            .map(|r| NoteUnitFragment::Composite(CompositeFragment::Reaction(r.fragment))),
        6 => to_repost(payload, ndb, txn).map(RepostResponse::into),
        9735 => to_zap(payload, ndb, txn).map(ZapResponse::into),
        _ => None,
    }
}

fn to_reaction(payload: &NotePayload, ndb: &Ndb, txn: &Transaction) -> Option<ReactionResponse> {
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
    })
}

pub struct ReactionResponse {
    fragment: ReactionFragment,
}

pub struct RepostResponse {
    fragment: RepostFragment,
}

impl From<RepostResponse> for NoteUnitFragment {
    fn from(value: RepostResponse) -> Self {
        NoteUnitFragment::Composite(CompositeFragment::Repost(value.fragment))
    }
}

fn to_repost(payload: &NotePayload, ndb: &Ndb, txn: &Transaction) -> Option<RepostResponse> {
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
    })
}

struct ZapResponse {
    fragment: ZapFragment,
}

impl From<ZapResponse> for NoteUnitFragment {
    fn from(value: ZapResponse) -> Self {
        NoteUnitFragment::Composite(CompositeFragment::Zap(value.fragment))
    }
}

fn to_zap(payload: &NotePayload, ndb: &Ndb, txn: &Transaction) -> Option<ZapResponse> {
    let mut note_zapped_id = None;
    let mut bolt11 = None;
    let mut description = None;

    for tag in payload.note.tags() {
        if tag.count() < 2 {
            continue;
        }

        let Some(tag_name) = tag.get_str(0) else {
            continue;
        };

        if tag_name == "e" {
            note_zapped_id = tag.get_id(1);
        } else if tag_name == "bolt11" {
            bolt11 = tag.get_str(1);
        } else if tag_name == "description" {
            description = tag.get_str(1);
        }

        if note_zapped_id.is_some() && bolt11.is_some() && description.is_some() {
            break;
        }
    }

    let note_zapped_id = note_zapped_id?;
    let bolt11_str = bolt11?;
    let description_str = description?;

    // Parse the zap request (description) to get the sender pubkey
    let zap_req = enostr::Note::from_json(description_str).ok()?;
    let sender_pk = *zap_req.pubkey.bytes();

    // Parse bolt11 invoice for amount
    let invoice: lightning_invoice::Bolt11Invoice = bolt11_str.parse().ok()?;
    let amount_msats = invoice.amount_milli_satoshis()?;

    // Look up the zapped note
    let zapped_note = ndb.get_note_by_id(txn, note_zapped_id).ok()?;
    let zapped_key = zapped_note.key()?;

    let sender_profilekey = ndb
        .get_profile_by_pubkey(txn, &sender_pk)
        .ok()
        .and_then(|p| p.key());

    Some(ZapResponse {
        fragment: ZapFragment {
            noteref_zapped: NoteRef {
                key: zapped_key,
                created_at: zapped_note.created_at(),
            },
            zap_note_ref: payload.noteref(),
            zap_info: ZapInfo {
                sender: Pubkey::new(sender_pk),
                sender_profilekey,
                amount_msats,
            },
        },
    })
}
