//! What `nd agentium config` and `nd agentium config …` share: which run config
//! a selector means, and the text each subcommand prints.
//!
//! A run config is what Dave's per-session run bar launches: a named shell
//! command (`cargo run`, say) registered for one host and working directory,
//! and run there with `sh -c`. Each is its own kind-31991 event (d-tag = the
//! config's UUID), so an edit is a newer revision of the same d-tag and a
//! delete is a newer revision carrying a `deleted` tag.
//!
//! The desktop only ever shows the configs of its own host, for the exact cwd
//! of the session in view. The command line sees every host: `list` is the
//! whole set, grouped host → cwd, and a selector resolves across all of it.

use serde::Serialize;

use super::term::{col, now_secs, paint, relative_time, SGR_BOLD};
use crate::session_loader::HostedRunConfig;

/// Width of the name column in `config list`.
const NAME_W: usize = 16;

/// A trimmed, non-empty `--name`/`--command`, mirroring the desktop editor,
/// which won't save a config with either blank.
pub fn required_field(flag: &str, value: Option<&str>) -> Result<String, String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{flag} needs a non-empty value"))
}

/// Case-insensitive substring filters over host and cwd — the `--host`/`--cwd`
/// flags, which read the same way for `list`. `add` reads them instead as the
/// exact place to register the config.
pub struct ConfigFilters {
    pub host: Option<String>,
    pub cwd: Option<String>,
}

impl ConfigFilters {
    /// Whether `c` passes both filters.
    pub fn matches(&self, c: &HostedRunConfig) -> bool {
        let contains = |hay: &str, needle: &Option<String>| {
            needle
                .as_ref()
                .is_none_or(|n| hay.to_lowercase().contains(&n.to_lowercase()))
        };
        contains(&c.hostname, &self.host) && contains(&c.cwd.to_string_lossy(), &self.cwd)
    }
}

/// The `created_at` for a revision replacing one stamped `prev`: now, or one
/// second past `prev` if the clock hasn't moved beyond it. A same-second
/// revision would tie, and a tie goes to whichever the fold sees first.
pub fn replacing(prev: u64) -> u64 {
    now_secs().max(prev + 1)
}

/// `add`'s host or cwd: the flag, else the current session's, else an error
/// naming the flag.
pub fn pick_place(
    flag: Option<&str>,
    current: Option<String>,
    name: &str,
) -> Result<String, String> {
    flag.map(str::to_string)
        .or(current)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            format!(
                "no {} — pass {name} (or run inside a session so $AGENTIUM_SESSION supplies it)",
                &name[2..]
            )
        })
}

/// Resolve `selector` to one config: an exact id, else the single config whose
/// id starts with it (case-insensitive) or whose name is exactly it. None, or
/// more than one, is an error listing what it could have meant.
pub fn resolve_config<'c>(
    configs: &'c [HostedRunConfig],
    selector: &str,
) -> Result<&'c HostedRunConfig, String> {
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
            "no run config matches '{selector}' (by id prefix or exact name) — see `nd agentium config list`"
        ));
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
    ))
}

/// One line naming a config: short id, name, and where it lives.
pub fn describe(c: &HostedRunConfig) -> String {
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
#[derive(Serialize)]
pub struct ConfigJson<'a> {
    id: &'a str,
    name: &'a str,
    command: &'a str,
    host: &'a str,
    cwd: std::borrow::Cow<'a, str>,
    updated_at: u64,
}

impl<'a> ConfigJson<'a> {
    pub fn new(c: &'a HostedRunConfig) -> Self {
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

/// The text form of `config list`: grouped host → cwd → one row per config,
/// or `no run configs`. `configs` arrives sorted by host then cwd, so a header
/// is due whenever either changes from the row before.
pub fn render_list(configs: &[HostedRunConfig], color: bool) -> String {
    if configs.is_empty() {
        return "no run configs\n".to_string();
    }
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

/// The text form of `config show`, relative to `now`.
pub fn render_show(c: &HostedRunConfig, now: u64) -> String {
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

/// The report of a published `add`/`edit`/`rm`: `<verb> run config <describe>
/// (event <short>…)`.
pub fn render_published(verb: &str, c: &HostedRunConfig, event_id: &str) -> String {
    format!(
        "{verb} run config {} (event {}…)\n",
        describe(c),
        &event_id[..event_id.len().min(8)]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RunConfig;

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
        let err = resolve_config(&c, "build").unwrap_err();
        assert!(err.contains("more than one"), "{err}");
        assert!(err.contains("aaaa1111 build (linux:/p)"), "{err}");
        assert!(err.contains("aaaa2222 build (mac:/p)"), "{err}");
        assert!(resolve_config(&c, "aaaa")
            .unwrap_err()
            .contains("more than one"));
        assert!(resolve_config(&c, "zzz")
            .unwrap_err()
            .contains("no run config matches 'zzz'"));
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
        assert_eq!(render_list(&[], false), "no run configs\n");
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
    fn fields_are_trimmed_and_required() {
        assert_eq!(required_field("--name", Some("  build ")).unwrap(), "build");
        assert!(required_field("--name", Some(" ")).is_err());
        assert!(required_field("--name", None).is_err());
    }

    #[test]
    fn replacing_never_ties_the_previous_revision() {
        let far_future = now_secs() + 1_000;
        assert_eq!(replacing(far_future), far_future + 1);
        assert!(replacing(0) >= now_secs().saturating_sub(1));
    }
}
