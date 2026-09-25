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
use agentium_core::config::RunConfig;
use agentium_core::session_events::{build_run_config_delete_event_at, build_run_config_event_at};
use agentium_core::session_loader::{HostedRunConfig, load_all_run_configs_from_ndb};
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use crate::publish::{flush_publish, json_line};
use crate::spawn::current_session;
use crate::term::{SGR_BOLD, col, now_secs, paint, relative_time};

/// Width of the name column in `config list`.
const NAME_W: usize = 16;

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
                name: required_field("--name", name)?,
                command: required_field("--command", command)?,
            },
            "edit" => {
                let name = name
                    .map(|n| required_field("--name", Some(n)))
                    .transpose()?;
                let command = command
                    .map(|c| required_field("--command", Some(c)))
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

/// A trimmed, non-empty `--name`/`--command`, mirroring the desktop editor,
/// which won't save a config with either blank.
fn required_field(flag: &str, value: Option<String>) -> Result<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| format!("{flag} needs a non-empty value").into())
}

/// Case-insensitive substring filters over host and cwd — the global
/// `--host`/`--cwd` flags, which read the same way for `list`. `add` reads them
/// instead as the exact place to register the config.
pub(crate) struct ConfigFilters {
    pub(crate) host: Option<String>,
    pub(crate) cwd: Option<String>,
}

impl ConfigFilters {
    fn matches(&self, c: &HostedRunConfig) -> bool {
        let contains = |hay: &str, needle: &Option<String>| {
            needle
                .as_ref()
                .is_none_or(|n| hay.to_lowercase().contains(&n.to_lowercase()))
        };
        contains(&c.hostname, &self.host) && contains(&c.cwd.to_string_lossy(), &self.cwd)
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
            hostname: pick_place(&filters.host, current.host, "--host")?,
            cwd: pick_place(&filters.cwd, current.cwd, "--cwd")?.into(),
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

/// The `created_at` for a revision replacing one stamped `prev`: now, or one
/// second past `prev` if the clock hasn't moved beyond it. A same-second
/// revision would tie, and a tie goes to whichever the fold sees first.
fn replacing(prev: u64) -> u64 {
    now_secs().max(prev + 1)
}

/// `add`'s host or cwd: the flag, else the current session's, else an error
/// naming the flag.
fn pick_place(flag: &Option<String>, current: Option<String>, name: &str) -> Result<String> {
    flag.clone()
        .or(current)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            format!(
                "no {} — pass {name} (or run inside a session so $AGENTIUM_SESSION supplies it)",
                &name[2..]
            )
            .into()
        })
}

/// Resolve `selector` to one config: an exact id, else the single config whose
/// id starts with it (case-insensitive) or whose name is exactly it. None, or
/// more than one, is an error listing what it could have meant.
fn resolve_config<'c>(
    configs: &'c [HostedRunConfig],
    selector: &str,
) -> Result<&'c HostedRunConfig> {
    if let Some(exact) = configs.iter().find(|c| c.config.id == selector) {
        return Ok(exact);
    }
    let prefix = selector.to_ascii_lowercase();
    let is_match =
        |c: &&HostedRunConfig| c.config.id.starts_with(&prefix) || c.config.name == selector;

    let mut matches = configs.iter().filter(is_match);
    let first = matches.next();
    if let (Some(only), None) = (first, matches.next()) {
        return Ok(only);
    }
    if first.is_none() {
        return Err(format!(
            "no run config matches '{selector}' (by id prefix or exact name) — see `agentium config list`"
        )
        .into());
    }
    let candidates = configs
        .iter()
        .filter(is_match)
        .map(describe)
        .collect::<Vec<_>>()
        .join("\n  ");
    Err(format!(
        "'{selector}' matches more than one run config — pass more of the id, or narrow with \
         --host/--cwd:\n  {candidates}"
    )
    .into())
}

/// One line naming a config: short id, name, and where it lives.
fn describe(c: &HostedRunConfig) -> String {
    format!(
        "{} {} ({}:{})",
        short_id(&c.config.id),
        c.config.name,
        c.hostname,
        c.cwd.display()
    )
}

/// A config id's first 8 characters — enough to select it by prefix.
fn short_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// The `--json` view of a config.
#[derive(serde::Serialize)]
struct ConfigJson<'a> {
    id: &'a str,
    name: &'a str,
    command: &'a str,
    host: &'a str,
    cwd: std::borrow::Cow<'a, str>,
    updated_at: u64,
}

impl<'a> ConfigJson<'a> {
    fn new(c: &'a HostedRunConfig) -> Self {
        ConfigJson {
            id: &c.config.id,
            name: &c.config.name,
            command: &c.config.command,
            host: &c.hostname,
            cwd: c.cwd.to_string_lossy(),
            updated_at: c.config.updated_at,
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
    if configs.is_empty() {
        println!("no run configs");
        return Ok(());
    }
    print!("{}", render_list(configs, std::io::stdout().is_terminal()));
    Ok(())
}

/// The text form of `config list`. `configs` arrives sorted by host then cwd,
/// so a header is due whenever either changes from the row before.
fn render_list(configs: &[HostedRunConfig], color: bool) -> String {
    let mut out = String::new();
    let mut prev: Option<&HostedRunConfig> = None;
    for c in configs {
        let new_host = prev.is_none_or(|p| p.hostname != c.hostname);
        if new_host {
            if prev.is_some() {
                out.push('\n');
            }
            let host = if c.hostname.is_empty() {
                "(no host)"
            } else {
                &c.hostname
            };
            out.push_str(&paint(color, SGR_BOLD, host));
            out.push('\n');
        }
        if new_host || prev.is_some_and(|p| p.cwd != c.cwd) {
            out.push_str(&format!("  {}\n", c.cwd.display()));
        }
        out.push_str(&format!(
            "    {}  {}  {}\n",
            short_id(&c.config.id),
            col(&c.config.name, NAME_W),
            c.config.command
        ));
        prev = Some(c);
    }
    out
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

/// The text form of `config show`, relative to `now`.
fn render_show(c: &HostedRunConfig, now: u64) -> String {
    let field = |label: &str, value: &str| format!("{label:<8} {value}\n");
    let mut out = String::new();
    out.push_str(&field("id", &c.config.id));
    out.push_str(&field("name", &c.config.name));
    out.push_str(&field("command", &c.config.command));
    out.push_str(&field("host", &c.hostname));
    out.push_str(&field("cwd", &c.cwd.to_string_lossy()));
    out.push_str(&field("updated", &relative_time(now, c.config.updated_at)));
    out
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
    println!(
        "{verb} run config {} (event {}…)",
        describe(c),
        &event_id[..8]
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosted(host: &str, cwd: &str, id: &str, name: &str, command: &str) -> HostedRunConfig {
        HostedRunConfig {
            hostname: host.into(),
            cwd: cwd.into(),
            config: RunConfig {
                id: id.into(),
                name: name.into(),
                command: command.into(),
                updated_at: 1_000,
            },
        }
    }

    fn sample() -> Vec<HostedRunConfig> {
        vec![
            hosted("linux", "/p", "aaaa1111-x", "build", "make"),
            hosted("mac", "/p", "aaaa2222-x", "build", "cargo build"),
            hosted("mac", "/p", "bbbb3333-x", "run", "cargo run"),
            hosted("mac", "/q", "cccc4444-x", "serve", "npm start"),
        ]
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn resolve_by_exact_id_prefix_or_name() {
        let c = sample();
        assert_eq!(resolve_config(&c, "aaaa2222-x").unwrap().hostname, "mac");
        assert_eq!(resolve_config(&c, "BBBB").unwrap().config.name, "run");
        assert_eq!(resolve_config(&c, "serve").unwrap().config.id, "cccc4444-x");
    }

    #[test]
    fn resolve_ambiguous_lists_candidates_and_unknown_errors() {
        let c = sample();
        let err = resolve_config(&c, "build").unwrap_err().to_string();
        assert!(err.contains("more than one"), "{err}");
        assert!(err.contains("aaaa1111 build (linux:/p)"), "{err}");
        assert!(err.contains("aaaa2222 build (mac:/p)"), "{err}");

        let err = resolve_config(&c, "aaaa").unwrap_err().to_string();
        assert!(err.contains("more than one"), "{err}");

        let err = resolve_config(&c, "zzz").unwrap_err().to_string();
        assert!(err.contains("no run config matches 'zzz'"), "{err}");
    }

    #[test]
    fn host_filter_disambiguates() {
        let filters = ConfigFilters {
            host: Some("MAC".into()),
            cwd: None,
        };
        let c: Vec<_> = sample()
            .into_iter()
            .filter(|c| filters.matches(c))
            .collect();
        assert_eq!(resolve_config(&c, "build").unwrap().config.id, "aaaa2222-x");
    }

    #[test]
    fn list_groups_by_host_then_cwd() {
        let out = render_list(&sample(), false);
        assert_eq!(
            out,
            format!(
                "linux\n  /p\n    aaaa1111  {}  make\n\nmac\n  /p\n    aaaa2222  {}  cargo build\n    \
                 bbbb3333  {}  cargo run\n  /q\n    cccc4444  {}  npm start\n",
                col("build", NAME_W),
                col("build", NAME_W),
                col("run", NAME_W),
                col("serve", NAME_W),
            )
        );
    }

    #[test]
    fn show_renders_every_field() {
        let out = render_show(&sample()[1], 1_000 + 7200);
        assert!(out.contains("id       aaaa2222-x\n"), "{out}");
        assert!(out.contains("command  cargo build\n"), "{out}");
        assert!(out.contains("host     mac\n"), "{out}");
        assert!(out.contains("cwd      /p\n"), "{out}");
        assert!(out.contains("updated  2h ago\n"), "{out}");
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

    #[test]
    fn replacing_never_ties_the_previous_revision() {
        let far_future = now_secs() + 1_000;
        assert_eq!(replacing(far_future), far_future + 1);
        assert!(replacing(0) >= now_secs().saturating_sub(1));
    }
}
