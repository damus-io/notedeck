use crate::column::{ColumnKind, PubkeySource};
use crate::filter::FilterState;
use crate::timeline::Timeline;
use enostr::{Filter, Keypair, Pubkey, SecretKey};
use nostrdb::Ndb;
use tracing::{debug, error, info};

pub struct Args {
    pub columns: Vec<ArgColumn>,
    pub relays: Vec<String>,
    pub is_mobile: Option<bool>,
    pub keys: Vec<Keypair>,
    pub since_optimize: bool,
    pub light: bool,
    pub debug: bool,
    pub dbpath: Option<String>,
}

impl Args {
    pub fn parse(args: &[String]) -> Self {
        let mut res = Args {
            columns: vec![],
            relays: vec![],
            is_mobile: None,
            keys: vec![],
            light: false,
            since_optimize: true,
            debug: false,
            dbpath: None,
        };

        let mut i = 0;
        let len = args.len();
        while i < len {
            let arg = &args[i];

            if arg == "--mobile" {
                res.is_mobile = Some(true);
            } else if arg == "--light" {
                res.light = true;
            } else if arg == "--dark" {
                res.light = false;
            } else if arg == "--debug" {
                res.debug = true;
            } else if arg == "--pub" || arg == "--npub" {
                i += 1;
                let pubstr = if let Some(next_arg) = args.get(i) {
                    next_arg
                } else {
                    error!("sec argument missing?");
                    continue;
                };

                if let Ok(pk) = Pubkey::parse(pubstr) {
                    res.keys.push(Keypair::only_pubkey(pk));
                } else {
                    error!(
                        "failed to parse {} argument. Make sure to use hex or npub.",
                        arg
                    );
                }
            } else if arg == "--sec" || arg == "--nsec" {
                i += 1;
                let secstr = if let Some(next_arg) = args.get(i) {
                    next_arg
                } else {
                    error!("sec argument missing?");
                    continue;
                };

                if let Ok(sec) = SecretKey::parse(secstr) {
                    res.keys.push(Keypair::from_secret(sec));
                } else {
                    error!(
                        "failed to parse {} argument. Make sure to use hex or nsec.",
                        arg
                    );
                }
            } else if arg == "--no-since-optimize" {
                res.since_optimize = false;
            } else if arg == "--filter" {
                i += 1;
                let filter = if let Some(next_arg) = args.get(i) {
                    next_arg
                } else {
                    error!("filter argument missing?");
                    continue;
                };

                if let Ok(filter) = Filter::from_json(filter) {
                    res.columns.push(ArgColumn::Generic(vec![filter]));
                } else {
                    error!("failed to parse filter '{}'", filter);
                }
            } else if arg == "--dbpath" {
                i += 1;
                let path = if let Some(next_arg) = args.get(i) {
                    next_arg
                } else {
                    error!("dbpath argument missing?");
                    continue;
                };
                res.dbpath = Some(path.clone());
            } else if arg == "-r" || arg == "--relay" {
                i += 1;
                let relay = if let Some(next_arg) = args.get(i) {
                    next_arg
                } else {
                    error!("relay argument missing?");
                    continue;
                };
                res.relays.push(relay.clone());
            } else if arg == "--column" || arg == "-c" {
                i += 1;
                let column_name = if let Some(next_arg) = args.get(i) {
                    next_arg
                } else {
                    error!("column argument missing");
                    continue;
                };

                if let Some(rest) = column_name.strip_prefix("contacts:") {
                    if let Ok(pubkey) = Pubkey::parse(rest) {
                        info!("contact column for user {}", pubkey.hex());
                        res.columns.push(ArgColumn::Column(ColumnKind::contact_list(
                            PubkeySource::Explicit(pubkey),
                        )))
                    } else {
                        error!("error parsing contacts pubkey {}", rest);
                        continue;
                    }
                } else if column_name == "contacts" {
                    res.columns.push(ArgColumn::Column(ColumnKind::contact_list(
                        PubkeySource::DeckAuthor,
                    )))
                } else if let Some(notif_pk_str) = column_name.strip_prefix("notifications:") {
                    if let Ok(pubkey) = Pubkey::parse(notif_pk_str) {
                        info!("got notifications column for user {}", pubkey.hex());
                        res.columns
                            .push(ArgColumn::Column(ColumnKind::notifications(
                                PubkeySource::Explicit(pubkey),
                            )))
                    } else {
                        error!("error parsing notifications pubkey {}", notif_pk_str);
                        continue;
                    }
                } else if column_name == "notifications" {
                    debug!("got notification column for default user");
                    res.columns
                        .push(ArgColumn::Column(ColumnKind::notifications(
                            PubkeySource::DeckAuthor,
                        )))
                } else if column_name == "profile" {
                    debug!("got profile column for default user");
                    res.columns.push(ArgColumn::Column(ColumnKind::profile(
                        PubkeySource::DeckAuthor,
                    )))
                } else if let Some(profile_pk_str) = column_name.strip_prefix("profile:") {
                    if let Ok(pubkey) = Pubkey::parse(profile_pk_str) {
                        info!("got profile column for user {}", pubkey.hex());
                        res.columns.push(ArgColumn::Column(ColumnKind::profile(
                            PubkeySource::Explicit(pubkey),
                        )))
                    } else {
                        error!("error parsing profile pubkey {}", profile_pk_str);
                        continue;
                    }
                }
            } else if arg == "--filter-file" || arg == "-f" {
                i += 1;
                let filter_file = if let Some(next_arg) = args.get(i) {
                    next_arg
                } else {
                    error!("filter file argument missing?");
                    continue;
                };

                let data = if let Ok(data) = std::fs::read(filter_file) {
                    data
                } else {
                    error!("failed to read filter file '{}'", filter_file);
                    continue;
                };

                if let Some(filter) = std::str::from_utf8(&data)
                    .ok()
                    .and_then(|s| Filter::from_json(s).ok())
                {
                    res.columns.push(ArgColumn::Generic(vec![filter]));
                } else {
                    error!("failed to parse filter in '{}'", filter_file);
                }
            }

            i += 1;
        }

        if res.columns.is_empty() {
            let ck = ColumnKind::contact_list(PubkeySource::DeckAuthor);
            info!("No columns set, setting up defaults: {:?}", ck);
            res.columns.push(ArgColumn::Column(ck));
        }

        res
    }
}

/// A way to define columns from the commandline. Can be column kinds or
/// generic queries
pub enum ArgColumn {
    Column(ColumnKind),
    Generic(Vec<Filter>),
}

impl ArgColumn {
    pub fn into_timeline(self, ndb: &Ndb, user: Option<&[u8; 32]>) -> Option<Timeline> {
        match self {
            ArgColumn::Generic(filters) => Some(Timeline::new(
                ColumnKind::Generic,
                FilterState::ready(filters),
            )),
            ArgColumn::Column(ck) => ck.into_timeline(ndb, user),
        }
    }
}
