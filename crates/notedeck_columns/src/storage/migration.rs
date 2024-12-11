use enostr::{NoteId, Pubkey};
use nostrdb::Ndb;
use serde::{Deserialize, Deserializer};
use tracing::error;

use crate::{
    accounts::AccountsRoute,
    column::{Columns, IntermediaryRoute},
    route::Route,
    timeline::{kind::ListKind, PubkeySource, Timeline, TimelineId, TimelineKind, TimelineRoute},
    ui::add_column::AddColumnRoute,
    Result,
};

use notedeck::{DataPath, DataPathType, Directory};

pub static COLUMNS_FILE: &str = "columns.json";

fn columns_json(path: &DataPath) -> Option<String> {
    let data_path = path.path(DataPathType::Setting);
    Directory::new(data_path)
        .get_file(COLUMNS_FILE.to_string())
        .ok()
}

#[derive(Deserialize, Debug, PartialEq)]
enum MigrationTimelineRoute {
    Timeline(u32),
    Thread(String),
    Profile(String),
    Reply(String),
    Quote(String),
}

impl MigrationTimelineRoute {
    fn timeline_route(self) -> Option<TimelineRoute> {
        match self {
            MigrationTimelineRoute::Timeline(id) => {
                Some(TimelineRoute::Timeline(TimelineId::new(id)))
            }
            MigrationTimelineRoute::Thread(note_id_hex) => {
                Some(TimelineRoute::Thread(NoteId::from_hex(&note_id_hex).ok()?))
            }
            MigrationTimelineRoute::Profile(pubkey_hex) => {
                Some(TimelineRoute::Profile(Pubkey::from_hex(&pubkey_hex).ok()?))
            }
            MigrationTimelineRoute::Reply(note_id_hex) => {
                Some(TimelineRoute::Reply(NoteId::from_hex(&note_id_hex).ok()?))
            }
            MigrationTimelineRoute::Quote(note_id_hex) => {
                Some(TimelineRoute::Quote(NoteId::from_hex(&note_id_hex).ok()?))
            }
        }
    }
}

#[derive(Deserialize, Debug, PartialEq)]
enum MigrationRoute {
    Timeline(MigrationTimelineRoute),
    Accounts(MigrationAccountsRoute),
    Relays,
    ComposeNote,
    AddColumn(MigrationAddColumnRoute),
    Support,
}

impl MigrationRoute {
    fn route(self) -> Option<Route> {
        match self {
            MigrationRoute::Timeline(migration_timeline_route) => {
                Some(Route::Timeline(migration_timeline_route.timeline_route()?))
            }
            MigrationRoute::Accounts(migration_accounts_route) => {
                Some(Route::Accounts(migration_accounts_route.accounts_route()))
            }
            MigrationRoute::Relays => Some(Route::Relays),
            MigrationRoute::ComposeNote => Some(Route::ComposeNote),
            MigrationRoute::AddColumn(migration_add_column_route) => Some(Route::AddColumn(
                migration_add_column_route.add_column_route(),
            )),
            MigrationRoute::Support => Some(Route::Support),
        }
    }
}

#[derive(Deserialize, Debug, PartialEq)]
enum MigrationAccountsRoute {
    Accounts,
    AddAccount,
}

impl MigrationAccountsRoute {
    fn accounts_route(self) -> AccountsRoute {
        match self {
            MigrationAccountsRoute::Accounts => AccountsRoute::Accounts,
            MigrationAccountsRoute::AddAccount => AccountsRoute::AddAccount,
        }
    }
}

#[derive(Deserialize, Debug, PartialEq)]
enum MigrationAddColumnRoute {
    Base,
    UndecidedNotification,
    ExternalNotification,
    Hashtag,
}

impl MigrationAddColumnRoute {
    fn add_column_route(self) -> AddColumnRoute {
        match self {
            MigrationAddColumnRoute::Base => AddColumnRoute::Base,
            MigrationAddColumnRoute::UndecidedNotification => AddColumnRoute::UndecidedNotification,
            MigrationAddColumnRoute::ExternalNotification => AddColumnRoute::ExternalNotification,
            MigrationAddColumnRoute::Hashtag => AddColumnRoute::Hashtag,
        }
    }
}

#[derive(Debug, PartialEq)]
struct MigrationColumn {
    routes: Vec<MigrationRoute>,
}

impl<'de> Deserialize<'de> for MigrationColumn {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let routes = Vec::<MigrationRoute>::deserialize(deserializer)?;

        Ok(MigrationColumn { routes })
    }
}

#[derive(Deserialize, Debug)]
struct MigrationColumns {
    columns: Vec<MigrationColumn>,
    timelines: Vec<MigrationTimeline>,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
struct MigrationTimeline {
    id: u32,
    kind: MigrationTimelineKind,
}

impl MigrationTimeline {
    fn into_timeline(self, ndb: &Ndb, deck_user_pubkey: Option<&[u8; 32]>) -> Option<Timeline> {
        self.kind
            .into_timeline_kind()?
            .into_timeline(ndb, deck_user_pubkey)
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
enum MigrationListKind {
    Contact(MigrationPubkeySource),
}

impl MigrationListKind {
    fn list_kind(self) -> Option<ListKind> {
        match self {
            MigrationListKind::Contact(migration_pubkey_source) => {
                Some(ListKind::Contact(migration_pubkey_source.pubkey_source()?))
            }
        }
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
enum MigrationPubkeySource {
    Explicit(String),
    DeckAuthor,
}

impl MigrationPubkeySource {
    fn pubkey_source(self) -> Option<PubkeySource> {
        match self {
            MigrationPubkeySource::Explicit(hex) => {
                Some(PubkeySource::Explicit(Pubkey::from_hex(hex.as_str()).ok()?))
            }
            MigrationPubkeySource::DeckAuthor => Some(PubkeySource::DeckAuthor),
        }
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
enum MigrationTimelineKind {
    List(MigrationListKind),
    Notifications(MigrationPubkeySource),
    Profile(MigrationPubkeySource),
    Universe,
    Generic,
    Hashtag(String),
}

impl MigrationTimelineKind {
    fn into_timeline_kind(self) -> Option<TimelineKind> {
        match self {
            MigrationTimelineKind::List(migration_list_kind) => {
                Some(TimelineKind::List(migration_list_kind.list_kind()?))
            }
            MigrationTimelineKind::Notifications(migration_pubkey_source) => Some(
                TimelineKind::Notifications(migration_pubkey_source.pubkey_source()?),
            ),
            MigrationTimelineKind::Profile(migration_pubkey_source) => Some(TimelineKind::Profile(
                migration_pubkey_source.pubkey_source()?,
            )),
            MigrationTimelineKind::Universe => Some(TimelineKind::Universe),
            MigrationTimelineKind::Generic => Some(TimelineKind::Generic),
            MigrationTimelineKind::Hashtag(hashtag) => Some(TimelineKind::Hashtag(hashtag)),
        }
    }
}

impl MigrationColumns {
    fn into_columns(self, ndb: &Ndb, deck_pubkey: Option<&[u8; 32]>) -> Columns {
        let mut columns = Columns::default();

        for column in self.columns {
            let mut cur_routes = Vec::new();
            for route in column.routes {
                match route {
                    MigrationRoute::Timeline(MigrationTimelineRoute::Timeline(timeline_id)) => {
                        if let Some(migration_tl) =
                            self.timelines.iter().find(|tl| tl.id == timeline_id)
                        {
                            let tl = migration_tl.clone().into_timeline(ndb, deck_pubkey);
                            if let Some(tl) = tl {
                                cur_routes.push(IntermediaryRoute::Timeline(tl));
                            } else {
                                error!("Problem deserializing timeline {:?}", migration_tl);
                            }
                        }
                    }
                    MigrationRoute::Timeline(MigrationTimelineRoute::Thread(_thread)) => {}
                    _ => {
                        if let Some(route) = route.route() {
                            cur_routes.push(IntermediaryRoute::Route(route));
                        }
                    }
                }
            }
            if !cur_routes.is_empty() {
                columns.insert_intermediary_routes(cur_routes);
            }
        }
        columns
    }
}

fn string_to_columns(
    serialized_columns: String,
    ndb: &Ndb,
    user: Option<&[u8; 32]>,
) -> Option<Columns> {
    Some(
        deserialize_columns_string(serialized_columns)
            .ok()?
            .into_columns(ndb, user),
    )
}

pub fn deserialize_columns(path: &DataPath, ndb: &Ndb, user: Option<&[u8; 32]>) -> Option<Columns> {
    string_to_columns(columns_json(path)?, ndb, user)
}

fn deserialize_columns_string(serialized_columns: String) -> Result<MigrationColumns> {
    Ok(
        serde_json::from_str::<MigrationColumns>(&serialized_columns)
            .map_err(notedeck::Error::Json)?,
    )
}

#[cfg(test)]
mod tests {
    use crate::storage::migration::{
        MigrationColumn, MigrationListKind, MigrationPubkeySource, MigrationRoute,
        MigrationTimeline, MigrationTimelineKind, MigrationTimelineRoute,
    };

    impl MigrationColumn {
        fn from_route(route: MigrationRoute) -> Self {
            Self {
                routes: vec![route],
            }
        }

        fn from_routes(routes: Vec<MigrationRoute>) -> Self {
            Self { routes }
        }
    }

    impl MigrationTimeline {
        fn new(id: u32, kind: MigrationTimelineKind) -> Self {
            Self { id, kind }
        }
    }

    use super::*;

    #[test]
    fn multi_column() {
        let route = r#"{"columns":[[{"Timeline":{"Timeline":2}}],[{"Timeline":{"Timeline":0}}],[{"Timeline":{"Timeline":1}}]],"timelines":[{"id":0,"kind":{"List":{"Contact":{"Explicit":"aa733081e4f0f79dd43023d8983265593f2b41a988671cfcef3f489b91ad93fe"}}}},{"id":1,"kind":{"Hashtag":"introductions"}},{"id":2,"kind":"Universe"}]}"#; // Multi-column

        let deserialized_columns = deserialize_columns_string(route.to_string());
        assert!(deserialized_columns.is_ok());

        let migration_cols = deserialized_columns.unwrap();

        assert_eq!(migration_cols.columns.len(), 3);
        assert_eq!(
            *migration_cols.columns.first().unwrap(),
            MigrationColumn::from_route(MigrationRoute::Timeline(
                MigrationTimelineRoute::Timeline(2)
            ))
        );

        assert_eq!(
            *migration_cols.columns.get(1).unwrap(),
            MigrationColumn::from_route(MigrationRoute::Timeline(
                MigrationTimelineRoute::Timeline(0)
            ))
        );

        assert_eq!(
            *migration_cols.columns.get(2).unwrap(),
            MigrationColumn::from_route(MigrationRoute::Timeline(
                MigrationTimelineRoute::Timeline(1)
            ))
        );

        assert_eq!(migration_cols.timelines.len(), 3);
        assert_eq!(
            *migration_cols.timelines.first().unwrap(),
            MigrationTimeline::new(
                0,
                MigrationTimelineKind::List(MigrationListKind::Contact(
                    MigrationPubkeySource::Explicit(
                        "aa733081e4f0f79dd43023d8983265593f2b41a988671cfcef3f489b91ad93fe"
                            .to_owned()
                    )
                ))
            )
        );
        assert_eq!(
            *migration_cols.timelines.get(1).unwrap(),
            MigrationTimeline::new(
                1,
                MigrationTimelineKind::Hashtag("introductions".to_owned())
            )
        );

        assert_eq!(
            *migration_cols.timelines.get(2).unwrap(),
            MigrationTimeline::new(2, MigrationTimelineKind::Universe)
        )
    }

    #[test]
    fn base() {
        let route = r#"{"columns":[[{"AddColumn":"Base"}]],"timelines":[]}"#;

        let deserialized_columns = deserialize_columns_string(route.to_string());
        assert!(deserialized_columns.is_ok());

        let migration_cols = deserialized_columns.unwrap();
        assert_eq!(migration_cols.columns.len(), 1);
        assert_eq!(
            *migration_cols.columns.first().unwrap(),
            MigrationColumn::from_route(MigrationRoute::AddColumn(MigrationAddColumnRoute::Base))
        );

        assert!(migration_cols.timelines.is_empty());
    }

    #[test]
    fn universe() {
        let route = r#"{"columns":[[{"Timeline":{"Timeline":0}}]],"timelines":[{"id":0,"kind":"Universe"}]}"#;
        let deserialized_columns = deserialize_columns_string(route.to_string());
        assert!(deserialized_columns.is_ok());

        let migration_cols = deserialized_columns.unwrap();
        assert_eq!(migration_cols.columns.len(), 1);
        assert_eq!(
            *migration_cols.columns.first().unwrap(),
            MigrationColumn::from_route(MigrationRoute::Timeline(
                MigrationTimelineRoute::Timeline(0)
            ))
        );

        assert_eq!(migration_cols.timelines.len(), 1);
        assert_eq!(
            *migration_cols.timelines.first().unwrap(),
            MigrationTimeline::new(0, MigrationTimelineKind::Universe)
        )
    }

    #[test]
    fn home() {
        let route = r#"{"columns":[[{"Timeline":{"Timeline":2}}]],"timelines":[{"id":2,"kind":{"List":{"Contact":{"Explicit":"aa733081e4f0f79dd43023d8983265593f2b41a988671cfcef3f489b91ad93fe"}}}}]}"#;

        let deserialized_columns = deserialize_columns_string(route.to_string());
        assert!(deserialized_columns.is_ok());

        let migration_cols = deserialized_columns.unwrap();
        assert_eq!(migration_cols.columns.len(), 1);
        assert_eq!(
            *migration_cols.columns.first().unwrap(),
            MigrationColumn::from_route(MigrationRoute::Timeline(
                MigrationTimelineRoute::Timeline(2)
            ))
        );

        assert_eq!(migration_cols.timelines.len(), 1);
        assert_eq!(
            *migration_cols.timelines.first().unwrap(),
            MigrationTimeline::new(
                2,
                MigrationTimelineKind::List(MigrationListKind::Contact(
                    MigrationPubkeySource::Explicit(
                        "aa733081e4f0f79dd43023d8983265593f2b41a988671cfcef3f489b91ad93fe"
                            .to_owned()
                    )
                ))
            )
        )
    }

    #[test]
    fn thread() {
        let route = r#"{"columns":[[{"Timeline":{"Timeline":7}},{"Timeline":{"Thread":"fb9b0c62bc91bbe28ca428fc85e310ae38795b94fb910e0f4e12962ced971f25"}}]],"timelines":[{"id":7,"kind":{"List":{"Contact":{"Explicit":"4a0510f26880d40e432f4865cb5714d9d3c200ca6ebb16b418ae6c555f574967"}}}}]}"#;

        let deserialized_columns = deserialize_columns_string(route.to_string());
        assert!(deserialized_columns.is_ok());

        let migration_cols = deserialized_columns.unwrap();
        assert_eq!(migration_cols.columns.len(), 1);
        assert_eq!(
            *migration_cols.columns.first().unwrap(),
            MigrationColumn::from_routes(vec![
                MigrationRoute::Timeline(MigrationTimelineRoute::Timeline(7),),
                MigrationRoute::Timeline(MigrationTimelineRoute::Thread(
                    "fb9b0c62bc91bbe28ca428fc85e310ae38795b94fb910e0f4e12962ced971f25".to_owned()
                )),
            ])
        );

        assert_eq!(migration_cols.timelines.len(), 1);
        assert_eq!(
            *migration_cols.timelines.first().unwrap(),
            MigrationTimeline::new(
                7,
                MigrationTimelineKind::List(MigrationListKind::Contact(
                    MigrationPubkeySource::Explicit(
                        "4a0510f26880d40e432f4865cb5714d9d3c200ca6ebb16b418ae6c555f574967"
                            .to_owned()
                    )
                ))
            )
        )
    }

    #[test]
    fn profile() {
        let route = r#"{"columns":[[{"Timeline":{"Timeline":7}},{"Timeline":{"Profile":"32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245"}}]],"timelines":[{"id":7,"kind":{"List":{"Contact":{"Explicit":"4a0510f26880d40e432f4865cb5714d9d3c200ca6ebb16b418ae6c555f574967"}}}}]}"#;

        let deserialized_columns = deserialize_columns_string(route.to_string());
        assert!(deserialized_columns.is_ok());

        let migration_cols = deserialized_columns.unwrap();
        assert_eq!(migration_cols.columns.len(), 1);
        assert_eq!(
            *migration_cols.columns.first().unwrap(),
            MigrationColumn::from_routes(vec![
                MigrationRoute::Timeline(MigrationTimelineRoute::Timeline(7),),
                MigrationRoute::Timeline(MigrationTimelineRoute::Profile(
                    "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245".to_owned()
                )),
            ])
        );

        assert_eq!(migration_cols.timelines.len(), 1);
        assert_eq!(
            *migration_cols.timelines.first().unwrap(),
            MigrationTimeline::new(
                7,
                MigrationTimelineKind::List(MigrationListKind::Contact(
                    MigrationPubkeySource::Explicit(
                        "4a0510f26880d40e432f4865cb5714d9d3c200ca6ebb16b418ae6c555f574967"
                            .to_owned()
                    )
                ))
            )
        )
    }

    #[test]
    fn your_notifs() {
        let route = r#"{"columns":[[{"Timeline":{"Timeline":5}}]],"timelines":[{"id":5,"kind":{"Notifications":"DeckAuthor"}}]}"#;

        let deserialized_columns = deserialize_columns_string(route.to_string());
        assert!(deserialized_columns.is_ok());

        let migration_cols = deserialized_columns.unwrap();
        assert_eq!(migration_cols.columns.len(), 1);
        assert_eq!(
            *migration_cols.columns.first().unwrap(),
            MigrationColumn::from_route(MigrationRoute::Timeline(
                MigrationTimelineRoute::Timeline(5)
            ))
        );

        assert_eq!(migration_cols.timelines.len(), 1);
        assert_eq!(
            *migration_cols.timelines.first().unwrap(),
            MigrationTimeline::new(
                5,
                MigrationTimelineKind::Notifications(MigrationPubkeySource::DeckAuthor)
            )
        )
    }

    #[test]
    fn undecided_notifs() {
        let route = r#"{"columns":[[{"AddColumn":"Base"},{"AddColumn":"UndecidedNotification"}]],"timelines":[]}"#;

        let deserialized_columns = deserialize_columns_string(route.to_string());
        assert!(deserialized_columns.is_ok());

        let migration_cols = deserialized_columns.unwrap();
        assert_eq!(migration_cols.columns.len(), 1);
        assert_eq!(
            *migration_cols.columns.first().unwrap(),
            MigrationColumn::from_routes(vec![
                MigrationRoute::AddColumn(MigrationAddColumnRoute::Base),
                MigrationRoute::AddColumn(MigrationAddColumnRoute::UndecidedNotification),
            ])
        );

        assert!(migration_cols.timelines.is_empty());
    }

    #[test]
    fn extern_notifs() {
        let route = r#"{"columns":[[{"Timeline":{"Timeline":4}}]],"timelines":[{"id":4,"kind":{"Notifications":{"Explicit":"32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245"}}}]}"#;

        let deserialized_columns = deserialize_columns_string(route.to_string());
        assert!(deserialized_columns.is_ok());

        let migration_cols = deserialized_columns.unwrap();
        assert_eq!(migration_cols.columns.len(), 1);
        assert_eq!(
            *migration_cols.columns.first().unwrap(),
            MigrationColumn::from_route(MigrationRoute::Timeline(
                MigrationTimelineRoute::Timeline(4)
            ))
        );

        assert_eq!(migration_cols.timelines.len(), 1);
        assert_eq!(
            *migration_cols.timelines.first().unwrap(),
            MigrationTimeline::new(
                4,
                MigrationTimelineKind::Notifications(MigrationPubkeySource::Explicit(
                    "32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245".to_owned()
                ))
            )
        )
    }

    #[test]
    fn hashtag() {
        let route = r#"{"columns":[[{"Timeline":{"Timeline":6}}]],"timelines":[{"id":6,"kind":{"Hashtag":"notedeck"}}]}"#;

        let deserialized_columns = deserialize_columns_string(route.to_string());
        assert!(deserialized_columns.is_ok());

        let migration_cols = deserialized_columns.unwrap();
        assert_eq!(migration_cols.columns.len(), 1);
        assert_eq!(
            *migration_cols.columns.first().unwrap(),
            MigrationColumn::from_route(MigrationRoute::Timeline(
                MigrationTimelineRoute::Timeline(6)
            ))
        );

        assert_eq!(migration_cols.timelines.len(), 1);
        assert_eq!(
            *migration_cols.timelines.first().unwrap(),
            MigrationTimeline::new(6, MigrationTimelineKind::Hashtag("notedeck".to_owned()))
        )
    }

    #[test]
    fn support() {
        let route = r#"{"columns":[[{"AddColumn":"Base"},"Support"]],"timelines":[]}"#;

        let deserialized_columns = deserialize_columns_string(route.to_string());
        assert!(deserialized_columns.is_ok());

        let migration_cols = deserialized_columns.unwrap();
        assert_eq!(migration_cols.columns.len(), 1);
        assert_eq!(
            *migration_cols.columns.first().unwrap(),
            MigrationColumn::from_routes(vec![
                MigrationRoute::AddColumn(MigrationAddColumnRoute::Base),
                MigrationRoute::Support
            ])
        );

        assert!(migration_cols.timelines.is_empty());
    }

    #[test]
    fn post() {
        let route = r#"{"columns":[[{"AddColumn":"Base"},"ComposeNote"]],"timelines":[]}"#;

        let deserialized_columns = deserialize_columns_string(route.to_string());
        assert!(deserialized_columns.is_ok());

        let migration_cols = deserialized_columns.unwrap();
        assert_eq!(migration_cols.columns.len(), 1);
        assert_eq!(
            *migration_cols.columns.first().unwrap(),
            MigrationColumn::from_routes(vec![
                MigrationRoute::AddColumn(MigrationAddColumnRoute::Base),
                MigrationRoute::ComposeNote
            ])
        );

        assert!(migration_cols.timelines.is_empty());
    }

    #[test]
    fn relay() {
        let route = r#"{"columns":[[{"AddColumn":"Base"},"Relays"]],"timelines":[]}"#;

        let deserialized_columns = deserialize_columns_string(route.to_string());
        assert!(deserialized_columns.is_ok());

        let migration_cols = deserialized_columns.unwrap();
        assert_eq!(migration_cols.columns.len(), 1);
        assert_eq!(
            *migration_cols.columns.first().unwrap(),
            MigrationColumn::from_routes(vec![
                MigrationRoute::AddColumn(MigrationAddColumnRoute::Base),
                MigrationRoute::Relays
            ])
        );

        assert!(migration_cols.timelines.is_empty());
    }

    #[test]
    fn accounts() {
        let route =
            r#"{"columns":[[{"AddColumn":"Base"},{"Accounts":"Accounts"}]],"timelines":[]}"#;

        let deserialized_columns = deserialize_columns_string(route.to_string());
        assert!(deserialized_columns.is_ok());

        let migration_cols = deserialized_columns.unwrap();
        assert_eq!(migration_cols.columns.len(), 1);
        assert_eq!(
            *migration_cols.columns.first().unwrap(),
            MigrationColumn::from_routes(vec![
                MigrationRoute::AddColumn(MigrationAddColumnRoute::Base),
                MigrationRoute::Accounts(MigrationAccountsRoute::Accounts),
            ])
        );

        assert!(migration_cols.timelines.is_empty());
    }

    #[test]
    fn login() {
        let route = r#"{"columns":[[{"AddColumn":"Base"},{"Accounts":"Accounts"},{"Accounts":"AddAccount"}]],"timelines":[]}"#;

        let deserialized_columns = deserialize_columns_string(route.to_string());
        assert!(deserialized_columns.is_ok());

        let migration_cols = deserialized_columns.unwrap();
        assert_eq!(migration_cols.columns.len(), 1);
        assert_eq!(
            *migration_cols.columns.first().unwrap(),
            MigrationColumn::from_routes(vec![
                MigrationRoute::AddColumn(MigrationAddColumnRoute::Base),
                MigrationRoute::Accounts(MigrationAccountsRoute::Accounts),
                MigrationRoute::Accounts(MigrationAccountsRoute::AddAccount),
            ])
        );

        assert!(migration_cols.timelines.is_empty());
    }
}
