use std::collections::BTreeSet;

use crate::timeline::TimelineKind;
use enostr::{Filter, Pubkey};
use oot_bitset::{bitset_clear, bitset_get, bitset_set};
use tracing::{debug, error, info};

#[repr(u16)]
pub enum ColumnsFlag {
    SinceOptimize,
    Textmode,
    Scramble,
    NoMedia,
    ShowNoteClientTop,
    ShowNoteClientBottom,
}

pub struct ColumnsArgs {
    pub columns: Vec<ArgColumn>,
    flags: [u16; 2],
}

impl ColumnsArgs {
    pub fn is_flag_set(&self, flag: ColumnsFlag) -> bool {
        bitset_get(&self.flags, flag as u16)
    }

    pub fn set_flag(&mut self, flag: ColumnsFlag) {
        bitset_set(&mut self.flags, flag as u16)
    }

    pub fn clear_flag(&mut self, flag: ColumnsFlag) {
        bitset_clear(&mut self.flags, flag as u16)
    }

    pub fn parse(args: &[String], deck_author: Option<&Pubkey>) -> (Self, BTreeSet<String>) {
        let mut unrecognized_args = BTreeSet::new();
        let mut res = Self {
            columns: vec![],
            flags: [0; 2],
        };

        // flag defaults
        res.set_flag(ColumnsFlag::SinceOptimize);

        let mut i = 0;
        let len = args.len();
        while i < len {
            let arg = &args[i];

            if arg == "--textmode" {
                res.set_flag(ColumnsFlag::Textmode);
            } else if arg == "--no-since-optimize" {
                res.clear_flag(ColumnsFlag::SinceOptimize);
            } else if arg == "--scramble" {
                res.set_flag(ColumnsFlag::Scramble);
            } else if arg == "--show-note-client=top" {
                res.set_flag(ColumnsFlag::ShowNoteClientTop);
            } else if arg == "--show-note-client=bottom" {
                res.set_flag(ColumnsFlag::ShowNoteClientBottom);
            } else if arg == "--no-media" {
                res.set_flag(ColumnsFlag::NoMedia);
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
                            )));
                    } else {
                        panic!(
                            "No accounts available, could not handle implicit pubkey contacts column"
                        );
                    }
                } else if column_name == "search" {
                    i += 1;
                    let search = if let Some(next_arg) = args.get(i) {
                        next_arg
                    } else {
                        error!("search filter argument missing?");
                        continue;
                    };

                    res.columns.push(ArgColumn::Timeline(TimelineKind::search(
                        search.to_string(),
                    )));
                } else if let Some(notif_pk_str) = column_name.strip_prefix("notifications:") {
                    if let Ok(pubkey) = Pubkey::parse(notif_pk_str) {
                        info!("got notifications column for user {}", pubkey.hex());
                        res.columns
                            .push(ArgColumn::Timeline(TimelineKind::notifications(pubkey)));
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
#[derive(Debug, Clone)]
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
