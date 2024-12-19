use notedeck::FilterState;

use crate::timeline::{PubkeySource, Timeline, TimelineKind, TimelineTab};
use enostr::{Filter, Pubkey};
use nostrdb::Ndb;
use tracing::{debug, error, info};

pub struct ColumnsArgs {
    pub columns: Vec<ArgColumn>,
    pub since_optimize: bool,
    pub textmode: bool,
}

impl ColumnsArgs {
    pub fn parse(args: &[String]) -> Self {
        let mut res = Self {
            columns: vec![],
            since_optimize: true,
            textmode: false,
        };

        let mut i = 0;
        let len = args.len();
        while i < len {
            let arg = &args[i];

            if arg == "--textmode" {
                res.textmode = true;
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
                        res.columns
                            .push(ArgColumn::Timeline(TimelineKind::contact_list(
                                PubkeySource::Explicit(pubkey),
                            )))
                    } else {
                        error!("error parsing contacts pubkey {}", rest);
                        continue;
                    }
                } else if column_name == "contacts" {
                    res.columns
                        .push(ArgColumn::Timeline(TimelineKind::contact_list(
                            PubkeySource::DeckAuthor,
                        )))
                } else if let Some(notif_pk_str) = column_name.strip_prefix("notifications:") {
                    if let Ok(pubkey) = Pubkey::parse(notif_pk_str) {
                        info!("got notifications column for user {}", pubkey.hex());
                        res.columns
                            .push(ArgColumn::Timeline(TimelineKind::notifications(
                                PubkeySource::Explicit(pubkey),
                            )))
                    } else {
                        error!("error parsing notifications pubkey {}", notif_pk_str);
                        continue;
                    }
                } else if column_name == "notifications" {
                    debug!("got notification column for default user");
                    res.columns
                        .push(ArgColumn::Timeline(TimelineKind::notifications(
                            PubkeySource::DeckAuthor,
                        )))
                } else if column_name == "profile" {
                    debug!("got profile column for default user");
                    res.columns.push(ArgColumn::Timeline(TimelineKind::profile(
                        PubkeySource::DeckAuthor,
                    )))
                } else if column_name == "universe" {
                    debug!("got universe column");
                    res.columns
                        .push(ArgColumn::Timeline(TimelineKind::Universe))
                } else if let Some(profile_pk_str) = column_name.strip_prefix("profile:") {
                    if let Ok(pubkey) = Pubkey::parse(profile_pk_str) {
                        info!("got profile column for user {}", pubkey.hex());
                        res.columns.push(ArgColumn::Timeline(TimelineKind::profile(
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

        res
    }
}

/// A way to define columns from the commandline. Can be column kinds or
/// generic queries
#[derive(Debug)]
pub enum ArgColumn {
    Timeline(TimelineKind),
    Generic(Vec<Filter>),
}

impl ArgColumn {
    pub fn into_timeline(self, ndb: &Ndb, user: Option<&[u8; 32]>) -> Option<Timeline> {
        match self {
            ArgColumn::Generic(filters) => Some(Timeline::new(
                TimelineKind::Generic,
                FilterState::ready(filters),
                TimelineTab::full_tabs(),
            )),
            ArgColumn::Timeline(tk) => tk.into_timeline(ndb, user),
        }
    }
}
