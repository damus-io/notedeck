//! Building and sending the notes a user's own actions write: a post, a
//! reaction, a follow, a mute. [`NoteSender`] is the one path every such note
//! leaves by, whether a Columns button or a nostr act tool raised it.

use enostr::RelayId;
use nostrdb::{Filter, IngestMetadata, Ndb, Note, NoteBuildOptions, NoteBuilder, Transaction};
use nostrdb_net::{ClientMessage, FilledKeypair, NoteId, Pubkey};
use tracing::info;

use crate::{Accounts, ExplicitPublishApi, Muted, PublishApi, RemoteApi, ToolContext};

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ReportType {
    Spam,
    Nudity,
    Profanity,
    Illegal,
    Impersonation,
    Other,
}

impl ReportType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReportType::Spam => "spam",
            ReportType::Nudity => "nudity",
            ReportType::Profanity => "profanity",
            ReportType::Illegal => "illegal",
            ReportType::Impersonation => "impersonation",
            ReportType::Other => "other",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ReportType::Spam => "Spam",
            ReportType::Nudity => "Nudity",
            ReportType::Profanity => "Profanity",
            ReportType::Illegal => "Illegal",
            ReportType::Impersonation => "Impersonation",
            ReportType::Other => "Other",
        }
    }

    pub const ALL: &'static [ReportType] = &[
        ReportType::Spam,
        ReportType::Nudity,
        ReportType::Profanity,
        ReportType::Illegal,
        ReportType::Impersonation,
        ReportType::Other,
    ];
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct ReportTarget {
    pub pubkey: Pubkey,
    pub note_id: Option<NoteId>,
}

/// A builder holding `note`'s kind, content and tags (but not its id,
/// signature or `created_at`), minus any tag `skip_tag` matches: the start of
/// a replaceable note's next revision. The builder copies every string, so it
/// outlives `note`.
pub fn builder_from_note<'b, F>(note: &Note<'_>, skip_tag: Option<F>) -> NoteBuilder<'b>
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

/// What a [`NoteSender`] sent: the note it wrote and where it went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SentNote {
    /// The new note's id.
    pub id: NoteId,
    /// Its kind.
    pub kind: u32,
    /// How many relays it was published to. 0 means it exists only in this
    /// notedeck's db until the account has a write relay.
    pub relays: usize,
}

/// The one path a note the user writes leaves by: sign it as the account,
/// ingest it locally so it shows at once, then publish it to the account's
/// write relays. The Columns buttons build one with [`from_app`](Self::from_app)
/// and the nostr act tools with [`from_tool_context`](Self::from_tool_context),
/// so a like from the button and a like from an agent are the same bytes to
/// the same relays.
pub struct NoteSender<'a> {
    ndb: &'a Ndb,
    signer: FilledKeypair<'a>,
    relays: Vec<RelayId>,
    publish: ExplicitPublishApi<'a>,
}

impl<'a> NoteSender<'a> {
    /// A sender signing with `signer` and publishing to `relays` through
    /// `publish`. Prefer [`from_app`](Self::from_app) or
    /// [`from_tool_context`](Self::from_tool_context), which take both from
    /// the selected account.
    pub fn new(
        ndb: &'a Ndb,
        signer: FilledKeypair<'a>,
        relays: Vec<RelayId>,
        publish: ExplicitPublishApi<'a>,
    ) -> Self {
        Self {
            ndb,
            signer,
            relays,
            publish,
        }
    }

    /// A sender for the selected account, publishing through `remote` to its
    /// write relays: what an app's own buttons send with. An error when the
    /// account is watch-only.
    pub fn from_app(
        ndb: &'a Ndb,
        accounts: &'a Accounts,
        remote: &'a mut RemoteApi<'_>,
    ) -> Result<Self, String> {
        let signer = selected_signer(accounts)?;
        let relays = accounts.selected_account_write_relays();
        Ok(Self::new(ndb, signer, relays, remote.publisher_explicit()))
    }

    /// A sender for `cx`'s selected account, publishing through its
    /// publisher to the account's write relays: what a tool that acts as the
    /// user sends with. An error when the account is watch-only or `cx` is
    /// read-only (it has no publisher).
    pub fn from_tool_context(cx: &'a mut ToolContext<'_>) -> Result<Self, String> {
        let signer = selected_signer(cx.accounts)?;
        let relays = cx.accounts.selected_account_write_relays();
        let publish = cx
            .remote
            .as_mut()
            .ok_or("publishing is unavailable in this context")?
            .publisher();
        Ok(Self::new(cx.ndb, signer, relays, publish))
    }

    /// The key this sender signs with, for a caller that builds and signs a
    /// note itself (a post, which [`send_note`](Self::send_note) then sends).
    pub fn signer(&self) -> FilledKeypair<'a> {
        self.signer
    }

    /// Sign `builder` as the account, then [`send_note`](Self::send_note) it.
    pub fn send(&mut self, builder: NoteBuilder<'_>) -> Result<SentNote, String> {
        let note = sign(builder, self.signer)?;
        self.send_note(&note)
    }

    /// Ingest the already-signed `note` locally, so it shows at once, then
    /// publish it to this sender's relays.
    pub fn send_note(&mut self, note: &Note) -> Result<SentNote, String> {
        store(self.ndb, note)?;
        self.publish.publish_note(note, self.relays.clone());
        Ok(SentNote {
            id: NoteId::new(*note.id()),
            kind: note.kind(),
            relays: self.relays.len(),
        })
    }
}

/// The selected account's keypair, or the reason it can't sign.
fn selected_signer(accounts: &Accounts) -> Result<FilledKeypair<'_>, String> {
    accounts
        .selected_filled()
        .ok_or_else(|| "the selected account is watch-only and can't sign".to_owned())
}

/// `builder` signed with `kp`.
fn sign(builder: NoteBuilder<'_>, kp: FilledKeypair<'_>) -> Result<Note<'static>, String> {
    let secret = kp.secret_key.secret_bytes();
    builder
        .sign(&secret)
        .build()
        .ok_or_else(|| "failed to build the note".to_owned())
}

/// Ingest the signed `note` into `ndb` as the client's own, so it shows
/// before any relay echoes it back.
fn store(ndb: &Ndb, note: &Note) -> Result<(), String> {
    let json = ClientMessage::event(note)
        .and_then(|event| event.to_json())
        .map_err(|e| format!("serializing the note: {e}"))?;
    ndb.process_event_with(&json, IngestMetadata::new().client(true))
        .map_err(|e| format!("storing the note: {e}"))?;
    info!("sending {json}");
    Ok(())
}

/// Sign `builder` with `kp`, ingest it, and publish it to the *selected*
/// account's write relays, whoever `kp` is. The senders still on it (mute,
/// report, people list, and onboarding's first contact list, signed by an
/// account not yet selected) keep that target; a note the selected account
/// writes goes through [`NoteSender`]. A note that won't build or store is
/// logged and dropped.
pub fn publish_note_builder(
    builder: NoteBuilder,
    ndb: &Ndb,
    publisher: &mut PublishApi<'_>,
    kp: FilledKeypair,
) {
    let note = match sign(builder, kp).and_then(|note| store(ndb, &note).map(|()| note)) {
        Ok(note) => note,
        Err(err) => {
            tracing::error!("publish_note_builder: {err}");
            return;
        }
    };
    publisher.accounts_write().publish_note(&note);
}

/// The next revision of the contact list `contacts`, with `target`'s `p` tag
/// appended when `follow` and dropped when not; every other tag and the
/// content (some clients keep relays there) as they were. The one kind-3
/// rebuild the profile's follow button and the `nostr_follow` tool share.
pub fn contact_list_change<'b>(contacts: &Note, target: &Pubkey, follow: bool) -> NoteBuilder<'b> {
    let target_bytes = target.bytes();
    let is_target = |tag: &nostrdb::Tag<'_>| {
        tag.count() >= 2 && tag.get_str(0) == Some("p") && tag.get_id(1) == Some(target_bytes)
    };
    let builder = builder_from_note(contacts, (!follow).then_some(is_target));
    if !follow {
        return builder;
    }
    builder.start_tag().tag_str("p").tag_id(target_bytes)
}

/// A NIP-25 reaction of `content` (`+`, `-` or an emoji) to `target`,
/// unsigned: the note the like button sends, and the `nostr_react` tool too.
/// It tags the target note and its author (with a relay hint when the db
/// knows where the note came from), its `a` coordinate when it is
/// addressable, and its kind.
pub fn reaction_note<'b>(txn: &Transaction, target: &Note<'_>, content: &str) -> NoteBuilder<'b> {
    let target_pubkey = Pubkey::new(*target.pubkey());
    let relay_hint = target.relays(txn).next();
    let target_kind = target.kind();

    let mut builder = NoteBuilder::new()
        .kind(7)
        .content(content)
        .start_tag()
        .tag_str("e")
        .tag_id(target.id())
        .tag_str(relay_hint.unwrap_or(""))
        .tag_str(&target_pubkey.hex())
        .start_tag()
        .tag_str("p")
        .tag_id(target_pubkey.bytes());

    if let Some(relay) = relay_hint {
        builder = builder.tag_str(relay);
    }

    if let Some(d_value) = addressable_d_tag(target) {
        let coordinates = format!("{}:{}:{}", target_kind, target_pubkey.hex(), d_value);
        builder = builder.start_tag().tag_str("a").tag_str(&coordinates);
        if let Some(relay) = relay_hint {
            builder = builder.tag_str(relay);
        }
    }

    builder
        .start_tag()
        .tag_str("k")
        .tag_str(&target_kind.to_string())
}

/// `note`'s first `d` tag value, which makes it addressable.
fn addressable_d_tag<'n>(note: &Note<'n>) -> Option<&'n str> {
    note.tags()
        .into_iter()
        .find(|tag| tag.count() >= 2 && tag.get_str(0) == Some("d"))
        .and_then(|tag| tag.get_str(1))
}

pub fn send_unmute_event(
    ndb: &Ndb,
    txn: &Transaction,
    publisher: &mut PublishApi<'_>,
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
        &existing_note,
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

    publish_note_builder(builder, ndb, publisher, kp);
}

pub fn send_mute_event(
    ndb: &Ndb,
    txn: &Transaction,
    publisher: &mut PublishApi<'_>,
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
        builder_from_note(&note, None::<fn(&nostrdb::Tag<'_>) -> bool>)
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

    publish_note_builder(builder, ndb, publisher, kp);
}

pub fn send_people_list_event(
    ndb: &Ndb,
    publisher: &mut PublishApi<'_>,
    kp: FilledKeypair,
    name: &str,
    members: &[Pubkey],
) {
    let builder = construct_people_list_note(name, members);

    publish_note_builder(builder, ndb, publisher, kp);
}

/// Construct a kind-30000 NIP-51 people-list note builder with one identifier,
/// title, and one `p` tag per member.
pub fn construct_people_list_note<'a>(name: &str, members: &[Pubkey]) -> NoteBuilder<'a> {
    let mut builder = NoteBuilder::new()
        .content("")
        .kind(30000)
        .options(NoteBuildOptions::default())
        .start_tag()
        .tag_str("d")
        .tag_str(name)
        .start_tag()
        .tag_str("title")
        .tag_str(name);

    for pk in members {
        builder = builder.start_tag().tag_str("p").tag_str(&pk.hex());
    }

    builder
}

pub fn send_report_event(
    ndb: &Ndb,
    publisher: &mut PublishApi<'_>,
    kp: FilledKeypair,
    target: &ReportTarget,
    report_type: ReportType,
) {
    let report_str = report_type.as_str();

    let mut builder = NoteBuilder::new()
        .content("")
        .kind(1984)
        .options(NoteBuildOptions::default())
        .start_tag()
        .tag_str("p")
        .tag_str(&target.pubkey.hex())
        .tag_str(report_str);

    if let Some(note_id) = &target.note_id {
        builder = builder
            .start_tag()
            .tag_str("e")
            .tag_str(&note_id.hex())
            .tag_str(report_str);
    }

    publish_note_builder(builder, ndb, publisher, kp);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_data::{RemoteIntent, RemoteIntentBatchBuilder, RemotePublishCommand};
    use crate::test_util::TempNdb;
    use enostr::NormRelayUrl;
    use nostrdb_net::FullKeypair;
    use std::time::Duration;

    /// The `p` values of `note`'s `p` tags, in order.
    fn p_tags(note: &Note) -> Vec<[u8; 32]> {
        note.tags()
            .into_iter()
            .filter(|tag| tag.get_str(0) == Some("p"))
            .filter_map(|tag| tag.get_id(1).copied())
            .collect()
    }

    /// A kind-3 contact list by `owner` following `follows`, with a relay
    /// map in its content and a non-`p` tag, as some clients write one.
    fn contact_list(owner: &FullKeypair, follows: &[&Pubkey]) -> Note<'static> {
        let mut builder = NoteBuilder::new()
            .kind(3)
            .content(r#"{"wss://relay.example.com":{"read":true,"write":true}}"#)
            .start_tag()
            .tag_str("t")
            .tag_str("kept");
        for pk in follows {
            builder = builder.start_tag().tag_str("p").tag_id(pk.bytes());
        }
        builder
            .sign(&owner.secret_key.secret_bytes())
            .build()
            .expect("contact list")
    }

    /// A follow appends the target's `p` tag; an unfollow drops only the
    /// target's. The content and every other tag are kept either way.
    #[test]
    fn contact_list_change_adds_or_drops_only_the_target() {
        let owner = FullKeypair::generate();
        let bob = FullKeypair::generate().pubkey;
        let carol = FullKeypair::generate().pubkey;
        let list = contact_list(&owner, &[&bob, &carol]);
        let secret = owner.secret_key.secret_bytes();

        let dave = FullKeypair::generate().pubkey;
        let followed = contact_list_change(&list, &dave, true)
            .sign(&secret)
            .build()
            .expect("follow");
        assert_eq!(followed.kind(), 3);
        assert_eq!(followed.content(), list.content());
        assert_eq!(
            p_tags(&followed),
            [*bob.bytes(), *carol.bytes(), *dave.bytes()]
        );

        let unfollowed = contact_list_change(&list, &bob, false)
            .sign(&secret)
            .build()
            .expect("unfollow");
        assert_eq!(unfollowed.content(), list.content());
        assert_eq!(p_tags(&unfollowed), [*carol.bytes()]);
        assert!(unfollowed
            .tags()
            .into_iter()
            .any(|tag| tag.get_str(0) == Some("t") && tag.get_str(1) == Some("kept")));
    }

    /// A reaction tags the note, its author and its kind, and carries the
    /// reaction as its content.
    #[test]
    fn reaction_note_tags_the_note_its_author_and_kind() {
        let TempNdb { dir: _dir, ndb } = TempNdb::new();
        let author = FullKeypair::generate();
        let target = NoteBuilder::new()
            .kind(1)
            .content("gm")
            .sign(&author.secret_key.secret_bytes())
            .build()
            .expect("target");
        let txn = Transaction::new(&ndb).expect("txn");
        let reactor = FullKeypair::generate();
        let reaction = reaction_note(&txn, &target, "🤙")
            .sign(&reactor.secret_key.secret_bytes())
            .build()
            .expect("reaction");

        assert_eq!(reaction.kind(), 7);
        assert_eq!(reaction.content(), "🤙");
        let tag = |name: &str| {
            reaction
                .tags()
                .into_iter()
                .find(|tag| tag.get_str(0) == Some(name))
                .expect(name)
        };
        assert_eq!(tag("e").get_id(1), Some(target.id()));
        assert_eq!(tag("p").get_id(1), Some(author.pubkey.bytes()));
        assert_eq!(tag("k").get_str(1), Some("1"));
    }

    /// A send signs as the sender's account, stores the note, publishes it
    /// to exactly the sender's relays, and reports how many there were.
    #[tokio::test]
    async fn send_signs_stores_and_publishes_to_its_relays() {
        let TempNdb { dir: _dir, ndb } = TempNdb::new();
        let account = FullKeypair::generate();
        let relays: Vec<RelayId> = ["wss://one.example.com", "wss://two.example.com"]
            .into_iter()
            .map(|url| RelayId::Websocket(NormRelayUrl::new(url).expect("relay")))
            .collect();
        let mut batch = RemoteIntentBatchBuilder::new();
        let stored = ndb
            .subscribe(&[Filter::new().authors([account.pubkey.bytes()]).build()])
            .expect("subscribe");

        let sent = {
            let publish = ExplicitPublishApi::new(&mut batch);
            let mut sender = NoteSender::new(&ndb, account.to_filled(), relays.clone(), publish);
            sender
                .send(NoteBuilder::new().kind(1).content("gm"))
                .expect("send")
        };

        assert_eq!(sent.kind, 1);
        assert_eq!(sent.relays, 2);
        let keys = ndb
            .wait_for_all_notes_within(stored, 1, Duration::from_secs(10))
            .await
            .expect("the sent note is stored");
        let txn = Transaction::new(&ndb).expect("txn");
        let note = ndb.get_note_by_key(&txn, keys[0]).expect("note");
        assert_eq!(note.id(), sent.id.bytes());
        assert_eq!(note.pubkey(), account.pubkey.bytes());

        let batch = batch.take().expect("a publish");
        let intents: Vec<_> = batch.sections().iter().flat_map(|s| s.intents()).collect();
        let [RemoteIntent::Publish(RemotePublishCommand::Explicit { msg, relays: to })] =
            intents.as_slice()
        else {
            panic!("expected one explicit publish");
        };
        assert_eq!(to, &relays);
        assert!(msg.note_json.contains(&sent.id.hex()));
    }

    #[test]
    fn construct_people_list_note_emits_expected_nip51_tags() {
        let owner = FullKeypair::generate();
        let member = FullKeypair::generate();

        let note = construct_people_list_note("friends", &[member.pubkey])
            .sign(&owner.secret_key.secret_bytes())
            .build()
            .expect("people list note");

        assert_eq!(note.kind(), 30000);
        assert!(note
            .tags()
            .into_iter()
            .any(|tag| tag.get_str(0) == Some("d") && tag.get_str(1) == Some("friends")));
        assert!(note
            .tags()
            .into_iter()
            .any(|tag| tag.get_str(0) == Some("title") && tag.get_str(1) == Some("friends")));
        assert!(note.tags().into_iter().any(|tag| {
            tag.get_str(0) == Some("p") && tag.get_id(1) == Some(member.pubkey.bytes())
        }));
    }
}
