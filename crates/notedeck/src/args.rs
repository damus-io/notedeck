use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::{DataPath, DataPathType, NotedeckOptions};
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
    pub title: Option<String>,
}

impl Args {
    /// Resolve the effective database path, respecting --dbpath override.
    pub fn db_path(&self, data_path: &DataPath) -> PathBuf {
        self.dbpath
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| data_path.path(DataPathType::Db))
    }

    /// Resolve the compact output path inside the db folder.
    pub fn db_compact_path(&self, data_path: &DataPath) -> PathBuf {
        self.db_path(data_path).join("compact")
    }

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
            title: None,
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
                res.options.set(NotedeckOptions::UseKeystore, false);
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
                res.options.set(NotedeckOptions::UseKeystore, false);
            } else if arg == "--relay-debug" {
                res.options.set(NotedeckOptions::RelayDebug, true);
            } else if arg == "--title" {
                i += 1;
                let title = if let Some(next_arg) = args.get(i) {
                    next_arg
                } else {
                    error!("title argument missing?");
                    continue;
                };
                res.title = Some(title.clone());
                res.options.set(NotedeckOptions::ShowTitle, true);
            } else {
                unrecognized_args.insert(arg.clone());
            }

            i += 1;
        }

        (res, unrecognized_args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Args {
        let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        let (parsed, unrecognized) = Args::parse(&owned);
        assert!(
            unrecognized.is_empty(),
            "expected all args to be recognized, got {unrecognized:?}"
        );
        parsed
    }

    #[test]
    fn parse_title_variants() {
        let cases = [
            (
                vec!["--title", "feature branch"],
                Some("feature branch"),
                true,
            ),
            (
                vec!["--title", "first", "--title", "second"],
                Some("second"),
                true,
            ),
            (vec!["--title"], None, false),
        ];

        for (args, expected_title, expected_show_title) in cases {
            let parsed = parse_args(&args);
            assert_eq!(parsed.title.as_deref(), expected_title);
            assert_eq!(
                parsed.options.contains(NotedeckOptions::ShowTitle),
                expected_show_title
            );
        }
    }

    /// Verifies `--no-keystore` disables OS-backed secure storage.
    #[test]
    fn parse_no_keystore_disables_keystore() {
        let (args, unrecognized) = Args::parse(&["--no-keystore".to_owned()]);

        assert!(unrecognized.is_empty());
        assert!(!args.options.contains(NotedeckOptions::UseKeystore));
    }

    /// Verifies the test runner path never touches the host keyring.
    #[test]
    fn parse_testrunner_disables_keystore() {
        let (args, unrecognized) = Args::parse(&["--testrunner".to_owned()]);

        assert!(unrecognized.is_empty());
        assert!(args.options.contains(NotedeckOptions::Tests));
        assert!(!args.options.contains(NotedeckOptions::UseKeystore));
    }
}
