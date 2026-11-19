use nostrdb::{Ndb, Note, NoteBuildOptions, NoteBuilder, Transaction};
use nostrdb_net::{FilledKeypair, FullKeypair, ProfileState, Pubkey};

use notedeck::{
    contact_list_change, note::publish::publish_note_builder, send_mute_event, Accounts,
    ContactState, DataPath, Localization, NoteSender, ProfileContext, PublishApi, RemoteApi,
    SentNote,
};

use crate::{column::Column, nav::RouterAction, route::Route, storage, Damus};

pub struct SaveProfileChanges {
    pub kp: FullKeypair,
    pub state: ProfileState,
}

impl SaveProfileChanges {
    pub fn new(kp: FullKeypair, state: ProfileState) -> Self {
        Self { kp, state }
    }
    pub fn to_note(&self) -> Note<'_> {
        let sec = &self.kp.secret_key.to_secret_bytes();
        add_client_tag(NoteBuilder::new())
            .kind(0)
            .content(&self.state.to_json())
            .options(NoteBuildOptions::default().created_at(true).sign(sec))
            .build()
            .expect("should build")
    }
}

fn add_client_tag(builder: NoteBuilder<'_>) -> NoteBuilder<'_> {
    builder
        .start_tag()
        .tag_str("client")
        .tag_str("Damus Notedeck")
}

pub enum ProfileAction {
    Edit(FullKeypair),
    SaveChanges(SaveProfileChanges),
    Follow(Pubkey),
    Unfollow(Pubkey),
    Context(ProfileContext),
}

pub enum ProfileActionResponse {
    Router(RouterAction),
    ViewAsAccount(Pubkey),
}

impl From<RouterAction> for ProfileActionResponse {
    fn from(action: RouterAction) -> Self {
        Self::Router(action)
    }
}

impl ProfileAction {
    #[allow(clippy::too_many_arguments)]
    pub fn process_profile_action(
        &self,
        app: &mut Damus,
        path: &DataPath,
        i18n: &mut Localization,
        ctx: &egui::Context,
        ndb: &Ndb,
        remote: &mut RemoteApi<'_>,
        accounts: &Accounts,
    ) -> Option<ProfileActionResponse> {
        match self {
            ProfileAction::Edit(kp) => {
                Some(RouterAction::route_to(Route::EditProfile(kp.pubkey)).into())
            }
            ProfileAction::SaveChanges(changes) => {
                let note = changes.to_note();
                let sent = NoteSender::from_app(ndb, accounts, remote)
                    .and_then(|mut sender| sender.send_note(&note));
                if let Err(err) = sent {
                    tracing::error!("could not send the profile: {err}");
                    return None;
                }
                Some(RouterAction::GoBack.into())
            }
            ProfileAction::Follow(target_key) => {
                if let Err(err) = send_kind_3_event(ndb, accounts, remote, target_key, true) {
                    tracing::error!("could not follow {}: {err}", target_key.hex());
                }
                None
            }
            ProfileAction::Unfollow(target_key) => {
                if let Err(err) = send_kind_3_event(ndb, accounts, remote, target_key, false) {
                    tracing::error!("could not unfollow {}: {err}", target_key.hex());
                }
                None
            }
            ProfileAction::Context(profile_context) => {
                use notedeck::ProfileContextSelection;
                match &profile_context.selection {
                    ProfileContextSelection::ViewAs => Some(ProfileActionResponse::ViewAsAccount(
                        profile_context.profile,
                    )),
                    ProfileContextSelection::AddProfileColumn => {
                        let timeline_route = Route::Timeline(
                            crate::timeline::TimelineKind::Profile(profile_context.profile),
                        );

                        let missing_column = {
                            let deck_columns = app.columns(accounts).columns();
                            let router_head = std::slice::from_ref(&timeline_route);
                            !deck_columns
                                .iter()
                                .any(|column| column.router.routes().starts_with(router_head))
                        };

                        if missing_column {
                            let column = Column::new(vec![timeline_route]);

                            app.columns_mut(i18n, accounts).add_column(column);

                            storage::save_decks_cache(path, &app.decks_cache);
                        }

                        None
                    }
                    ProfileContextSelection::MuteUser => {
                        let kp = accounts.get_selected_account().key.to_full()?;
                        let muted = accounts.mute();
                        let txn = Transaction::new(ndb).expect("txn");
                        let publisher = &mut remote.publisher();
                        if muted.is_pk_muted(profile_context.profile.bytes()) {
                            notedeck::send_unmute_event(
                                ndb,
                                &txn,
                                publisher,
                                kp,
                                &muted,
                                &profile_context.profile,
                            );
                        } else {
                            send_mute_event(
                                ndb,
                                &txn,
                                publisher,
                                kp,
                                &muted,
                                &profile_context.profile,
                            );
                        }
                        None
                    }
                    ProfileContextSelection::ReportUser => {
                        let target = notedeck::ReportTarget {
                            pubkey: profile_context.profile,
                            note_id: None,
                        };
                        Some(
                            RouterAction::route_to_sheet(
                                Route::Report(target),
                                egui_nav::Split::AbsoluteFromBottom(340.0),
                            )
                            .into(),
                        )
                    }
                    _ => {
                        profile_context
                            .selection
                            .process(ctx, &profile_context.profile);
                        None
                    }
                }
            }
        }
    }
}

/// The profile's follow button: the selected account's contact list
/// ([`ContactState::Received`]) with `target` added when `follow` and dropped
/// when not ([`contact_list_change`]), sent through its [`NoteSender`]. The
/// `nostr_follow` tool sends the same rebuild.
pub(crate) fn send_kind_3_event(
    ndb: &Ndb,
    accounts: &Accounts,
    remote: &mut RemoteApi<'_>,
    target: &Pubkey,
    follow: bool,
) -> Result<SentNote, String> {
    let ContactState::Received { note_key, .. } =
        accounts.get_selected_account().data.contacts.get_state()
    else {
        return Err("the account's contact list hasn't arrived yet".to_owned());
    };

    let txn = Transaction::new(ndb).map_err(|e| format!("opening a db transaction: {e}"))?;
    let contacts = ndb
        .get_note_by_key(&txn, *note_key)
        .map_err(|_| "the contact list is received but its note key isn't in the db".to_owned())?;
    if contacts.kind() != 3 {
        return Err(format!(
            "the contact list's note key holds a kind {} note",
            contacts.kind()
        ));
    }

    NoteSender::from_app(ndb, accounts, remote)?
        .send(contact_list_change(&contacts, target, follow))
}

pub fn send_new_contact_list(
    kp: FilledKeypair,
    ndb: &Ndb,
    publisher: &mut PublishApi<'_>,
    mut pks_to_follow: Vec<Pubkey>,
) {
    if !pks_to_follow.contains(kp.pubkey) {
        pks_to_follow.push(*kp.pubkey);
    }

    let builder = construct_contact_list_note(pks_to_follow);

    publish_note_builder(builder, ndb, publisher, kp);
}

/// Builds a kind-3 contact-list note for the provided pubkeys.
fn construct_contact_list_note<'a>(pks: Vec<Pubkey>) -> NoteBuilder<'a> {
    let mut builder = NoteBuilder::new()
        .content("")
        .kind(3)
        .options(NoteBuildOptions::default());

    for pk in pks {
        builder = builder.start_tag().tag_str("p").tag_str(&pk.hex());
    }

    builder
}

pub fn send_default_dms_relay_list(
    kp: FilledKeypair<'_>,
    ndb: &Ndb,
    publisher: &mut PublishApi<'_>,
) {
    publish_note_builder(construct_default_dms_relay_list(), ndb, publisher, kp);
}

fn construct_default_dms_relay_list<'a>() -> NoteBuilder<'a> {
    let mut builder = NoteBuilder::new()
        .content("")
        .kind(10050)
        .options(NoteBuildOptions::default());

    for relay in default_dms_relays() {
        builder = builder.start_tag().tag_str("relay").tag_str(relay);
    }

    builder
}

fn default_dms_relays() -> Vec<&'static str> {
    vec!["wss://relay.damus.io", "wss://nos.lol"]
}

#[cfg(test)]
mod tests {
    use super::construct_contact_list_note;
    use nostrdb_net::FullKeypair;

    #[test]
    fn construct_contact_list_note_emits_expected_tags() {
        let owner = FullKeypair::generate();
        let first = FullKeypair::generate();
        let second = FullKeypair::generate();

        let note = construct_contact_list_note(vec![first.pubkey, second.pubkey])
            .sign(&owner.secret_key.secret_bytes())
            .build()
            .expect("contact list note");

        assert_eq!(note.kind(), 3);
        assert!(note.tags().into_iter().any(|tag| {
            tag.get_str(0) == Some("p") && tag.get_id(1) == Some(first.pubkey.bytes())
        }));
        assert!(note.tags().into_iter().any(|tag| {
            tag.get_str(0) == Some("p") && tag.get_id(1) == Some(second.pubkey.bytes())
        }));
    }
}
