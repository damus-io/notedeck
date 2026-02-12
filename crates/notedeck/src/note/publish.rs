use enostr::{FilledKeypair, Pubkey, RelayPool};
use nostrdb::{Filter, Ndb, Note, NoteBuildOptions, NoteBuilder, Transaction};
use tracing::info;

use crate::Muted;

pub fn builder_from_note<F>(note: Note<'_>, skip_tag: Option<F>) -> NoteBuilder<'_>
where
    F: Fn(&nostrdb::Tag<'_>) -> bool,
{
    let mut builder = NoteBuilder::new();

    builder = builder.content(note.content());
    builder = builder.options(NoteBuildOptions::default());
    builder = builder.kind(note.kind());
    builder = builder.pubkey(note.pubkey());

    for tag in note.tags() {
        if let Some(skip) = &skip_tag {
            if skip(&tag) {
                continue;
            }
        }

        builder = builder.start_tag();
        for tag_item in tag {
            builder = match tag_item.variant() {
                nostrdb::NdbStrVariant::Id(i) => builder.tag_id(i),
                nostrdb::NdbStrVariant::Str(s) => builder.tag_str(s),
            };
        }
    }

    builder
}

pub fn send_note_builder(builder: NoteBuilder, ndb: &Ndb, pool: &mut RelayPool, kp: FilledKeypair) {
    let note = builder
        .sign(&kp.secret_key.secret_bytes())
        .build()
        .expect("build note");

    let Ok(event) = &enostr::ClientMessage::event(&note) else {
        tracing::error!("send_note_builder: failed to build json");
        return;
    };

    let Ok(json) = event.to_json() else {
        tracing::error!("send_note_builder: failed to build json");
        return;
    };

    let _ = ndb.process_event_with(&json, nostrdb::IngestMetadata::new().client(true));
    info!("sending {}", &json);
    pool.send(event);
}

pub fn send_unmute_event(
    ndb: &Ndb,
    txn: &Transaction,
    pool: &mut RelayPool,
    kp: FilledKeypair,
    muted: &Muted,
    target: &Pubkey,
) {
    if !muted.is_pk_muted(target.bytes()) {
        tracing::info!("pubkey {} is not muted, nothing to unmute", target.hex());
        return;
    }

    let filter = Filter::new()
        .authors([kp.pubkey.bytes()])
        .kinds([10000])
        .limit(1)
        .build();

    let lim = filter.limit().unwrap_or(crate::filter::default_limit()) as i32;

    let Some(existing_note) = ndb
        .query(txn, std::slice::from_ref(&filter), lim)
        .ok()
        .and_then(|results| results.first().map(|qr| qr.note_key))
        .and_then(|nk| ndb.get_note_by_key(txn, nk).ok())
    else {
        tracing::warn!("no existing kind 10000 mute list found, nothing to unmute from");
        return;
    };

    let target_bytes = target.bytes();
    let builder = builder_from_note(
        existing_note,
        Some(|tag: &nostrdb::Tag<'_>| {
            if tag.count() < 2 {
                return false;
            }
            let Some("p") = tag.get_str(0) else {
                return false;
            };
            let Some(val) = tag.get_id(1) else {
                return false;
            };
            val == target_bytes
        }),
    );

    send_note_builder(builder, ndb, pool, kp);
}

pub fn send_mute_event(
    ndb: &Ndb,
    txn: &Transaction,
    pool: &mut RelayPool,
    kp: FilledKeypair,
    muted: &Muted,
    target: &Pubkey,
) {
    if muted.is_pk_muted(target.bytes()) {
        tracing::info!("pubkey {} is already muted", target.hex());
        return;
    }

    // Query for the existing mute list (kind 10000)
    let filter = Filter::new()
        .authors([kp.pubkey.bytes()])
        .kinds([10000])
        .limit(1)
        .build();

    let lim = filter.limit().unwrap_or(crate::filter::default_limit()) as i32;

    let existing_note = ndb
        .query(txn, std::slice::from_ref(&filter), lim)
        .ok()
        .and_then(|results| results.first().map(|qr| qr.note_key))
        .and_then(|nk| ndb.get_note_by_key(txn, nk).ok());

    let builder = if let Some(note) = existing_note {
        // Append new "p" tag to existing mute list
        builder_from_note(note, None::<fn(&nostrdb::Tag<'_>) -> bool>)
            .start_tag()
            .tag_str("p")
            .tag_str(&target.hex())
    } else {
        // Create a fresh mute list
        NoteBuilder::new()
            .content("")
            .kind(10000)
            .options(NoteBuildOptions::default())
            .start_tag()
            .tag_str("p")
            .tag_str(&target.hex())
    };

    send_note_builder(builder, ndb, pool, kp);
}
