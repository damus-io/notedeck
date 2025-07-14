use nostrdb::{Filter, Ndb, Note, Transaction};

fn pk1_is_following_pk2(
    ndb: &Ndb,
    txn: &Transaction,
    pk1: &[u8; 32],
    pk2: &[u8; 32],
) -> Option<bool> {
    let note = get_contacts_note(ndb, txn, pk1)?;

    Some(note_follows(note, pk2))
}

pub fn trust_media_from_pk2(
    ndb: &Ndb,
    txn: &Transaction,
    pk1: Option<&[u8; 32]>,
    pk2: &[u8; 32],
) -> bool {
    pk1.map(|pk| pk == pk2 || pk1_is_following_pk2(ndb, txn, pk, pk2).unwrap_or(false))
        .unwrap_or(false)
}

fn get_contacts_note<'a>(ndb: &'a Ndb, txn: &'a Transaction, user: &[u8; 32]) -> Option<Note<'a>> {
    Some(
        ndb.query(txn, &[contacts_filter(user)], 1)
            .ok()?
            .first()?
            .note
            .clone(),
    )
}

pub fn contacts_filter(pk: &[u8; 32]) -> Filter {
    Filter::new().authors([pk]).kinds([3]).limit(1).build()
}

fn note_follows(contacts_note: Note<'_>, pk: &[u8; 32]) -> bool {
    for tag in contacts_note.tags() {
        if tag.count() < 2 {
            continue;
        }

        let Some("p") = tag.get_str(0) else {
            continue;
        };

        let Some(author) = tag.get_id(1) else {
            continue;
        };

        if pk == author {
            return true;
        }
    }

    false
}
