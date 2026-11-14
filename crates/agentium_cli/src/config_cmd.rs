//! `agentium config` — list, show, add, edit and remove run configs.
//!
//! A run config is what Dave's per-session run bar launches: a named shell
//! command (`cargo run`, say) registered for one host and working directory, and
//! run there with `sh -c`. Each is its own kind-31991 event (d-tag = the
//! config's UUID), so an edit is a newer revision of the same d-tag and a delete
//! is a newer revision carrying a `deleted` tag.
//!
//! The desktop only ever shows the configs of its own host, for the exact cwd of
//! the session in view. The CLI sees every host: `list` is the whole set, grouped
//! host → cwd, and a selector resolves across all of it.

use std::io::IsTerminal;

use agentium_core::Engine;
pub(crate) use agentium_core::cli::config::ConfigFilters;
use agentium_core::cli::config::{
    ConfigJson, pick_place, render_list, render_published, render_show, replacing, required_field,
    resolve_config,
};
use agentium_core::config::RunConfig;
use agentium_core::session_events::{build_run_config_delete_event_at, build_run_config_event_at};
use agentium_core::session_loader::{HostedRunConfig, load_all_run_configs_from_ndb};
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use crate::publish::{flush_publish, json_line};
use crate::spawn::current_session;
use crate::term::now_secs;

/// The `config` subcommand, as parsed.
pub(crate) enum ConfigAction {
    /// Every config the filters keep, grouped host → cwd.
    List,
    /// One config in full.
    Show { selector: String },
    /// Register a new config on a host+cwd (the current session's by default).
    Add { name: String, command: String },
    /// Rename a config or change its command, keeping its id.
    Edit {
        selector: String,
        name: Option<String>,
        command: Option<String>,
    },
    /// Delete a config.
    Rm { selector: String },
}

impl ConfigAction {
    /// Parse `config <sub> [args]`. A bare `config` lists. `--name`/`--command`
    /// arrive already pulled out of the flag loop; they are required by `add`,
    /// optional (but at least one) for `edit`, and rejected elsewhere.
    pub(crate) fn parse(
        rest: &[String],
        name: Option<String>,
        command: Option<String>,
    ) -> Result<Self> {
        let sub = rest.first().map(String::as_str).unwrap_or("list");
        let selector = || -> Result<String> {
            rest.get(1)
                .cloned()
                .ok_or_else(|| format!("`config {sub}` needs a config id or name").into())
        };
        let no_fields = |name: &Option<String>, command: &Option<String>| -> Result<()> {
            if name.is_some() || command.is_some() {
                return Err("--name/--command only apply to `config add`/`config edit`".into());
            }
            Ok(())
        };

        Ok(match sub {
            "list" | "ls" => {
                no_fields(&name, &command)?;
                ConfigAction::List
            }
            "show" => {
                no_fields(&name, &command)?;
                ConfigAction::Show {
                    selector: selector()?,
                }
            }
            "rm" | "remove" | "delete" => {
                no_fields(&name, &command)?;
                ConfigAction::Rm {
                    selector: selector()?,
                }
            }
            "add" => ConfigAction::Add {
                name: required_field("--name", name.as_deref())?,
                command: required_field("--command", command.as_deref())?,
            },
            "edit" => {
                let name = name
                    .map(|n| required_field("--name", Some(&n)))
                    .transpose()?;
                let command = command
                    .map(|c| required_field("--command", Some(&c)))
                    .transpose()?;
                if name.is_none() && command.is_none() {
                    return Err("`config edit` needs --name and/or --command".into());
                }
                ConfigAction::Edit {
                    selector: selector()?,
                    name,
                    command,
                }
            }
            other => {
                return Err(format!(
                    "unknown config subcommand '{other}' (list | show | add | edit | rm)"
                )
                .into());
            }
        })
    }

    /// Whether the action publishes, and so needs the relay (see
    /// `Command::needs_relay`).
    pub(crate) fn publishes(&self) -> bool {
        !matches!(self, ConfigAction::List | ConfigAction::Show { .. })
    }
}

/// `agentium config …` — dispatch one [`ConfigAction`].
///
/// `secret` signs what `add`/`edit`/`rm` publish; it is the same key the engine
/// was opened with, which keeps its own copy private.
pub(crate) async fn cmd_config(
    engine: &Engine,
    author: &Pubkey,
    secret: &[u8; 32],
    action: &ConfigAction,
    filters: &ConfigFilters,
    as_json: bool,
) -> Result<()> {
    // `add` reads nothing: its --host/--cwd name a place rather than filter one.
    if let ConfigAction::Add { name, command } = action {
        let current = current_session(engine, author)?;
        let hosted = HostedRunConfig {
            hostname: pick_place(filters.host.as_deref(), current.host, "--host")?,
            cwd: pick_place(filters.cwd.as_deref(), current.cwd, "--cwd")?.into(),
            config: RunConfig::new(name.clone(), command.clone()),
        };
        let built = build_run_config_event_at(
            &hosted.config,
            &hosted.cwd.to_string_lossy(),
            &hosted.hostname,
            None,
            secret,
        )?;
        return publish(engine, "added", &hosted, &built, as_json).await;
    }

    let configs = {
        let txn = Transaction::new(engine.ndb())?;
        let mut all = load_all_run_configs_from_ndb(engine.ndb(), &txn, author);
        all.retain(|c| filters.matches(c));
        all
    };

    match action {
        ConfigAction::Add { .. } => unreachable!("handled above"),
        ConfigAction::List => list(&configs, as_json),
        ConfigAction::Show { selector } => show(resolve_config(&configs, selector)?, as_json),
        ConfigAction::Edit {
            selector,
            name,
            command,
        } => {
            let mut hosted = resolve_config(&configs, selector)?.clone();
            if let Some(name) = name {
                hosted.config.name = name.clone();
            }
            if let Some(command) = command {
                hosted.config.command = command.clone();
            }
            let built = build_run_config_event_at(
                &hosted.config,
                &hosted.cwd.to_string_lossy(),
                &hosted.hostname,
                Some(replacing(hosted.config.updated_at)),
                secret,
            )?;
            publish(engine, "updated", &hosted, &built, as_json).await
        }
        ConfigAction::Rm { selector } => {
            let hosted = resolve_config(&configs, selector)?;
            let built = build_run_config_delete_event_at(
                &hosted.config.id,
                &hosted.cwd.to_string_lossy(),
                &hosted.hostname,
                Some(replacing(hosted.config.updated_at)),
                secret,
            )?;
            publish(engine, "removed", hosted, &built, as_json).await
        }
    }
}

/// `config list`: grouped host → cwd → one row per config, or with `--json` the
/// flat array (already in host, cwd, name order).
fn list(configs: &[HostedRunConfig], as_json: bool) -> Result<()> {
    if as_json {
        let rows: Vec<ConfigJson> = configs.iter().map(ConfigJson::new).collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    print!("{}", render_list(configs, std::io::stdout().is_terminal()));
    Ok(())
}

/// `config show`: every field of one config.
fn show(c: &HostedRunConfig, as_json: bool) -> Result<()> {
    if as_json {
        println!("{}", serde_json::to_string_pretty(&ConfigJson::new(c))?);
        return Ok(());
    }
    print!("{}", render_show(c, now_secs()));
    Ok(())
}

/// Publish a built kind-31991 event, wait for it to leave, and report it.
async fn publish(
    engine: &Engine,
    verb: &str,
    c: &HostedRunConfig,
    built: &agentium_core::session_events::BuiltEvent,
    as_json: bool,
) -> Result<()> {
    engine.publish_event(built)?;
    flush_publish(engine).await;

    let event_id = hex::encode(built.note_id);
    if as_json {
        let obj = serde_json::json!({
            "action": verb,
            "event_id": event_id,
            "config": ConfigJson::new(c),
        });
        println!("{}", json_line(&obj)?);
        return Ok(());
    }
    print!("{}", render_published(verb, c, &event_id));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parse_defaults_to_list_and_checks_fields() {
        assert!(matches!(
            ConfigAction::parse(&[], None, None).unwrap(),
            ConfigAction::List
        ));

        match ConfigAction::parse(
            &strings(&["add"]),
            Some("  build ".into()),
            Some("cargo build".into()),
        )
        .unwrap()
        {
            ConfigAction::Add { name, command } => {
                assert_eq!(name, "build", "trimmed like the desktop editor");
                assert_eq!(command, "cargo build");
            }
            _ => panic!("expected Add"),
        }

        let blank = ConfigAction::parse(&strings(&["add"]), Some(" ".into()), Some("x".into()));
        assert!(blank.is_err(), "a blank name is refused");
        let missing = ConfigAction::parse(&strings(&["add"]), Some("n".into()), None);
        assert!(missing.is_err(), "add needs --command");
        let bare_edit = ConfigAction::parse(&strings(&["edit", "abc"]), None, None);
        assert!(bare_edit.is_err(), "edit needs something to change");
        let stray = ConfigAction::parse(&strings(&["rm", "abc"]), Some("n".into()), None);
        assert!(stray.is_err(), "--name means nothing to rm");
        assert!(ConfigAction::parse(&strings(&["rm"]), None, None).is_err());
        assert!(ConfigAction::parse(&strings(&["frob"]), None, None).is_err());
    }
}
