use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use crate::{DataPath, DataPathType, NotedeckOptions};
use nostrdb_net::{Keypair, Pubkey, SecretKey};
use tracing::error;
use unic_langid::{LanguageIdentifier, LanguageIdentifierError};

/// Default bind address for the embedded localhost nostr relay.
pub const DEFAULT_LOCAL_RELAY_ADDR: &str = "127.0.0.1:6677";

/// How long a `--serve` notedeck waits without a socket request before it
/// exits, unless `--idle-exit <secs>` says otherwise.
pub const DEFAULT_IDLE_EXIT: Duration = Duration::from_secs(10 * 60);

/// What [`Args::parse`] makes of a command line.
pub struct ParsedArgs {
    /// The arguments notedeck recognized.
    pub args: Args,
    /// The arguments it did not, which apps may read.
    pub unrecognized: BTreeSet<String>,
}

pub struct Args {
    pub relays: Vec<String>,
    pub locale: Option<LanguageIdentifier>,
    pub keys: Vec<Keypair>,
    pub options: NotedeckOptions,
    pub dbpath: Option<String>,
    pub datapath: Option<String>,
    pub title: Option<String>,
    /// Bind address for the embedded localhost nostr relay. Defaults to
    /// `127.0.0.1:6677`; `--relay-bind <addr>` overrides it and
    /// `--no-local-relay` disables the relay (`None`).
    pub local_relay: Option<String>,
    /// How long a `--serve` notedeck goes without a socket request before it
    /// exits: [`DEFAULT_IDLE_EXIT`] unless `--idle-exit <secs>` is passed.
    /// Nothing else reads it.
    pub idle_exit: Duration,
}

impl Args {
    /// Resolve the effective database path, respecting --dbpath override.
    pub fn db_path(&self, data_path: &DataPath) -> PathBuf {
        self.dbpath
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| data_path.path(DataPathType::Db))
    }

    /// Resolve the prune output path inside the db folder.
    ///
    /// The directory is still literally `compact/`: the feature was renamed
    /// after it shipped, and a database already staged there has to keep
    /// getting swapped in.
    pub fn db_prune_path(&self, data_path: &DataPath) -> PathBuf {
        self.db_path(data_path).join("compact")
    }

    /// Parse `args`, keeping the ones notedeck does not know for apps to read.
    pub fn parse(args: &[String]) -> ParsedArgs {
        let mut unrecognized_args = BTreeSet::new();
        let mut res = Args {
            relays: vec![],
            keys: vec![],
            options: NotedeckOptions::default(),
            dbpath: None,
            datapath: None,
            locale: None,
            title: None,
            local_relay: Some(DEFAULT_LOCAL_RELAY_ADDR.to_owned()),
            idle_exit: DEFAULT_IDLE_EXIT,
        };
        // `--serve` gives up the local relay unless one of these asks for it.
        let mut relay_chosen = false;

        let mut i = 0;
        let len = args.len();
        while i < len {
            let arg = &args[i];

            if arg == "--light" {
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
            } else if arg == "--no-local-relay" {
                res.local_relay = None;
                relay_chosen = true;
            } else if arg == "--relay-bind" {
                i += 1;
                let Some(addr) = args.get(i) else {
                    error!("relay-bind argument missing?");
                    continue;
                };
                res.local_relay = Some(addr.clone());
                relay_chosen = true;
            } else if arg == "--use-keystore" {
                res.options.set(NotedeckOptions::UseKeystore, true);
            } else if arg == "--all-apps-active" {
                res.options.set(NotedeckOptions::AllAppsActive, true);
            } else if arg == "--headless" {
                // Headless implies all-apps-active: with no display stack there
                // is no active app to render, so every app's update() loop runs.
                res.options.set(
                    NotedeckOptions::Headless | NotedeckOptions::AllAppsActive,
                    true,
                );
            } else if arg == "--serve" {
                // A notedeck `nd` starts: headless, data apps only. It is
                // the fallback for a user who has no notedeck open, so it
                // must not hold the local relay's port against the one they
                // open next.
                res.options.set(
                    NotedeckOptions::Serve
                        | NotedeckOptions::Headless
                        | NotedeckOptions::AllAppsActive,
                    true,
                );
            } else if arg == "--idle-exit" {
                i += 1;
                let Some(secs) = args.get(i) else {
                    error!("idle-exit argument missing?");
                    continue;
                };
                match secs.parse::<u64>() {
                    Ok(secs) => res.idle_exit = Duration::from_secs(secs),
                    Err(err) => error!("--idle-exit takes whole seconds, not '{secs}': {err}"),
                }
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

        if res.options.contains(NotedeckOptions::Serve) && !relay_chosen {
            res.local_relay = None;
        }

        ParsedArgs {
            args: res,
            unrecognized: unrecognized_args,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Args {
        let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        let ParsedArgs {
            args: parsed,
            unrecognized,
        } = Args::parse(&owned);
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

    /// The OS keychain is off by default; secrets go to file-based storage.
    #[test]
    fn parse_keystore_off_by_default() {
        let ParsedArgs { args, unrecognized } = Args::parse(&[]);

        assert!(unrecognized.is_empty());
        assert!(!args.options.contains(NotedeckOptions::UseKeystore));
    }

    /// Verifies `--use-keystore` opts into OS-backed secure storage.
    #[test]
    fn parse_use_keystore_enables_keystore() {
        let ParsedArgs { args, unrecognized } = Args::parse(&["--use-keystore".to_owned()]);

        assert!(unrecognized.is_empty());
        assert!(args.options.contains(NotedeckOptions::UseKeystore));
    }

    /// `--serve` is a headless run that gives up the local relay, and keeps
    /// one only when asked for it by name.
    #[test]
    fn parse_serve_is_headless_without_the_local_relay() {
        let args = parse_args(&["--serve"]);
        assert!(args.options.contains(
            NotedeckOptions::Serve | NotedeckOptions::Headless | NotedeckOptions::AllAppsActive
        ));
        assert_eq!(args.local_relay, None);
        assert_eq!(args.idle_exit, DEFAULT_IDLE_EXIT);

        let args = parse_args(&["--relay-bind", "127.0.0.1:7000", "--serve"]);
        assert_eq!(args.local_relay.as_deref(), Some("127.0.0.1:7000"));

        let args = parse_args(&["--headless", "--serve", "--idle-exit", "3"]);
        assert_eq!(args.idle_exit, Duration::from_secs(3));

        let args = parse_args(&["--headless"]);
        assert!(!args.options.contains(NotedeckOptions::Serve));
        assert_eq!(args.local_relay.as_deref(), Some(DEFAULT_LOCAL_RELAY_ADDR));
    }

    /// Verifies the test runner path never touches the host keyring.
    #[test]
    fn parse_testrunner_disables_keystore() {
        let ParsedArgs { args, unrecognized } = Args::parse(&["--testrunner".to_owned()]);

        assert!(unrecognized.is_empty());
        assert!(args.options.contains(NotedeckOptions::Tests));
        assert!(!args.options.contains(NotedeckOptions::UseKeystore));
    }
}
