use std::{collections::HashMap, fmt, str::FromStr};

use enostr::Pubkey;
use nostrdb::{Ndb, Transaction};
use serde::{Deserialize, Serialize};
use tracing::{debug, error};

use crate::{
    column::{ColSize, Column, Columns, IntermediaryRoute},
    decks::{Deck, Decks, DecksCache},
    route::Route,
    timeline::{TimelineCache, TimelineKind},
    Error,
};

use notedeck::{storage, DataPath, DataPathType, Directory};
use tokenator::{ParseError, TokenParser, TokenWriter};

pub static DECKS_CACHE_FILE: &str = "decks_cache.json";

pub fn load_decks_cache(
    path: &DataPath,
    ndb: &Ndb,
    timeline_cache: &mut TimelineCache,
) -> Option<DecksCache> {
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

    serializable_decks_cache
        .decks_cache(ndb, timeline_cache)
        .ok()
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
        debug!("Successfully wrote decks cache to {}", DECKS_CACHE_FILE);
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

    pub fn decks_cache(
        self,
        ndb: &Ndb,
        timeline_cache: &mut TimelineCache,
    ) -> Result<DecksCache, Error> {
        let account_to_decks = self
            .decks_cache
            .into_iter()
            .map(|(pubkey, serializable_decks)| {
                serializable_decks
                    .decks(ndb, timeline_cache, &pubkey)
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

    fn decks(
        self,
        ndb: &Ndb,
        timeline_cache: &mut TimelineCache,
        deck_key: &Pubkey,
    ) -> Result<Decks, Error> {
        Ok(Decks::from_decks(
            self.active_deck,
            self.decks
                .into_iter()
                .map(|d| d.deck(ndb, timeline_cache, deck_key))
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

    pub fn deck(
        self,
        ndb: &Ndb,
        timeline_cache: &mut TimelineCache,
        deck_user: &Pubkey,
    ) -> Result<Deck, Error> {
        let columns = deserialize_columns(ndb, timeline_cache, deck_user, self.columns);
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
            let mut writer = TokenWriter::default();
            writer.write_token("size");
            writer.write_token(format!("{:?}", column.col_size).as_str());
            route.serialize_tokens(&mut writer);
            column_routes.push(writer.str().to_string());
        }
        cols_serialized.push(column_routes);
    }

    cols_serialized
}

fn deserialize_columns(
    ndb: &Ndb,
    timeline_cache: &mut TimelineCache,
    deck_user: &Pubkey,
    columns: Vec<Vec<String>>,
) -> Columns {
    let mut cols = Columns::new();
    for column in columns {
        let Some(route) = column.first() else {
            continue;
        };

        let mut tokens: Vec<&str> = route.split(":").collect();

        let mut col_size = ColSize::S;
        if Some(&"size") == tokens.first() {
            if let Some(size_str) = tokens.get(1) {
                if let Ok(size) = size_str.parse::<crate::column::ColSize>() {
                    col_size = size;
                }
            }
            tokens = tokens.into_iter().skip(2).collect();
        }

        let mut parser = TokenParser::new(&tokens);

        let mut col: Option<&mut Column> = None;
        match CleanIntermediaryRoute::parse(&mut parser, deck_user) {
            Ok(route_intermediary) => {
                if let Some(ir) = route_intermediary.into_intermediary_route(ndb) {
                    col = Some(cols.insert_intermediary_routes(timeline_cache, vec![ir]));
                }
            }
            Err(err) => {
                error!("could not turn tokens to RouteIntermediary: {:?}", err);
            }
        }

        if let Some(col) = col {
            col.col_size = col_size;
        }
    }

    cols
}

enum CleanIntermediaryRoute {
    ToTimeline(TimelineKind),
    ToRoute(Route),
}

impl CleanIntermediaryRoute {
    fn into_intermediary_route(self, ndb: &Ndb) -> Option<IntermediaryRoute> {
        match self {
            CleanIntermediaryRoute::ToTimeline(timeline_kind) => {
                let txn = Transaction::new(ndb).unwrap();
                Some(IntermediaryRoute::Timeline(
                    timeline_kind.into_timeline(&txn, ndb)?,
                ))
            }
            CleanIntermediaryRoute::ToRoute(route) => Some(IntermediaryRoute::Route(route)),
        }
    }

    fn parse<'a>(
        parser: &mut TokenParser<'a>,
        deck_author: &Pubkey,
    ) -> Result<Self, ParseError<'a>> {
        let timeline = parser.try_parse(|p| {
            Ok(CleanIntermediaryRoute::ToTimeline(TimelineKind::parse(
                p,
                deck_author,
            )?))
        });
        if timeline.is_ok() {
            return timeline;
        }

        parser.try_parse(|p| {
            Ok(CleanIntermediaryRoute::ToRoute(Route::parse(
                p,
                deck_author,
            )?))
        })
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
