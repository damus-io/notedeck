use std::{collections::HashMap, fmt, str::FromStr};

use enostr::{NoteId, Pubkey};
use nostrdb::Ndb;
use serde::{Deserialize, Serialize};
use strum::IntoEnumIterator;
use strum_macros::EnumIter;
use tracing::{error, info};

use crate::{
    accounts::AccountsRoute,
    column::{Columns, IntermediaryRoute},
    decks::{Deck, Decks, DecksCache},
    route::Route,
    timeline::{kind::ListKind, AlgoTimeline, PubkeySource, TimelineKind, TimelineRoute},
    ui::add_column::{AddAlgoRoute, AddColumnRoute},
    Error,
};

use notedeck::{storage, DataPath, DataPathType, Directory};

pub static DECKS_CACHE_FILE: &str = "decks_cache.json";

pub fn load_decks_cache(path: &DataPath, ndb: &Ndb) -> Option<DecksCache> {
    let data_path = path.path(DataPathType::Setting);

    let decks_cache_str = match Directory::new(data_path).get_file(DECKS_CACHE_FILE.to_owned()) {
        Ok(s) => s,
        Err(e) => {
            error!(
                "Could not read decks cache from file {}:  {}",
                DECKS_CACHE_FILE, e
            );
            return None;
        }
    };

    let serializable_decks_cache =
        serde_json::from_str::<SerializableDecksCache>(&decks_cache_str).ok()?;

    serializable_decks_cache.decks_cache(ndb).ok()
}

pub fn save_decks_cache(path: &DataPath, decks_cache: &DecksCache) {
    let serialized_decks_cache =
        match serde_json::to_string(&SerializableDecksCache::to_serializable(decks_cache)) {
            Ok(s) => s,
            Err(e) => {
                error!("Could not serialize decks cache: {}", e);
                return;
            }
        };

    let data_path = path.path(DataPathType::Setting);

    if let Err(e) = storage::write_file(
        &data_path,
        DECKS_CACHE_FILE.to_string(),
        &serialized_decks_cache,
    ) {
        error!(
            "Could not write decks cache to file {}: {}",
            DECKS_CACHE_FILE, e
        );
    } else {
        info!("Successfully wrote decks cache to {}", DECKS_CACHE_FILE);
    }
}

#[derive(Serialize, Deserialize)]
struct SerializableDecksCache {
    #[serde(serialize_with = "serialize_map", deserialize_with = "deserialize_map")]
    decks_cache: HashMap<Pubkey, SerializableDecks>,
}

impl SerializableDecksCache {
    fn to_serializable(decks_cache: &DecksCache) -> Self {
        SerializableDecksCache {
            decks_cache: decks_cache
                .get_mapping()
                .iter()
                .map(|(k, v)| (*k, SerializableDecks::from_decks(v)))
                .collect(),
        }
    }

    pub fn decks_cache(self, ndb: &Ndb) -> Result<DecksCache, Error> {
        let account_to_decks = self
            .decks_cache
            .into_iter()
            .map(|(pubkey, serializable_decks)| {
                let deck_key = pubkey.bytes();
                serializable_decks
                    .decks(ndb, deck_key)
                    .map(|decks| (pubkey, decks))
            })
            .collect::<Result<HashMap<Pubkey, Decks>, Error>>()?;

        Ok(DecksCache::new(account_to_decks))
    }
}

fn serialize_map<S>(
    map: &HashMap<Pubkey, SerializableDecks>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let stringified_map: HashMap<String, &SerializableDecks> =
        map.iter().map(|(k, v)| (k.hex(), v)).collect();
    stringified_map.serialize(serializer)
}

fn deserialize_map<'de, D>(deserializer: D) -> Result<HashMap<Pubkey, SerializableDecks>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let stringified_map: HashMap<String, SerializableDecks> = HashMap::deserialize(deserializer)?;

    stringified_map
        .into_iter()
        .map(|(k, v)| {
            let key = Pubkey::from_hex(&k).map_err(serde::de::Error::custom)?;
            Ok((key, v))
        })
        .collect()
}

#[derive(Serialize, Deserialize)]
struct SerializableDecks {
    active_deck: usize,
    decks: Vec<SerializableDeck>,
}

impl SerializableDecks {
    pub fn from_decks(decks: &Decks) -> Self {
        Self {
            active_deck: decks.active_index(),
            decks: decks
                .decks()
                .iter()
                .map(SerializableDeck::from_deck)
                .collect(),
        }
    }

    fn decks(self, ndb: &Ndb, deck_key: &[u8; 32]) -> Result<Decks, Error> {
        Ok(Decks::from_decks(
            self.active_deck,
            self.decks
                .into_iter()
                .map(|d| d.deck(ndb, deck_key))
                .collect::<Result<_, _>>()?,
        ))
    }
}

#[derive(Serialize, Deserialize)]
struct SerializableDeck {
    metadata: Vec<String>,
    columns: Vec<Vec<String>>,
}

#[derive(PartialEq, Clone)]
enum MetadataKeyword {
    Icon,
    Name,
}

impl MetadataKeyword {
    const MAPPING: &'static [(&'static str, MetadataKeyword)] = &[
        ("icon", MetadataKeyword::Icon),
        ("name", MetadataKeyword::Name),
    ];
}
impl fmt::Display for MetadataKeyword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(name) = MetadataKeyword::MAPPING
            .iter()
            .find(|(_, keyword)| keyword == self)
            .map(|(name, _)| *name)
        {
            write!(f, "{}", name)
        } else {
            write!(f, "UnknownMetadataKeyword")
        }
    }
}

impl FromStr for MetadataKeyword {
    type Err = Error;

    fn from_str(serialized: &str) -> Result<Self, Self::Err> {
        MetadataKeyword::MAPPING
            .iter()
            .find(|(name, _)| *name == serialized)
            .map(|(_, keyword)| keyword.clone())
            .ok_or(Error::Generic(
                "Could not convert string to Keyword enum".to_owned(),
            ))
    }
}

struct MetadataPayload {
    keyword: MetadataKeyword,
    value: String,
}

impl MetadataPayload {
    fn new(keyword: MetadataKeyword, value: String) -> Self {
        Self { keyword, value }
    }
}

fn serialize_metadata(payloads: Vec<MetadataPayload>) -> Vec<String> {
    payloads
        .into_iter()
        .map(|payload| format!("{}:{}", payload.keyword, payload.value))
        .collect()
}

fn deserialize_metadata(serialized_metadatas: Vec<String>) -> Option<Vec<MetadataPayload>> {
    let mut payloads = Vec::new();
    for serialized_metadata in serialized_metadatas {
        let cur_split: Vec<&str> = serialized_metadata.split(':').collect();
        if cur_split.len() != 2 {
            continue;
        }

        if let Ok(keyword) = MetadataKeyword::from_str(cur_split.first().unwrap()) {
            payloads.push(MetadataPayload {
                keyword,
                value: cur_split.get(1).unwrap().to_string(),
            });
        }
    }

    if payloads.is_empty() {
        None
    } else {
        Some(payloads)
    }
}

impl SerializableDeck {
    pub fn from_deck(deck: &Deck) -> Self {
        let columns = serialize_columns(deck.columns());

        let metadata = serialize_metadata(vec![
            MetadataPayload::new(MetadataKeyword::Icon, deck.icon.to_string()),
            MetadataPayload::new(MetadataKeyword::Name, deck.name.clone()),
        ]);

        SerializableDeck { metadata, columns }
    }

    pub fn deck(self, ndb: &Ndb, deck_user: &[u8; 32]) -> Result<Deck, Error> {
        let columns = deserialize_columns(ndb, deck_user, self.columns);
        let deserialized_metadata = deserialize_metadata(self.metadata)
            .ok_or(Error::Generic("Could not deserialize metadata".to_owned()))?;

        let icon = deserialized_metadata
            .iter()
            .find(|p| p.keyword == MetadataKeyword::Icon)
            .map_or_else(|| "🇩", |f| &f.value);
        let name = deserialized_metadata
            .iter()
            .find(|p| p.keyword == MetadataKeyword::Name)
            .map_or_else(|| "Deck", |f| &f.value)
            .to_string();

        Ok(Deck::new_with_columns(
            icon.parse::<char>()
                .map_err(|_| Error::Generic("could not convert String -> char".to_owned()))?,
            name,
            columns,
        ))
    }
}

fn serialize_columns(columns: &Columns) -> Vec<Vec<String>> {
    let mut cols_serialized: Vec<Vec<String>> = Vec::new();

    for column in columns.columns() {
        let mut column_routes = Vec::new();
        for route in column.router().routes() {
            if let Some(route_str) = serialize_route(route, columns) {
                column_routes.push(route_str);
            }
        }
        cols_serialized.push(column_routes);
    }

    cols_serialized
}

fn deserialize_columns(ndb: &Ndb, deck_user: &[u8; 32], serialized: Vec<Vec<String>>) -> Columns {
    let mut cols = Columns::new();
    for serialized_routes in serialized {
        let mut cur_routes = Vec::new();
        for serialized_route in serialized_routes {
            let selections = Selection::from_serialized(&serialized_route);
            if let Some(route_intermediary) = selections_to_route(&selections) {
                if let Some(ir) = route_intermediary.intermediary_route(ndb, Some(deck_user)) {
                    match &ir {
                        IntermediaryRoute::Route(Route::Timeline(TimelineRoute::Thread(_)))
                        | IntermediaryRoute::Route(Route::Timeline(TimelineRoute::Profile(_))) => {
                            // Do nothing. TimelineRoute Threads & Profiles not yet supported for deserialization
                        }
                        _ => cur_routes.push(ir),
                    }
                }
            } else {
                error!(
                    "could not turn selections to RouteIntermediary: {:?}",
                    selections
                );
            }
        }

        if !cur_routes.is_empty() {
            cols.insert_intermediary_routes(cur_routes);
        }
    }

    cols
}

/// Different token types for our deck serializer/deserializer
///
/// We have more than one token type so that we can avoid match catch-alls
/// in different parts of our parser.
#[derive(Clone, Debug)]
enum Selection {
    Keyword(Keyword),
    Algo(AlgoKeyword),
    List(ListKeyword),
    PubkeySource(PubkeySourceKeyword),
    Payload(String),
}

impl FromStr for Selection {
    type Err = Error;

    fn from_str(serialized: &str) -> Result<Self, Self::Err> {
        Ok(parse_selection(serialized))
    }
}

#[derive(Clone, PartialEq, Eq, Debug, EnumIter)]
enum AlgoKeyword {
    LastPerPubkey,
}

impl AlgoKeyword {
    #[inline]
    pub fn name(&self) -> &'static str {
        match self {
            AlgoKeyword::LastPerPubkey => "last_per_pubkey",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, EnumIter)]
enum ListKeyword {
    Contact,
}

impl ListKeyword {
    #[inline]
    pub fn name(&self) -> &'static str {
        match self {
            ListKeyword::Contact => "contact",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, EnumIter)]
enum PubkeySourceKeyword {
    Explicit,
    DeckAuthor,
}

impl PubkeySourceKeyword {
    #[inline]
    pub fn name(&self) -> &'static str {
        match self {
            PubkeySourceKeyword::Explicit => "explicit",
            PubkeySourceKeyword::DeckAuthor => "deck_author",
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, EnumIter)]
enum Keyword {
    Notifs,
    Universe,
    Profile,
    Hashtag,
    Generic,
    Thread,
    Reply,
    Quote,
    Account,
    Show,
    New,
    Relay,
    Compose,
    Column,
    AlgoSelection,
    NotificationSelection,
    ExternalNotifSelection,
    HashtagSelection,
    Support,
    Deck,
    Edit,
    IndividualSelection,
    ExternalIndividualSelection,
}

impl Keyword {
    fn name(&self) -> &'static str {
        match self {
            Keyword::Notifs => "notifs",
            Keyword::Universe => "universe",
            Keyword::Profile => "profile",
            Keyword::Hashtag => "hashtag",
            Keyword::Generic => "generic",
            Keyword::Thread => "thread",
            Keyword::Reply => "reply",
            Keyword::Quote => "quote",
            Keyword::Account => "account",
            Keyword::Show => "show",
            Keyword::New => "new",
            Keyword::Relay => "relay",
            Keyword::Compose => "compose",
            Keyword::Column => "column",
            Keyword::AlgoSelection => "algo_selection",
            Keyword::NotificationSelection => "notification_selection",
            Keyword::ExternalNotifSelection => "external_notif_selection",
            Keyword::IndividualSelection => "individual_selection",
            Keyword::ExternalIndividualSelection => "external_individual_selection",
            Keyword::HashtagSelection => "hashtag_selection",
            Keyword::Support => "support",
            Keyword::Deck => "deck",
            Keyword::Edit => "edit",
        }
    }
}

impl fmt::Display for Keyword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl fmt::Display for AlgoKeyword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl fmt::Display for ListKeyword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl FromStr for PubkeySourceKeyword {
    type Err = Error;

    fn from_str(serialized: &str) -> Result<Self, Self::Err> {
        for keyword in Self::iter() {
            if serialized == keyword.name() {
                return Ok(keyword);
            }
        }

        Err(Error::Generic(
            "Could not convert string to Keyword enum".to_owned(),
        ))
    }
}

impl FromStr for ListKeyword {
    type Err = Error;

    fn from_str(serialized: &str) -> Result<Self, Self::Err> {
        for keyword in Self::iter() {
            if serialized == keyword.name() {
                return Ok(keyword);
            }
        }

        Err(Error::Generic(
            "Could not convert string to Keyword enum".to_owned(),
        ))
    }
}

impl fmt::Display for PubkeySourceKeyword {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

impl FromStr for AlgoKeyword {
    type Err = Error;

    fn from_str(serialized: &str) -> Result<Self, Self::Err> {
        for keyword in Self::iter() {
            if serialized == keyword.name() {
                return Ok(keyword);
            }
        }

        Err(Error::Generic(
            "Could not convert string to Keyword enum".to_owned(),
        ))
    }
}

impl FromStr for Keyword {
    type Err = Error;

    fn from_str(serialized: &str) -> Result<Self, Self::Err> {
        for keyword in Self::iter() {
            if serialized == keyword.name() {
                return Ok(keyword);
            }
        }

        Err(Error::Generic(
            "Could not convert string to Keyword enum".to_owned(),
        ))
    }
}

enum CleanIntermediaryRoute {
    ToTimeline(TimelineKind),
    ToRoute(Route),
}

impl CleanIntermediaryRoute {
    fn intermediary_route(self, ndb: &Ndb, user: Option<&[u8; 32]>) -> Option<IntermediaryRoute> {
        match self {
            CleanIntermediaryRoute::ToTimeline(timeline_kind) => Some(IntermediaryRoute::Timeline(
                timeline_kind.into_timeline(ndb, user)?,
            )),
            CleanIntermediaryRoute::ToRoute(route) => Some(IntermediaryRoute::Route(route)),
        }
    }
}

// TODO: The public-accessible version will be a subset of this
fn serialize_route(route: &Route, columns: &Columns) -> Option<String> {
    let mut selections: Vec<Selection> = Vec::new();
    match route {
        Route::Timeline(timeline_route) => match timeline_route {
            TimelineRoute::Timeline(timeline_id) => {
                if let Some(timeline) = columns.find_timeline(*timeline_id) {
                    match &timeline.kind {
                        TimelineKind::List(list_kind) => match list_kind {
                            ListKind::Contact(pubkey_source) => {
                                selections.push(Selection::List(ListKeyword::Contact));
                                selections.extend(generate_pubkey_selections(pubkey_source));
                            }
                        },
                        TimelineKind::Algo(AlgoTimeline::LastPerPubkey(list_kind)) => {
                            match list_kind {
                                ListKind::Contact(pk_src) => {
                                    selections.push(Selection::Algo(AlgoKeyword::LastPerPubkey));
                                    selections.push(Selection::List(ListKeyword::Contact));
                                    selections.extend(generate_pubkey_selections(pk_src));
                                }
                            }
                        }
                        TimelineKind::Notifications(pubkey_source) => {
                            selections.push(Selection::Keyword(Keyword::Notifs));
                            selections.extend(generate_pubkey_selections(pubkey_source));
                        }
                        TimelineKind::Profile(pubkey_source) => {
                            selections.push(Selection::Keyword(Keyword::Profile));
                            selections.extend(generate_pubkey_selections(pubkey_source));
                        }
                        TimelineKind::Universe => {
                            selections.push(Selection::Keyword(Keyword::Universe))
                        }
                        TimelineKind::Thread(root_id) => {
                            selections.push(Selection::Keyword(Keyword::Thread));
                            selections.push(Selection::Payload(hex::encode(root_id.bytes())));
                        }
                        TimelineKind::Generic => {
                            selections.push(Selection::Keyword(Keyword::Generic))
                        }
                        TimelineKind::Hashtag(hashtag) => {
                            selections.push(Selection::Keyword(Keyword::Hashtag));
                            selections.push(Selection::Payload(hashtag.to_string()));
                        }
                    }
                }
            }
            TimelineRoute::Thread(note_id) => {
                selections.push(Selection::Keyword(Keyword::Thread));
                selections.push(Selection::Payload(note_id.hex()));
            }
            TimelineRoute::Profile(pubkey) => {
                selections.push(Selection::Keyword(Keyword::Profile));
                selections.push(Selection::PubkeySource(PubkeySourceKeyword::Explicit));
                selections.push(Selection::Payload(pubkey.hex()));
            }
            TimelineRoute::Reply(note_id) => {
                selections.push(Selection::Keyword(Keyword::Reply));
                selections.push(Selection::Payload(note_id.hex()));
            }
            TimelineRoute::Quote(note_id) => {
                selections.push(Selection::Keyword(Keyword::Quote));
                selections.push(Selection::Payload(note_id.hex()));
            }
        },
        Route::Accounts(accounts_route) => {
            selections.push(Selection::Keyword(Keyword::Account));
            match accounts_route {
                AccountsRoute::Accounts => selections.push(Selection::Keyword(Keyword::Show)),
                AccountsRoute::AddAccount => selections.push(Selection::Keyword(Keyword::New)),
            }
        }
        Route::Relays => selections.push(Selection::Keyword(Keyword::Relay)),
        Route::ComposeNote => selections.push(Selection::Keyword(Keyword::Compose)),
        Route::AddColumn(add_column_route) => {
            selections.push(Selection::Keyword(Keyword::Column));
            match add_column_route {
                AddColumnRoute::Base => (),
                AddColumnRoute::Algo(algo_route) => match algo_route {
                    AddAlgoRoute::Base => {
                        selections.push(Selection::Keyword(Keyword::AlgoSelection))
                    }

                    AddAlgoRoute::LastPerPubkey => {
                        selections.push(Selection::Keyword(Keyword::AlgoSelection));
                        selections.push(Selection::Algo(AlgoKeyword::LastPerPubkey));
                    }
                },
                AddColumnRoute::UndecidedNotification => {
                    selections.push(Selection::Keyword(Keyword::NotificationSelection))
                }
                AddColumnRoute::ExternalNotification => {
                    selections.push(Selection::Keyword(Keyword::ExternalNotifSelection))
                }
                AddColumnRoute::Hashtag => {
                    selections.push(Selection::Keyword(Keyword::HashtagSelection))
                }
                AddColumnRoute::UndecidedIndividual => {
                    selections.push(Selection::Keyword(Keyword::IndividualSelection))
                }
                AddColumnRoute::ExternalIndividual => {
                    selections.push(Selection::Keyword(Keyword::ExternalIndividualSelection))
                }
            }
        }
        Route::Support => selections.push(Selection::Keyword(Keyword::Support)),
        Route::NewDeck => {
            selections.push(Selection::Keyword(Keyword::Deck));
            selections.push(Selection::Keyword(Keyword::New));
        }
        Route::EditDeck(index) => {
            selections.push(Selection::Keyword(Keyword::Deck));
            selections.push(Selection::Keyword(Keyword::Edit));
            selections.push(Selection::Payload(index.to_string()));
        }
        Route::EditProfile(pubkey) => {
            selections.push(Selection::Keyword(Keyword::Profile));
            selections.push(Selection::Keyword(Keyword::Edit));
            selections.push(Selection::Payload(pubkey.hex()));
        }
    }

    if selections.is_empty() {
        None
    } else {
        Some(
            selections
                .iter()
                .map(|k| k.to_string())
                .collect::<Vec<String>>()
                .join(":"),
        )
    }
}

fn generate_pubkey_selections(source: &PubkeySource) -> Vec<Selection> {
    let mut selections = Vec::new();
    match source {
        PubkeySource::Explicit(pubkey) => {
            selections.push(Selection::PubkeySource(PubkeySourceKeyword::Explicit));
            selections.push(Selection::Payload(pubkey.hex()));
        }
        PubkeySource::DeckAuthor => {
            selections.push(Selection::PubkeySource(PubkeySourceKeyword::DeckAuthor));
        }
    }
    selections
}

/// Parses a selection
fn parse_selection(token: &str) -> Selection {
    AlgoKeyword::from_str(token)
        .map(Selection::Algo)
        .or_else(|_| ListKeyword::from_str(token).map(Selection::List))
        .or_else(|_| PubkeySourceKeyword::from_str(token).map(Selection::PubkeySource))
        .or_else(|_| Keyword::from_str(token).map(Selection::Keyword))
        .unwrap_or_else(|_| Selection::Payload(token.to_owned()))
}

impl Selection {
    fn from_serialized(buffer: &str) -> Vec<Self> {
        let mut selections = Vec::new();
        let seperator = ":";
        let sep_len = seperator.len();
        let mut pos = 0;

        while let Some(offset) = buffer[pos..].find(seperator) {
            selections.push(parse_selection(&buffer[pos..pos + offset]));
            pos = pos + offset + sep_len;
        }

        selections.push(parse_selection(&buffer[pos..]));

        selections
    }
}

/// Parse an explicit:abdef... or deck_author from a Selection token stream.
///
/// Also handle the case where there is nothing. We assume this means deck_author.
fn parse_pubkey_src_selection(tokens: &[Selection]) -> Option<PubkeySource> {
    match tokens.first() {
        // we handle bare payloads and assume they are explicit pubkey sources
        Some(Selection::Payload(hex)) => {
            let pk = Pubkey::from_hex(hex.as_str()).ok()?;
            Some(PubkeySource::Explicit(pk))
        }

        Some(Selection::PubkeySource(PubkeySourceKeyword::Explicit)) => {
            if let Selection::Payload(hex) = tokens.get(1)? {
                let pk = Pubkey::from_hex(hex.as_str()).ok()?;
                Some(PubkeySource::Explicit(pk))
            } else {
                None
            }
        }

        None | Some(Selection::PubkeySource(PubkeySourceKeyword::DeckAuthor)) => {
            Some(PubkeySource::DeckAuthor)
        }

        Some(Selection::Keyword(_kw)) => None,
        Some(Selection::Algo(_kw)) => None,
        Some(Selection::List(_kw)) => None,
    }
}

/// Parse ListKinds from Selections
fn parse_list_kind_selections(tokens: &[Selection]) -> Option<ListKind> {
    // only list selections are valid in this position
    let list_kw = if let Selection::List(list_kw) = tokens.first()? {
        list_kw
    } else {
        return None;
    };

    let pubkey_src = parse_pubkey_src_selection(&tokens[1..])?;

    Some(match list_kw {
        ListKeyword::Contact => ListKind::contact_list(pubkey_src),
    })
}

fn selections_to_route(selections: &[Selection]) -> Option<CleanIntermediaryRoute> {
    match selections.first()? {
        Selection::Keyword(Keyword::AlgoSelection) => {
            let r = match selections.get(1) {
                None => AddColumnRoute::Algo(AddAlgoRoute::Base),
                Some(Selection::Algo(algo_kw)) => match algo_kw {
                    AlgoKeyword::LastPerPubkey => AddColumnRoute::Algo(AddAlgoRoute::LastPerPubkey),
                },
                // other keywords are invalid here
                Some(_) => {
                    return None;
                }
            };

            Some(CleanIntermediaryRoute::ToRoute(Route::AddColumn(r)))
        }

        // Algorithm timelines
        Selection::Algo(algo_kw) => {
            let timeline_kind = match algo_kw {
                AlgoKeyword::LastPerPubkey => {
                    let list_kind = parse_list_kind_selections(&selections[1..])?;
                    TimelineKind::last_per_pubkey(list_kind)
                }
            };

            Some(CleanIntermediaryRoute::ToTimeline(timeline_kind))
        }

        // We never have PubkeySource keywords at the top level
        Selection::PubkeySource(_pk_src) => None,

        Selection::List(ListKeyword::Contact) => {
            // only pubkey/src is allowed in this position
            let pubkey_src = parse_pubkey_src_selection(&selections[1..])?;
            Some(CleanIntermediaryRoute::ToTimeline(
                TimelineKind::contact_list(pubkey_src),
            ))
        }

        Selection::Keyword(Keyword::Notifs) => {
            let pubkey_src = parse_pubkey_src_selection(&selections[1..])?;
            Some(CleanIntermediaryRoute::ToTimeline(
                TimelineKind::notifications(pubkey_src),
            ))
        }

        Selection::Keyword(Keyword::Profile) => {
            // we only expect PubkeySource in this position
            let pubkey_src = parse_pubkey_src_selection(&selections[1..])?;
            Some(CleanIntermediaryRoute::ToTimeline(TimelineKind::profile(
                pubkey_src,
            )))
        }

        Selection::Keyword(Keyword::Universe) => {
            Some(CleanIntermediaryRoute::ToTimeline(TimelineKind::Universe))
        }

        Selection::Keyword(Keyword::Hashtag) => {
            if let Selection::Payload(hashtag) = selections.get(1)? {
                Some(CleanIntermediaryRoute::ToTimeline(TimelineKind::Hashtag(
                    hashtag.to_string(),
                )))
            } else {
                None
            }
        }

        Selection::Keyword(Keyword::Generic) => {
            Some(CleanIntermediaryRoute::ToTimeline(TimelineKind::Generic))
        }

        Selection::Keyword(Keyword::Thread) => {
            if let Selection::Payload(hex) = selections.get(1)? {
                Some(CleanIntermediaryRoute::ToRoute(Route::thread(
                    NoteId::from_hex(hex.as_str()).ok()?,
                )))
            } else {
                None
            }
        }

        Selection::Keyword(Keyword::Reply) => {
            if let Selection::Payload(hex) = selections.get(1)? {
                Some(CleanIntermediaryRoute::ToRoute(Route::reply(
                    NoteId::from_hex(hex.as_str()).ok()?,
                )))
            } else {
                None
            }
        }
        Selection::Keyword(Keyword::Quote) => {
            if let Selection::Payload(hex) = selections.get(1)? {
                Some(CleanIntermediaryRoute::ToRoute(Route::quote(
                    NoteId::from_hex(hex.as_str()).ok()?,
                )))
            } else {
                None
            }
        }
        Selection::Keyword(Keyword::Account) => match selections.get(1)? {
            Selection::Keyword(Keyword::Show) => Some(CleanIntermediaryRoute::ToRoute(
                Route::Accounts(AccountsRoute::Accounts),
            )),
            Selection::Keyword(Keyword::New) => Some(CleanIntermediaryRoute::ToRoute(
                Route::Accounts(AccountsRoute::AddAccount),
            )),
            _ => None,
        },
        Selection::Keyword(Keyword::Relay) => Some(CleanIntermediaryRoute::ToRoute(Route::Relays)),
        Selection::Keyword(Keyword::Compose) => {
            Some(CleanIntermediaryRoute::ToRoute(Route::ComposeNote))
        }
        Selection::Keyword(Keyword::Column) => match selections.get(1)? {
            Selection::Keyword(Keyword::NotificationSelection) => {
                Some(CleanIntermediaryRoute::ToRoute(Route::AddColumn(
                    AddColumnRoute::UndecidedNotification,
                )))
            }
            Selection::Keyword(Keyword::ExternalNotifSelection) => {
                Some(CleanIntermediaryRoute::ToRoute(Route::AddColumn(
                    AddColumnRoute::ExternalNotification,
                )))
            }
            Selection::Keyword(Keyword::HashtagSelection) => Some(CleanIntermediaryRoute::ToRoute(
                Route::AddColumn(AddColumnRoute::Hashtag),
            )),
            Selection::Keyword(Keyword::IndividualSelection) => {
                Some(CleanIntermediaryRoute::ToRoute(Route::AddColumn(
                    AddColumnRoute::UndecidedIndividual,
                )))
            }
            Selection::Keyword(Keyword::ExternalIndividualSelection) => {
                Some(CleanIntermediaryRoute::ToRoute(Route::AddColumn(
                    AddColumnRoute::ExternalIndividual,
                )))
            }
            _ => None,
        },
        Selection::Keyword(Keyword::Support) => {
            Some(CleanIntermediaryRoute::ToRoute(Route::Support))
        }
        Selection::Keyword(Keyword::Deck) => match selections.get(1)? {
            Selection::Keyword(Keyword::New) => {
                Some(CleanIntermediaryRoute::ToRoute(Route::NewDeck))
            }
            Selection::Keyword(Keyword::Edit) => {
                if let Selection::Payload(index_str) = selections.get(2)? {
                    let parsed_index = index_str.parse::<usize>().ok()?;
                    Some(CleanIntermediaryRoute::ToRoute(Route::EditDeck(
                        parsed_index,
                    )))
                } else {
                    None
                }
            }
            _ => None,
        },
        Selection::Payload(_)
        | Selection::Keyword(Keyword::New)
        | Selection::Keyword(Keyword::Show)
        | Selection::Keyword(Keyword::NotificationSelection)
        | Selection::Keyword(Keyword::ExternalNotifSelection)
        | Selection::Keyword(Keyword::HashtagSelection)
        | Selection::Keyword(Keyword::IndividualSelection)
        | Selection::Keyword(Keyword::ExternalIndividualSelection)
        | Selection::Keyword(Keyword::Edit) => None,
    }
}

impl fmt::Display for Selection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Selection::Keyword(keyword) => write!(f, "{}", keyword),
            Selection::Payload(payload) => write!(f, "{}", payload),
            Selection::Algo(algo_kw) => write!(f, "{}", algo_kw),
            Selection::List(list_kw) => write!(f, "{}", list_kw),
            Selection::PubkeySource(pk_src_kw) => write!(f, "{}", pk_src_kw),
        }
    }
}

#[cfg(test)]
mod tests {
    //use enostr::Pubkey;

    //use crate::{route::Route, timeline::TimelineRoute};

    //use super::deserialize_columns;

    /* TODO: re-enable once we have test_app working again
    #[test]
    fn test_deserialize_columns() {
        let serialized = vec![
            vec!["universe".to_owned()],
            vec![
                "notifs:explicit:aa733081e4f0f79dd43023d8983265593f2b41a988671cfcef3f489b91ad93fe"
                    .to_owned(),
            ],
        ];

        let user =
            Pubkey::from_hex("aa733081e4f0f79dd43023d8983265593f2b41a988671cfcef3f489b91ad93fe")
                .unwrap();

        let app = test_app();
        let cols = deserialize_columns(&app.ndb, user.bytes(), serialized);

        assert_eq!(cols.columns().len(), 2);
        let router = cols.column(0).router();
        assert_eq!(router.routes().len(), 1);

        if let Route::Timeline(TimelineRoute::Timeline(_)) = router.routes().first().unwrap() {
        } else {
            panic!("The first router route is not a TimelineRoute::Timeline variant");
        }

        let router = cols.column(1).router();
        assert_eq!(router.routes().len(), 1);
        if let Route::Timeline(TimelineRoute::Timeline(_)) = router.routes().first().unwrap() {
        } else {
            panic!("The second router route is not a TimelineRoute::Timeline variant");
        }
    }
    */
}
