//! `agentium profile` — list, show, set and remove backend profiles, and set or
//! remove their secrets.
//!
//! A backend profile is a named env overlay on the Claude backend (the endpoint,
//! credentials and models a session runs against), stored as one account-wide
//! kind-31992 event per profile (d-tag = slug), only ever published PNS-wrapped.
//! See [`BackendProfile`].
//!
//! Every write is a whole-profile replace: read the newest revision, change it,
//! publish the result as the next revision. A secret value never goes in argv,
//! a log line or output: `secret set` reads it from stdin, and everything that
//! prints goes through [`BackendProfile::redacted_env`].

use std::io::{IsTerminal, Read};

use agentium_core::Engine;
use agentium_core::config::{
    BACKEND_PROFILE_BACKEND, BackendProfile, RedactedValue, is_secret_var, validate_env_name,
    validate_slug,
};
use agentium_core::session_events::{
    BuiltEvent, build_backend_profile_delete_event_at, build_backend_profile_event_at,
};
use agentium_core::session_loader::load_backend_profiles_from_ndb;
use nostrdb::Transaction;
use nostrdb_net::Pubkey;
use nostrdb_net::relay::sync::Result;

use crate::publish::{flush_publish, json_line};
use crate::term::{SGR_BOLD, now_secs, paint, relative_time};
use agentium_core::cli::config::replacing;

/// The `profile` subcommand, as parsed.
pub(crate) enum ProfileAction {
    /// Every profile, slug order.
    List,
    /// One profile in full, secrets masked.
    Show { slug: String },
    /// Create a profile, or change an existing one.
    Set { slug: String, edit: ProfileEdit },
    /// Store one secret var, its value read from stdin by
    /// [`ProfileAction::read_secret`] before any relay work.
    SecretSet {
        slug: String,
        name: String,
        value: Option<SecretValue>,
    },
    /// Drop one secret var.
    SecretRm { slug: String, name: String },
    /// Delete a profile (a tombstone revision).
    Rm { slug: String },
}

/// What `profile set` changes. Each field left `None`/empty keeps the
/// profile's current value.
#[derive(Default)]
pub(crate) struct ProfileEdit {
    pub(crate) label: Option<String>,
    /// Replaces the whole model list when non-empty (`--model`, repeatable).
    pub(crate) models: Vec<String>,
    /// `Some("")` clears it.
    pub(crate) default_model: Option<String>,
    /// `Some("")` clears it, sharing `~/.claude` again.
    pub(crate) config_dir: Option<String>,
    /// `--env NAME=VALUE`, in order; a later one for the same name wins.
    pub(crate) env: Vec<EnvAssign>,
    /// `--unset NAME`: drop the var from the profile.
    pub(crate) unset: Vec<String>,
}

impl ProfileEdit {
    /// Whether the edit would change nothing at all.
    fn is_empty(&self) -> bool {
        self.label.is_none()
            && self.models.is_empty()
            && self.default_model.is_none()
            && self.config_dir.is_none()
            && self.env.is_empty()
            && self.unset.is_empty()
    }
}

/// One `--env NAME=VALUE`.
pub(crate) struct EnvAssign {
    pub(crate) name: String,
    pub(crate) value: String,
}

impl EnvAssign {
    /// Parse `NAME=VALUE`. A secret-looking name ([`is_secret_var`]) with a
    /// value is refused, since argv lands in shell history and `ps`; `NAME=`
    /// (export empty, clearing an inherited var) is fine for any name.
    pub(crate) fn parse(arg: &str) -> Result<Self> {
        // The error never echoes `arg`: it may be a token typed in by mistake.
        let Some((name, value)) = arg.split_once('=') else {
            return Err("--env needs the form NAME=VALUE".into());
        };
        validate_env_name(name)?;
        if is_secret_var(name) && !value.is_empty() {
            return Err(format!(
                "{name} looks secret, so it can't go on the command line — use \
                 `agentium profile secret set <slug> {name}`, which reads it from stdin"
            )
            .into());
        }
        Ok(EnvAssign {
            name: name.to_string(),
            value: value.to_string(),
        })
    }
}

/// A secret read from stdin. `Debug` never shows it.
pub(crate) struct SecretValue(String);

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValue(********)")
    }
}

impl ProfileAction {
    /// Parse `profile <sub> [args]`. A bare `profile` lists. `edit` holds the
    /// `set` flags, already pulled out of the flag loop; it is rejected for
    /// every other subcommand.
    pub(crate) fn parse(rest: &[String], edit: ProfileEdit) -> Result<Self> {
        let sub = rest.first().map(String::as_str).unwrap_or("list");
        let positional = |i: usize, what: &str, usage: &str| -> Result<String> {
            rest.get(i)
                .cloned()
                .ok_or_else(|| format!("`profile {usage}` needs {what}").into())
        };
        let slug = |usage: &str| -> Result<String> {
            let slug = positional(1, "a profile slug", usage)?;
            validate_slug(&slug)?;
            Ok(slug)
        };
        if sub != "set" && !edit.is_empty() {
            return Err(
                "--label/--model/--default-model/--config-dir/--env/--unset only apply to \
                 `profile set`"
                    .into(),
            );
        }

        Ok(match sub {
            "list" | "ls" => ProfileAction::List,
            "show" => ProfileAction::Show {
                slug: slug("show")?,
            },
            "set" => ProfileAction::Set {
                slug: slug("set")?,
                edit,
            },
            "rm" | "remove" | "delete" => ProfileAction::Rm { slug: slug("rm")? },
            "secret" => {
                let op = rest.get(1).map(String::as_str).unwrap_or("");
                let usage = format!("secret {op} <slug> <NAME>");
                let slug = rest
                    .get(2)
                    .cloned()
                    .ok_or_else(|| format!("`profile {usage}` needs a profile slug"))?;
                validate_slug(&slug)?;
                let name = rest
                    .get(3)
                    .cloned()
                    .ok_or_else(|| format!("`profile {usage}` needs a variable NAME"))?;
                validate_env_name(&name)?;
                if rest.len() > 4 {
                    return Err(format!(
                        "`profile {usage}` takes no value argument — the value is read from \
                         stdin, which keeps it out of shell history"
                    )
                    .into());
                }
                match op {
                    "set" => {
                        if !is_secret_var(&name) {
                            return Err(format!(
                                "{name} doesn't look secret (its name has no TOKEN, KEY, SECRET \
                                 or PASSWORD), so `profile show` would print it — set it with \
                                 `profile set {slug} --env {name}=<value>`"
                            )
                            .into());
                        }
                        ProfileAction::SecretSet {
                            slug,
                            name,
                            value: None,
                        }
                    }
                    "rm" | "remove" | "delete" => ProfileAction::SecretRm { slug, name },
                    other => {
                        return Err(format!(
                            "unknown profile secret subcommand '{other}' (set | rm)"
                        )
                        .into());
                    }
                }
            }
            other => {
                return Err(format!(
                    "unknown profile subcommand '{other}' (list | show | set | secret | rm)"
                )
                .into());
            }
        })
    }

    /// Whether the action publishes, and so needs the relay (see
    /// `Command::needs_relay`).
    pub(crate) fn publishes(&self) -> bool {
        !matches!(self, ProfileAction::List | ProfileAction::Show { .. })
    }

    /// For `secret set`, read the value from stdin now, before the engine opens
    /// and syncs: a typo'd pipe fails fast, and a terminal prompt doesn't wait
    /// behind a relay reconcile. Every other action is untouched.
    pub(crate) fn read_secret(&mut self) -> Result<()> {
        if let ProfileAction::SecretSet { name, value, .. } = self {
            *value = Some(read_secret_value(name)?);
        }
        Ok(())
    }
}

/// Read a secret value from stdin: one line, not echoed, from a terminal; all
/// of it from a pipe. The trailing newline is trimmed. Empty or multi-line
/// values are refused: a token is one line, and a multi-line one is usually a
/// whole `pass` entry piped in by mistake.
fn read_secret_value(name: &str) -> Result<SecretValue> {
    let mut stdin = std::io::stdin();
    let mut raw = String::new();
    if stdin.is_terminal() {
        eprint!("{name} (input hidden): ");
        let _echo = EchoOff::new();
        stdin.read_line(&mut raw)?;
        eprintln!();
    } else {
        stdin.read_to_string(&mut raw)?;
    }
    parse_secret_value(name, raw)
}

/// [`read_secret_value`]'s checks on what was read, apart from stdin.
fn parse_secret_value(name: &str, raw: String) -> Result<SecretValue> {
    let value = raw.trim_end_matches(['\n', '\r']);
    if value.is_empty() {
        return Err(format!("no value for {name} on stdin; nothing stored").into());
    }
    let lines = value.lines().count();
    if lines > 1 {
        return Err(format!(
            "the value for {name} spans {lines} lines; pipe just the secret (e.g. `pass show \
             <entry> | head -1`)"
        )
        .into());
    }
    if value.contains('\0') {
        return Err(format!("the value for {name} contains a NUL byte").into());
    }
    Ok(SecretValue(value.to_string()))
}

/// Terminal echo turned off for as long as it lives, so a typed secret isn't
/// drawn on screen. A no-op when stdin isn't a terminal, or off unix.
struct EchoOff {
    #[cfg(unix)]
    saved: Option<libc::termios>,
}

impl EchoOff {
    #[cfg(unix)]
    fn new() -> Self {
        // SAFETY: tcgetattr/tcsetattr only read and write the termios struct
        // we hand them, for fd 0; a failure leaves `saved` None and echo as is.
        unsafe {
            let mut term: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut term) != 0 {
                return EchoOff { saved: None };
            }
            let saved = term;
            term.c_lflag &= !libc::ECHO;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &term) != 0 {
                return EchoOff { saved: None };
            }
            EchoOff { saved: Some(saved) }
        }
    }

    #[cfg(not(unix))]
    fn new() -> Self {
        EchoOff {}
    }
}

impl Drop for EchoOff {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(saved) = self.saved {
            // SAFETY: restores the termios read in `new`, for fd 0.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved);
            }
        }
    }
}

/// `agentium profile …` — dispatch one [`ProfileAction`].
///
/// `secret` signs what the writes publish; it is the key the engine was
/// opened with, so the PNS envelope is this account's own.
pub(crate) async fn cmd_profile(
    engine: &Engine,
    author: &Pubkey,
    secret: &[u8; 32],
    action: ProfileAction,
    as_json: bool,
) -> Result<()> {
    let mut profiles = {
        let txn = Transaction::new(engine.ndb())?;
        load_backend_profiles_from_ndb(engine.ndb(), &txn, author)
    };

    match action {
        ProfileAction::List => list(&profiles, as_json),
        ProfileAction::Show { slug } => show(existing(&profiles, &slug)?, as_json),
        ProfileAction::Set { slug, edit } => {
            let (verb, profile) = match profiles.remove(&slug) {
                Some(mut profile) => {
                    if edit.is_empty() {
                        return Err(format!(
                            "nothing to change on profile '{slug}' — pass --label, --model, \
                             --default-model, --config-dir, --env or --unset"
                        )
                        .into());
                    }
                    apply_edit(&mut profile, edit)?;
                    ("updated", profile)
                }
                None => {
                    let mut profile = BackendProfile::new(slug.clone(), slug);
                    apply_edit(&mut profile, edit)?;
                    ("created", profile)
                }
            };
            publish_profile(engine, secret, verb, &profile, as_json).await
        }
        ProfileAction::SecretSet { slug, name, value } => {
            let value = value.ok_or("secret value was never read from stdin")?;
            let mut profile = existing_owned(&mut profiles, &slug)?;
            profile.env.insert(name.clone(), value.0);
            report_secret(
                &publish(engine, secret, &profile).await?,
                "stored",
                &name,
                &slug,
                as_json,
            )
        }
        ProfileAction::SecretRm { slug, name } => {
            let mut profile = existing_owned(&mut profiles, &slug)?;
            if profile.env.remove(&name).is_none() {
                return Err(format!("profile '{slug}' has no {name}").into());
            }
            report_secret(
                &publish(engine, secret, &profile).await?,
                "removed",
                &name,
                &slug,
                as_json,
            )
        }
        ProfileAction::Rm { slug } => {
            let profile = existing(&profiles, &slug)?;
            let built = build_backend_profile_delete_event_at(
                &slug,
                Some(replacing(profile.updated_at)),
                secret,
            )?;
            let event_id = send(engine, &built).await?;
            if as_json {
                let obj = serde_json::json!({
                    "action": "removed",
                    "event_id": event_id,
                    "slug": slug,
                });
                println!("{}", json_line(&obj)?);
            } else {
                println!("removed profile {slug} (event {}…)", &event_id[..8]);
            }
            Ok(())
        }
    }
}

/// The profile named `slug`, or an error pointing at `profile list`.
fn existing<'p>(
    profiles: &'p std::collections::BTreeMap<String, BackendProfile>,
    slug: &str,
) -> Result<&'p BackendProfile> {
    profiles.get(slug).ok_or_else(|| no_such_profile(slug))
}

/// [`existing`], taken out of the map to be changed and republished.
fn existing_owned(
    profiles: &mut std::collections::BTreeMap<String, BackendProfile>,
    slug: &str,
) -> Result<BackendProfile> {
    profiles.remove(slug).ok_or_else(|| no_such_profile(slug))
}

/// The error for a slug with no profile, naming the commands that help.
fn no_such_profile(slug: &str) -> Box<dyn std::error::Error> {
    format!(
        "no profile '{slug}' — see `agentium profile list`, or create it with \
         `agentium profile set {slug}`"
    )
    .into()
}

/// Apply `edit` to `profile`, checking the default model is one it offers.
fn apply_edit(profile: &mut BackendProfile, edit: ProfileEdit) -> Result<()> {
    if let Some(label) = edit.label {
        let label = label.trim();
        if label.is_empty() {
            return Err("--label needs a non-empty value".into());
        }
        profile.label = label.to_string();
    }
    if !edit.models.is_empty() {
        profile.models = edit.models;
    }
    if let Some(default) = edit.default_model {
        profile.default_model = (!default.is_empty()).then_some(default);
    }
    if let Some(dir) = edit.config_dir {
        profile.config_dir = (!dir.is_empty()).then_some(dir);
    }
    for EnvAssign { name, value } in edit.env {
        profile.env.insert(name, value);
    }
    for name in edit.unset {
        if profile.env.remove(&name).is_none() {
            return Err(format!("--unset {name}: profile '{}' has no {name}", profile.slug).into());
        }
    }
    if let Some(default) = &profile.default_model
        && !profile.models.is_empty()
        && !profile.models.contains(default)
    {
        return Err(format!(
            "the default model {default} isn't one of the profile's models ({}) — pass \
             --default-model",
            profile.models.join(", ")
        )
        .into());
    }
    Ok(())
}

/// Build the next revision of `profile`, publish it, and return its event id.
async fn publish(engine: &Engine, secret: &[u8; 32], profile: &BackendProfile) -> Result<String> {
    let built =
        build_backend_profile_event_at(profile, Some(replacing(profile.updated_at)), secret)?;
    send(engine, &built).await
}

/// Publish a built event, wait for it to leave, and return its id in hex.
async fn send(engine: &Engine, built: &BuiltEvent) -> Result<String> {
    engine.publish_event(built)?;
    flush_publish(engine).await;
    Ok(hex::encode(built.note_id))
}

/// `set`: publish and report the profile as it now stands, secrets masked.
async fn publish_profile(
    engine: &Engine,
    secret: &[u8; 32],
    verb: &str,
    profile: &BackendProfile,
    as_json: bool,
) -> Result<()> {
    let event_id = publish(engine, secret, profile).await?;
    if as_json {
        let obj = serde_json::json!({
            "action": verb,
            "event_id": event_id,
            "profile": ProfileJson::new(profile),
        });
        println!("{}", json_line(&obj)?);
        return Ok(());
    }
    println!(
        "{verb} profile {} (event {}…)",
        profile.slug,
        &event_id[..8]
    );
    Ok(())
}

/// `secret set`/`secret rm`: report by name only — never the value.
fn report_secret(event_id: &str, verb: &str, name: &str, slug: &str, as_json: bool) -> Result<()> {
    if as_json {
        let obj = serde_json::json!({
            "action": format!("secret {verb}"),
            "event_id": event_id,
            "slug": slug,
            "name": name,
        });
        println!("{}", json_line(&obj)?);
        return Ok(());
    }
    println!(
        "{verb} {name} on profile {slug} (event {}…)",
        &event_id[..8]
    );
    Ok(())
}

/// The `--json` view of a profile: every field, with secret values masked
/// exactly as the text view masks them.
#[derive(serde::Serialize)]
struct ProfileJson<'a> {
    slug: &'a str,
    label: &'a str,
    backend: &'static str,
    models: &'a [String],
    default_model: Option<&'a str>,
    config_dir: Option<&'a str>,
    env: Vec<EnvJson<'a>>,
    updated_at: u64,
}

/// One env var in [`ProfileJson`]. `value` is `null` for a masked secret and
/// `""` for a var the profile clears from the inherited env.
#[derive(serde::Serialize)]
struct EnvJson<'a> {
    name: &'a str,
    value: Option<&'a str>,
    secret: bool,
}

impl<'a> ProfileJson<'a> {
    fn new(p: &'a BackendProfile) -> Self {
        ProfileJson {
            slug: &p.slug,
            label: &p.label,
            backend: BACKEND_PROFILE_BACKEND,
            models: &p.models,
            default_model: p.default_model.as_deref(),
            config_dir: p.config_dir.as_deref(),
            env: p
                .redacted_env()
                .map(|var| EnvJson {
                    name: var.name,
                    value: match var.value {
                        RedactedValue::Plain(value) => Some(value),
                        RedactedValue::Unset => Some(""),
                        RedactedValue::Masked => None,
                    },
                    secret: is_secret_var(var.name),
                })
                .collect(),
            updated_at: p.updated_at,
        }
    }
}

/// `profile list`: one block per profile, or with `--json` the array.
fn list(
    profiles: &std::collections::BTreeMap<String, BackendProfile>,
    as_json: bool,
) -> Result<()> {
    if as_json {
        let rows: Vec<ProfileJson> = profiles.values().map(ProfileJson::new).collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if profiles.is_empty() {
        println!("no backend profiles — create one with `agentium profile set <slug>`");
        return Ok(());
    }
    print!(
        "{}",
        render_list(profiles.values(), std::io::stdout().is_terminal())
    );
    Ok(())
}

/// The text form of `profile list`: a bold `slug  label  (backend)` line, then
/// the models (the default starred) and the env var names, never values.
fn render_list<'p>(profiles: impl Iterator<Item = &'p BackendProfile>, color: bool) -> String {
    let mut out = String::new();
    for (i, p) in profiles.enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&paint(color, SGR_BOLD, &p.slug));
        out.push_str(&format!("  {}  ({BACKEND_PROFILE_BACKEND})\n", p.label));
        out.push_str(&format!("  models  {}\n", models_line(p, "*")));
        let names = p.env.keys().map(String::as_str).collect::<Vec<_>>();
        let names = if names.is_empty() {
            "(none)".to_string()
        } else {
            names.join(" ")
        };
        out.push_str(&format!("  env     {names}\n"));
    }
    out
}

/// A profile's models in order, the default marked with `mark`; `(backend
/// default)` when it lists none.
fn models_line(p: &BackendProfile, mark: &str) -> String {
    if p.models.is_empty() {
        return match &p.default_model {
            Some(default) => format!("{default}{mark}"),
            None => "(backend default)".to_string(),
        };
    }
    p.models
        .iter()
        .map(|m| {
            if p.default_model.as_ref() == Some(m) {
                format!("{m}{mark}")
            } else {
                m.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `profile show`: every field of one profile, secrets masked.
fn show(p: &BackendProfile, as_json: bool) -> Result<()> {
    if as_json {
        println!("{}", serde_json::to_string_pretty(&ProfileJson::new(p))?);
        return Ok(());
    }
    print!("{}", render_show(p, now_secs()));
    Ok(())
}

/// The text form of `profile show`, relative to `now`.
fn render_show(p: &BackendProfile, now: u64) -> String {
    let field = |label: &str, value: &str| format!("{label:<11} {value}\n");
    let mut out = String::new();
    out.push_str(&field("slug", &p.slug));
    out.push_str(&field("label", &p.label));
    out.push_str(&field("backend", BACKEND_PROFILE_BACKEND));
    out.push_str(&field("models", &models_line(p, " (default)")));
    let config_dir = p.config_dir.as_deref().unwrap_or("(shares ~/.claude)");
    out.push_str(&field("config dir", config_dir));
    out.push_str(&field("updated", &relative_time(now, p.updated_at)));
    if p.env.is_empty() {
        out.push_str(&field("env", "(none)"));
        return out;
    }
    out.push_str("env\n");
    let width = p.env.keys().map(String::len).max().unwrap_or(0);
    for var in p.redacted_env() {
        out.push_str(&format!("  {:<width$}  {}\n", var.name, var.value));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "sk-very-secret-0123456789";

    fn hydra() -> BackendProfile {
        let mut p = BackendProfile::new("hydra", "Hydra");
        p.env
            .insert("ANTHROPIC_BASE_URL".into(), "https://llm.hydra.host".into());
        p.env.insert("ANTHROPIC_AUTH_TOKEN".into(), TOKEN.into());
        p.env.insert("ANTHROPIC_API_KEY".into(), String::new());
        p.models = vec!["glm-5.3-flash".into(), "qwen3.8-flash-next".into()];
        p.default_model = Some("glm-5.3-flash".into());
        p.updated_at = 1_000;
        p
    }

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn show_masks_secrets_and_marks_unset() {
        let out = render_show(&hydra(), 1_000 + 120);
        assert!(!out.contains(TOKEN), "{out}");
        assert!(
            out.contains("ANTHROPIC_AUTH_TOKEN  ********\n"),
            "masked:\n{out}"
        );
        assert!(
            out.contains("ANTHROPIC_BASE_URL    https://llm.hydra.host\n"),
            "{out}"
        );
        assert!(out.contains("ANTHROPIC_API_KEY     (unset)\n"), "{out}");
        assert!(
            out.contains("models      glm-5.3-flash (default), qwen3.8-flash-next\n"),
            "{out}"
        );
        assert!(out.contains("config dir  (shares ~/.claude)\n"), "{out}");
        assert!(out.contains("updated     2m ago\n"), "{out}");
    }

    #[test]
    fn list_names_vars_but_never_values() {
        let p = hydra();
        let out = render_list(std::iter::once(&p), false);
        assert_eq!(
            out,
            "hydra  Hydra  (claude)\n  models  glm-5.3-flash*, qwen3.8-flash-next\n  env     \
             ANTHROPIC_API_KEY ANTHROPIC_AUTH_TOKEN ANTHROPIC_BASE_URL\n"
        );
    }

    #[test]
    fn json_masks_secrets() {
        let p = hydra();
        let json = serde_json::to_string(&ProfileJson::new(&p)).unwrap();
        assert!(!json.contains(TOKEN), "{json}");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let env = v["env"].as_array().unwrap();
        let token = env
            .iter()
            .find(|e| e["name"] == "ANTHROPIC_AUTH_TOKEN")
            .unwrap();
        assert_eq!(token["value"], serde_json::Value::Null);
        assert_eq!(token["secret"], true);
        let key = env
            .iter()
            .find(|e| e["name"] == "ANTHROPIC_API_KEY")
            .unwrap();
        assert_eq!(key["value"], "", "a cleared var reads as empty, not masked");
    }

    #[test]
    fn env_flag_refuses_secret_values_but_allows_clearing() {
        let err = EnvAssign::parse("ANTHROPIC_AUTH_TOKEN=x")
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("agentium profile secret set"), "{err}");
        let cleared = EnvAssign::parse("ANTHROPIC_API_KEY=").unwrap();
        assert_eq!(cleared.value, "");
        let url = EnvAssign::parse("ANTHROPIC_BASE_URL=https://a=b").unwrap();
        assert_eq!(url.value, "https://a=b", "splits on the first '=' only");
        assert!(EnvAssign::parse("NOEQUALS").is_err());
        assert!(EnvAssign::parse("1BAD=x").is_err());
    }

    #[test]
    fn parse_subcommands() {
        assert!(matches!(
            ProfileAction::parse(&[], ProfileEdit::default()).unwrap(),
            ProfileAction::List
        ));
        assert!(matches!(
            ProfileAction::parse(
                &strings(&["secret", "set", "hydra", "ANTHROPIC_AUTH_TOKEN"]),
                ProfileEdit::default()
            )
            .unwrap(),
            ProfileAction::SecretSet { value: None, .. }
        ));
        let with_value = ProfileAction::parse(
            &strings(&["secret", "set", "hydra", "ANTHROPIC_AUTH_TOKEN", "x"]),
            ProfileEdit::default(),
        );
        assert!(
            with_value.err().unwrap().to_string().contains("stdin"),
            "a value in argv is refused"
        );
        let plain = ProfileAction::parse(
            &strings(&["secret", "set", "hydra", "ANTHROPIC_BASE_URL"]),
            ProfileEdit::default(),
        );
        assert!(plain.is_err(), "a non-secret name goes through --env");
        let stray = ProfileAction::parse(
            &strings(&["rm", "hydra"]),
            ProfileEdit {
                label: Some("x".into()),
                ..Default::default()
            },
        );
        assert!(stray.is_err(), "--label means nothing to rm");
        assert!(
            ProfileAction::parse(&strings(&["show", "Hydra"]), ProfileEdit::default()).is_err()
        );
        assert!(ProfileAction::parse(&strings(&["show"]), ProfileEdit::default()).is_err());
        assert!(ProfileAction::parse(&strings(&["frob"]), ProfileEdit::default()).is_err());
    }

    #[test]
    fn edit_replaces_models_and_checks_the_default() {
        let mut p = hydra();
        apply_edit(
            &mut p,
            ProfileEdit {
                models: vec!["a".into(), "b".into()],
                default_model: Some("b".into()),
                env: vec![EnvAssign::parse("ANTHROPIC_MODEL=a").unwrap()],
                unset: vec!["ANTHROPIC_API_KEY".into()],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(p.models, ["a", "b"]);
        assert_eq!(p.default_model.as_deref(), Some("b"));
        assert_eq!(p.env.get("ANTHROPIC_MODEL").map(String::as_str), Some("a"));
        assert!(!p.env.contains_key("ANTHROPIC_API_KEY"));
        assert_eq!(
            p.env.get("ANTHROPIC_AUTH_TOKEN").map(String::as_str),
            Some(TOKEN)
        );

        let mut p = hydra();
        let err = apply_edit(
            &mut p,
            ProfileEdit {
                default_model: Some("opus".into()),
                ..Default::default()
            },
        );
        assert!(err.is_err(), "a default outside the model list is refused");

        let mut p = hydra();
        let err = apply_edit(
            &mut p,
            ProfileEdit {
                unset: vec!["NOPE".into()],
                ..Default::default()
            },
        );
        assert!(err.is_err(), "unsetting a var the profile lacks is refused");
    }

    #[test]
    fn secret_value_trims_newline_and_refuses_junk() {
        assert_eq!(
            parse_secret_value("T", format!("{TOKEN}\r\n")).unwrap().0,
            TOKEN
        );
        assert!(parse_secret_value("T", "\n".into()).is_err());
        let err = parse_secret_value("T", "pw\nlogin: me\n".into())
            .unwrap_err()
            .to_string();
        assert!(err.contains("2 lines") && !err.contains("pw"), "{err}");
        assert_eq!(
            format!("{:?}", SecretValue(TOKEN.into())),
            "SecretValue(********)"
        );
    }
}
