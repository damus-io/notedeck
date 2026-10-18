//! `cashflow` — import, inspect and export a personal finance ledger.
//!
//! So far: the data directory ([`cashflow::CashflowDir`]) and its config, the
//! journal (`journal ls`/`status`/`verify`/`publish`/`rm`, `import`,
//! `prices fetch`), its segment manifests in the vault and `sync`, which
//! moves the segments between machines, the
//! categorized books (`export`, `report`), the signing key
//! (`login`/`logout`), the vault's synced settings and categorization rules
//! (`rule`), the per-transaction edits (`categorize`, `uncategorize`,
//! `split`, `transfer`, `edits`), the triage queue (`unknown`), the chart
//! of accounts' metadata and aliases (`account`), the goals on Headway
//! cards (`goals`), the Adventure's skills, quests and achievements
//! (`skills`, `quests`, `achievements`), and the help surface.
//! The real importers and the vault's other
//! records are later cards on headway:cashflow/flame-vault-near and
//! headway:cashflow/luggage-insane-limit.
//!
//! Parsing is [`args`], the help table [`help`], and each command its own
//! module; this file is the entry point and the dispatch.

mod account_cmd;
mod adventure_cmd;
mod args;
mod books_cmd;
mod config_cmd;
mod edit_cmd;
mod goals_cmd;
mod help;
mod journal_cmd;
mod prices_cmd;
mod rule_cmd;
mod sync_cmd;
mod vault_cmd;

use std::process::ExitCode;

use cashflow::CashflowDir;

use crate::args::{AccountCommand, Command, Invocation, Run};
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
        Command::ConfigSet { key, value } => config_cmd::set(&dir, key, value),
        Command::ConfigUnset { key } => config_cmd::unset(&dir, key),
        Command::SecretSet { name } => config_cmd::secret_set(&dir, name),
        Command::SecretRm { name } => config_cmd::secret_rm(&dir, name),
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
        Command::JournalRm { hash, json } => {
            let mut session = Session::open_if_logged_in(&dir, &run).await?;
            journal_cmd::rm(&dir, session.as_mut(), hash, *json).await
        }
        Command::JournalStatus { json } => {
            let session = Session::open_if_logged_in(&dir, &run).await?;
            journal_cmd::status(&dir, session.as_ref(), *json)
        }
        Command::JournalPublish { write } => {
            let mut session = Session::open(&dir, &run).await?;
            journal_cmd::publish(&dir, &mut session, *write).await
        }
        Command::Sync { transport, write } => {
            sync_cmd::sync(&dir, &run, transport.as_deref(), *write).await
        }
        Command::Import(args) => {
            let session = Session::open_if_logged_in(&dir, &run).await?;
            journal_cmd::import(&dir, session, args).await
        }
        Command::PricesFetch(args) => {
            let session = Session::open_if_logged_in(&dir, &run).await?;
            prices_cmd::fetch(&dir, session, args).await
        }
        Command::Export { books, range } => books_cmd::export(&dir, &run, books, *range).await,
        Command::Report { books, report } => books_cmd::report(&dir, &run, books, report).await,
        Command::Rule(cmd) => {
            let mut session = Session::open(&dir, &run).await?;
            rule_cmd::run(&dir, &mut session, cmd).await
        }
        Command::Categorize {
            record_id,
            account,
            write,
        } => {
            let mut session = Session::open(&dir, &run).await?;
            edit_cmd::categorize(&dir, &mut session, record_id, account, *write).await
        }
        Command::Uncategorize { record_id, write } => {
            let mut session = Session::open(&dir, &run).await?;
            edit_cmd::uncategorize(&dir, &mut session, record_id, *write).await
        }
        Command::Split {
            record_id,
            legs,
            write,
        } => {
            let mut session = Session::open(&dir, &run).await?;
            edit_cmd::split(&dir, &mut session, record_id, legs, *write).await
        }
        Command::Transfer(cmd) => {
            let mut session = Session::open(&dir, &run).await?;
            edit_cmd::transfer(&dir, &mut session, cmd).await
        }
        Command::Edits { by, json } => {
            edit_cmd::ls(&Session::open(&dir, &run).await?, by.as_deref(), *json)
        }
        Command::Unknown(args) => {
            edit_cmd::unknown(&dir, &Session::open(&dir, &run).await?, args).await
        }
        Command::Account(AccountCommand::Ls {
            books,
            account,
            depth,
            all,
            json,
        }) => {
            let args = account_cmd::LsArgs {
                books,
                account: account.as_deref(),
                depth: *depth,
                all: *all,
                json: *json,
            };
            account_cmd::ls(&dir, &run, args).await
        }
        Command::Account(AccountCommand::Set {
            account,
            set,
            write,
        }) => {
            let mut session = Session::open(&dir, &run).await?;
            account_cmd::set(&mut session, account, set, *write).await
        }
        Command::Account(AccountCommand::Alias { from, to, write }) => {
            let mut session = Session::open(&dir, &run).await?;
            account_cmd::alias(&dir, &mut session, from, Some(to), *write).await
        }
        Command::Account(AccountCommand::Unalias { from, write }) => {
            let mut session = Session::open(&dir, &run).await?;
            account_cmd::alias(&dir, &mut session, from, None, *write).await
        }
        Command::Goals(args) => goals_cmd::ls(&dir, args),
        Command::Skills(args) => adventure_cmd::skills(&dir, &run, args).await,
        Command::Quests(args) => adventure_cmd::quests(&dir, &run, args).await,
        Command::Achievements(args) => adventure_cmd::achievements(&dir, &run, args).await,
    }
}
