use std::collections::BTreeSet;

use crate::timeline::TimelineKind;
use enostr::{Filter, Pubkey};
use tracing::{debug, error, info};

pub struct ColumnsArgs {
    pub columns: Vec<ArgColumn>,
    pub since_optimize: bool,
    pub textmode: bool,
    pub scramble: bool,
}

impl ColumnsArgs {
    pub fn parse(args: &[String], deck_author: Option<&Pubkey>) -> (Self, BTreeSet<String>) {
        let mut unrecognized_args = BTreeSet::new();
        let mut res = Self {
            columns: vec![],
            since_optimize: true,
            textmode: false,
            scramble: false,
        };

        let mut i = 0;
        let len = args.len();
        while i < len {
            let arg = &args[i];

            if arg == "--textmode" {
                res.textmode = true;
            } else if arg == "--no-since-optimize" {
                res.since_optimize = false;
            } else if arg == "--scramble" {
                res.scramble = true;
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
                            .push(ArgColumn::Timeline(TimelineKind::contact_list(pubkey)))
                    } else {
                        error!("error parsing contacts pubkey {}", rest);
                        continue;
                    }
                } else if column_name == "contacts" {
                    if let Some(deck_author) = deck_author {
                        res.columns
                            .push(ArgColumn::Timeline(TimelineKind::contact_list(
                                deck_author.to_owned(),
                            )))
                    } else {
                        panic!("No accounts available, could not handle implicit pubkey contacts column")
                    }
                } else if let Some(notif_pk_str) = column_name.strip_prefix("notifications:") {
                    if let Ok(pubkey) = Pubkey::parse(notif_pk_str) {
                        info!("got notifications column for user {}", pubkey.hex());
                        res.columns
                            .push(ArgColumn::Timeline(TimelineKind::notifications(pubkey)))
                    } else {
                        error!("error parsing notifications pubkey {}", notif_pk_str);
                        continue;
                    }
                } else if column_name == "notifications" {
                    debug!("got notification column for default user");
                    if let Some(deck_author) = deck_author {
                        res.columns
                            .push(ArgColumn::Timeline(TimelineKind::notifications(
                                deck_author.to_owned(),
                            )));
                    } else {
                        panic!("Tried to push notifications timeline with no available users");
                    }
                } else if column_name == "profile" {
                    debug!("got profile column for default user");
                    if let Some(deck_author) = deck_author {
                        res.columns.push(ArgColumn::Timeline(TimelineKind::profile(
                            deck_author.to_owned(),
                        )));
                    } else {
                        panic!("Tried to push profile timeline with no available users");
                    }
                } else if column_name == "universe" {
                    debug!("got universe column");
                    res.columns
                        .push(ArgColumn::Timeline(TimelineKind::Universe))
                } else if let Some(profile_pk_str) = column_name.strip_prefix("profile:") {
                    if let Ok(pubkey) = Pubkey::parse(profile_pk_str) {
                        info!("got profile column for user {}", pubkey.hex());
                        res.columns
                            .push(ArgColumn::Timeline(TimelineKind::profile(pubkey)))
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
            } else {
                unrecognized_args.insert(arg.clone());
            }

            i += 1;
        }

        (res, unrecognized_args)
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
    pub fn into_timeline_kind(self) -> TimelineKind {
        match self {
            ArgColumn::Generic(_filters) => {
                // TODO: fix generic filters by referencing some filter map
                TimelineKind::Generic(0)
            }
            ArgColumn::Timeline(tk) => tk,
        }
    }
}
