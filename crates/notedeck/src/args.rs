use enostr::{Keypair, Pubkey, SecretKey};
use tracing::error;

pub struct Args {
    pub relays: Vec<String>,
    pub is_mobile: Option<bool>,
    pub keys: Vec<Keypair>,
    pub light: bool,
    pub debug: bool,
    pub relay_debug: bool,

    /// Enable when running tests so we don't panic on app startup
    pub tests: bool,

    pub use_keystore: bool,
    pub dbpath: Option<String>,
    pub datapath: Option<String>,
}

impl Args {
    pub fn parse(args: &[String]) -> Self {
        let mut res = Args {
            relays: vec![],
            is_mobile: None,
            keys: vec![],
            light: false,
            debug: false,
            relay_debug: false,
            tests: false,
            use_keystore: true,
            dbpath: None,
            datapath: None,
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
            } else if arg == "--testrunner" {
                res.tests = true;
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
                res.use_keystore = false;
            } else if arg == "--relay-debug" {
                res.relay_debug = true;
            }

            i += 1;
        }

        res
    }
}
