use std::collections::BTreeSet;

use crate::NotedeckOptions;
use enostr::{Keypair, Pubkey, SecretKey};
use tracing::error;
use unic_langid::{LanguageIdentifier, LanguageIdentifierError};

pub struct Args {
    pub relays: Vec<String>,
    pub locale: Option<LanguageIdentifier>,
    pub keys: Vec<Keypair>,
    pub options: NotedeckOptions,
    pub dbpath: Option<String>,
    pub datapath: Option<String>,
}

impl Args {
    // parse arguments, return set of unrecognized args
    pub fn parse(args: &[String]) -> (Self, BTreeSet<String>) {
        let mut unrecognized_args = BTreeSet::new();
        let mut res = Args {
            relays: vec![],
            keys: vec![],
            options: NotedeckOptions::default(),
            dbpath: None,
            datapath: None,
            locale: None,
        };

        let mut i = 0;
        let len = args.len();
        while i < len {
            let arg = &args[i];

            if arg == "--mobile" {
                res.options.set(NotedeckOptions::Mobile, true);
            } else if arg == "--light" {
                res.options.set(NotedeckOptions::LightTheme, true);
            } else if arg == "--locale" {
                i += 1;
                let Some(locale) = args.get(i) else {
                    panic!("locale argument missing?");
                };
                let parsed: Result<LanguageIdentifier, LanguageIdentifierError> = locale.parse();
                match parsed {
                    Err(err) => {
                        panic!("locale failed to parse: {err}");
                    }
                    Ok(locale) => {
                        tracing::info!(
                            "parsed locale '{locale}' from args, not sure if we have it yet though."
                        );
                        res.locale = Some(locale);
                    }
                }
            } else if arg == "--dark" {
                res.options.set(NotedeckOptions::LightTheme, false);
            } else if arg == "--debug" {
                res.options.set(NotedeckOptions::Debug, true);
            } else if arg == "--testrunner" {
                res.options.set(NotedeckOptions::Tests, true);
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
            } else if arg == "--dbpath" {
                i += 1;
                let path = if let Some(next_arg) = args.get(i) {
                    next_arg
                } else {
                    error!("dbpath argument missing?");
                    continue;
                };
                res.dbpath = Some(path.clone());
            } else if arg == "--datapath" {
                i += 1;
                let path = if let Some(next_arg) = args.get(i) {
                    next_arg
                } else {
                    error!("datapath argument missing?");
                    continue;
                };
                res.datapath = Some(path.clone());
            } else if arg == "-r" || arg == "--relay" {
                i += 1;
                let relay = if let Some(next_arg) = args.get(i) {
                    next_arg
                } else {
                    error!("relay argument missing?");
                    continue;
                };
                res.relays.push(relay.clone());
            } else if arg == "--no-keystore" {
                res.options.set(NotedeckOptions::UseKeystore, true);
            } else if arg == "--relay-debug" {
                res.options.set(NotedeckOptions::RelayDebug, true);
            } else if arg == "--notebook" {
                res.options.set(NotedeckOptions::FeatureNotebook, true);
            } else {
                unrecognized_args.insert(arg.clone());
            }

            i += 1;
        }

        (res, unrecognized_args)
    }
}
