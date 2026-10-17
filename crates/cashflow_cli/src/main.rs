//! `cashflow` — import, inspect and export a personal finance ledger.
//!
//! So far: the data directory ([`cashflow::CashflowDir`]) and its config, the
//! journal (`journal ls`/`verify`/`rm`, `import`), the categorized books (`export`,
//! `report`), the signing key (`login`/`logout`), the vault's synced
//! settings, and the help surface. The real importers and the vault's other
//! records are later cards on headway:cashflow/flame-vault-near and
//! headway:cashflow/luggage-insane-limit.
//!
//! Parsing is [`args`], the help table [`help`], and each command its own
//! module; this file is the entry point and the dispatch.

mod args;
mod books_cmd;
mod config_cmd;
mod help;
mod journal_cmd;
mod vault_cmd;

use std::process::ExitCode;

use cashflow::CashflowDir;

use crate::args::{Command, Invocation, Run};
use crate::vault_cmd::Session;

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
        Invocation::Run(run) => execute(*run).await,
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
/// the platform default. A vault command opens a [`Session`] first.
async fn execute(run: Run) -> Result<(), String> {
    let dir = match &run.dir {
        Some(dir) => CashflowDir::new(dir),
        None => CashflowDir::from_env().map_err(|e| e.to_string())?,
    };
    match &run.command {
        Command::ConfigPath { name, json } => config_cmd::path(&dir, name.as_deref(), *json),
        Command::ConfigShow { json } => config_cmd::show(&dir, *json),
        Command::Login { nsec } => vault_cmd::login(&dir, nsec.clone()),
        Command::Logout => vault_cmd::logout(&dir),
        Command::SettingsShow { json } => vault_cmd::show(&Session::open(&dir, &run).await?, *json),
        Command::SettingsGet { name } => vault_cmd::get(&Session::open(&dir, &run).await?, name),
        Command::SettingsSet { name, value } => {
            let mut session = Session::open(&dir, &run).await?;
            vault_cmd::set(&mut session, name, value).await
        }
        Command::JournalLs { json } => journal_cmd::ls(&dir, *json),
        Command::JournalVerify { json } => journal_cmd::verify(&dir, *json),
        Command::JournalRm { hash, json } => journal_cmd::rm(&dir, hash, *json),
        Command::Import(args) => journal_cmd::import(&dir, args),
        Command::Export { books, range } => books_cmd::export(&dir, books, *range),
        Command::Report { books, report } => books_cmd::report(&dir, books, report),
    }
}
