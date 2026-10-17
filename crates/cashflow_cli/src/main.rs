//! `cashflow` — import, inspect and export a personal finance ledger.
//!
//! Only the skeleton so far: the data directory ([`cashflow::CashflowDir`]),
//! its config, and the help surface. The journal, importers, export and the
//! nostr vault are later cards on headway:cashflow/flame-vault-near and
//! headway:cashflow/luggage-insane-limit.
//!
//! Parsing is [`args`], the help table [`help`], and each command its own
//! module; this file is the entry point and the dispatch.

mod args;
mod config_cmd;
mod help;

use std::process::ExitCode;

use cashflow::CashflowDir;

use crate::args::{Command, Invocation, Run};

#[tokio::main]
async fn main() -> ExitCode {
    // Terminate quietly on a closed pipe (`cashflow config show | head`)
    // instead of panicking in println! on EPIPE.
    nostrdb_net::relay::sync::reset_sigpipe();
    // Select the rustls CryptoProvider before any wss:// handshake: the vault's
    // relay sync needs it, and a standalone CLI never runs notedeck's init.
    enostr::install_crypto();

    let invocation = match args::parse(std::env::args().skip(1)) {
        Ok(invocation) => invocation,
        Err(message) => {
            eprintln!("cashflow: {message}");
            return ExitCode::FAILURE;
        }
    };
    let result = match invocation {
        Invocation::Usage => {
            eprint!("{}", help::CLI.overview(help::GLOBALS));
            Ok(())
        }
        Invocation::CommandHelp(cmd) => {
            eprint!("{}", help::CLI.page(cmd));
            Ok(())
        }
        Invocation::Options => {
            eprint!(
                "{}",
                help::CLI.options_page(help::GLOBALS, help::OPTIONS_FOOTER)
            );
            Ok(())
        }
        Invocation::Run(run) => execute(run),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("cashflow: {message}");
            ExitCode::FAILURE
        }
    }
}

/// Run one command against the data dir: `--dir`, else `$CASHFLOW_DIR`, else
/// the platform default.
fn execute(run: Run) -> Result<(), String> {
    let dir = match run.dir {
        Some(dir) => CashflowDir::new(dir),
        None => CashflowDir::from_env().map_err(|e| e.to_string())?,
    };
    match run.command {
        Command::ConfigPath { name, json } => config_cmd::path(&dir, name.as_deref(), json),
        Command::ConfigShow { json } => config_cmd::show(&dir, json),
    }
}
