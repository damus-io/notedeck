//! What `nd agentium profile …` needs to edit backend profiles: how an edit
//! applies, how a secret value is checked, and the text and JSON each
//! subcommand prints.
//!
//! A backend profile is a named env overlay on a backend (the endpoint,
//! credentials and models a session runs against): the Claude harness, or an
//! OpenAI-compatible provider for the native loop (`--backend openai`). It is
//! stored as one account-wide kind-31992 event per profile (d-tag = slug),
//! only ever published PNS-wrapped. See [`BackendProfile`].
//!
//! Every write is a whole-profile replace: read the newest revision, change
//! it, publish the result as the next revision. A secret value is never
//! printed: everything that shows a profile goes through
//! [`BackendProfile::redacted_env`], and no error here echoes a value.

use std::collections::BTreeMap;

use serde::Serialize;

use super::term::{paint, relative_time, SGR_BOLD};
use crate::config::{
    is_secret_var, validate_env_name, BackendProfile, ProfileBackend, RedactedValue,
};

/// What `profile set` changes. Each field left `None`/empty keeps the
/// profile's current value.
#[derive(Default)]
pub struct ProfileEdit {
    pub label: Option<String>,
    /// `--backend`: which loop the profile configures. A new profile is a
    /// Claude one unless this says otherwise.
    pub backend: Option<ProfileBackend>,
    /// Replaces the whole model list when non-empty (`--model`, repeatable).
    pub models: Vec<String>,
    /// `Some("")` clears it.
    pub default_model: Option<String>,
    /// `Some("")` clears it, sharing `~/.claude` again.
    pub config_dir: Option<String>,
    /// `--env NAME=VALUE`, in order; a later one for the same name wins.
    pub env: Vec<EnvAssign>,
    /// `--unset NAME`: drop the var from the profile.
    pub unset: Vec<String>,
}

impl ProfileEdit {
    /// Whether the edit would change nothing at all.
    pub fn is_empty(&self) -> bool {
        self.label.is_none()
            && self.backend.is_none()
            && self.models.is_empty()
            && self.default_model.is_none()
            && self.config_dir.is_none()
            && self.env.is_empty()
            && self.unset.is_empty()
    }
}

/// One `--env NAME=VALUE`.
pub struct EnvAssign {
    pub name: String,
    pub value: String,
}

impl EnvAssign {
    /// Parse `NAME=VALUE`, splitting on the first `=`. A secret-looking name
    /// ([`is_secret_var`]) with a value is refused, since a command line lands
    /// in shell history and `ps`; `NAME=` (export empty, clearing an
    /// inherited var) is fine for any name.
    pub fn parse(arg: &str) -> Result<Self, String> {
        // The error never echoes `arg`: it may be a token typed in by mistake.
        let Some((name, value)) = arg.split_once('=') else {
            return Err("--env needs the form NAME=VALUE".into());
        };
        validate_env_name(name).map_err(|e| e.to_string())?;
        if is_secret_var(name) && !value.is_empty() {
            return Err(format!(
                "{name} looks secret, so it can't go on the command line — use \
                 `nd agentium profile secret set <slug> {name} --value-file -`, which reads it \
                 from stdin"
            ));
        }
        Ok(EnvAssign {
            name: name.to_string(),
            value: value.to_string(),
        })
    }
}

/// Check the variable `secret set` stores: a valid name that looks secret.
/// A plain one goes through `profile set --env`, where `show` prints it.
pub fn check_secret_name(slug: &str, name: &str) -> Result<(), String> {
    validate_env_name(name).map_err(|e| e.to_string())?;
    if is_secret_var(name) {
        return Ok(());
    }
    Err(format!(
        "{name} doesn't look secret (its name has no TOKEN, KEY, SECRET or PASSWORD), so \
         `profile show` would print it — set it with `profile set {slug} --env {name}=<value>`"
    ))
}

/// Check a secret value: the trailing newline trimmed, and an empty,
/// multi-line or NUL-holding one refused. A token is one line, and a
/// multi-line one is usually a whole `pass` entry piped in by mistake. No
/// error carries the value.
pub fn parse_secret_value(name: &str, raw: &str) -> Result<String, String> {
    let value = raw.trim_end_matches(['\n', '\r']);
    if value.is_empty() {
        return Err(format!("no value for {name}; nothing stored"));
    }
    let lines = value.lines().count();
    if lines > 1 {
        return Err(format!(
            "the value for {name} spans {lines} lines; pipe just the secret (e.g. `pass show \
             <entry> | head -1`)"
        ));
    }
    if value.contains('\0') {
        return Err(format!("the value for {name} contains a NUL byte"));
    }
    Ok(value.to_string())
}

/// Parse a `--backend` value: `claude` or `openai`.
pub fn parse_backend(arg: &str) -> Result<ProfileBackend, String> {
    ProfileBackend::from_tag(arg).ok_or_else(|| {
        let known = ProfileBackend::ALL.map(ProfileBackend::as_tag).join(", ");
        format!("unknown backend {arg:?}: use one of {known}")
    })
}

/// The error for a slug with no profile, naming the commands that help.
pub fn no_such_profile(slug: &str) -> String {
    format!(
        "no profile '{slug}' — see `nd agentium profile list`, or create it with \
         `nd agentium profile set {slug}`"
    )
}

/// The profile named `slug` taken out of `profiles`, to be changed and
/// republished, or [`no_such_profile`].
pub fn take_profile(
    profiles: &mut BTreeMap<String, BackendProfile>,
    slug: &str,
) -> Result<BackendProfile, String> {
    profiles.remove(slug).ok_or_else(|| no_such_profile(slug))
}

/// What `profile set` makes of `existing` (the newest revision of the slug,
/// if any): the edited profile, and whether it was `"created"` or
/// `"updated"`. An edit that changes nothing on an existing profile is
/// refused, as is one that leaves the default model outside the model list.
pub fn set_profile(
    slug: &str,
    existing: Option<BackendProfile>,
    edit: ProfileEdit,
) -> Result<SetProfile, String> {
    let (action, mut profile) = match existing {
        Some(profile) => {
            if edit.is_empty() {
                return Err(format!(
                    "nothing to change on profile '{slug}' — pass --label, --backend, \
                     --model, --default-model, --config-dir, --env or --unset"
                ));
            }
            ("updated", profile)
        }
        None => ("created", BackendProfile::new(slug, slug)),
    };
    apply_edit(&mut profile, edit)?;
    Ok(SetProfile { action, profile })
}

/// [`set_profile`]'s answer.
pub struct SetProfile {
    /// `"created"` or `"updated"`.
    pub action: &'static str,
    pub profile: BackendProfile,
}

/// Apply `edit` to `profile`, checking the default model is one it offers.
pub fn apply_edit(profile: &mut BackendProfile, edit: ProfileEdit) -> Result<(), String> {
    if let Some(label) = edit.label {
        let label = label.trim();
        if label.is_empty() {
            return Err("--label needs a non-empty value".into());
        }
        profile.label = label.to_string();
    }
    if let Some(backend) = edit.backend {
        profile.backend = backend;
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
            return Err(format!(
                "--unset {name}: profile '{}' has no {name}",
                profile.slug
            ));
        }
    }
    let Some(default) = &profile.default_model else {
        return Ok(());
    };
    if profile.models.is_empty() || profile.models.contains(default) {
        return Ok(());
    }
    Err(format!(
        "the default model {default} isn't one of the profile's models ({}) — pass \
         --default-model",
        profile.models.join(", ")
    ))
}

/// The `--json` view of a profile: every field, with secret values masked
/// exactly as the text view masks them.
#[derive(Serialize)]
pub struct ProfileJson<'a> {
    slug: &'a str,
    label: &'a str,
    backend: ProfileBackend,
    models: &'a [String],
    default_model: Option<&'a str>,
    config_dir: Option<&'a str>,
    env: Vec<EnvJson<'a>>,
    updated_at: u64,
}

/// One env var in [`ProfileJson`]. `value` is `null` for a masked secret and
/// `""` for a var the profile clears from the inherited env.
#[derive(Serialize)]
struct EnvJson<'a> {
    name: &'a str,
    value: Option<&'a str>,
    secret: bool,
}

impl<'a> ProfileJson<'a> {
    pub fn new(p: &'a BackendProfile) -> Self {
        ProfileJson {
            slug: &p.slug,
            label: &p.label,
            backend: p.backend,
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

/// The text form of `profile list`: a bold `slug  label  (backend)` line,
/// then the models (the default starred) and the env var names, never
/// values; or how to make one when there are none.
pub fn render_list<'p>(
    mut profiles: impl Iterator<Item = &'p BackendProfile>,
    color: bool,
) -> String {
    let Some(first) = profiles.next() else {
        return "no backend profiles — create one with `nd agentium profile set <slug>`\n"
            .to_string();
    };
    let mut out = String::new();
    for (i, p) in std::iter::once(first).chain(profiles).enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&paint(color, SGR_BOLD, &p.slug));
        out.push_str(&format!("  {}  ({})\n", p.label, p.backend));
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

/// The text form of `profile show`, relative to `now`, secrets masked.
pub fn render_show(p: &BackendProfile, now: u64) -> String {
    let field = |label: &str, value: &str| format!("{label:<11} {value}\n");
    let mut out = String::new();
    out.push_str(&field("slug", &p.slug));
    out.push_str(&field("label", &p.label));
    out.push_str(&field("backend", p.backend.as_tag()));
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

/// The report of a published `set`, `rm` or secret write: `<what>
/// (event <short>…)`.
pub fn render_published(what: &str, event_id: &str) -> String {
    format!("{what} (event {}…)\n", &event_id[..event_id.len().min(8)])
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
        assert!(render_list(std::iter::empty(), false).starts_with("no backend profiles"));
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
    fn env_assign_refuses_secret_values_but_allows_clearing() {
        let err = EnvAssign::parse("ANTHROPIC_AUTH_TOKEN=x").err().unwrap();
        assert!(err.contains("profile secret set"), "{err}");
        let cleared = EnvAssign::parse("ANTHROPIC_API_KEY=").unwrap();
        assert_eq!(cleared.value, "");
        let url = EnvAssign::parse("ANTHROPIC_BASE_URL=https://a=b").unwrap();
        assert_eq!(url.value, "https://a=b", "splits on the first '=' only");
        assert!(EnvAssign::parse("NOEQUALS").is_err());
        assert!(EnvAssign::parse("1BAD=x").is_err());
    }

    #[test]
    fn secret_names_must_look_secret() {
        assert!(check_secret_name("hydra", "ANTHROPIC_AUTH_TOKEN").is_ok());
        let err = check_secret_name("hydra", "ANTHROPIC_BASE_URL").unwrap_err();
        assert!(err.contains("--env ANTHROPIC_BASE_URL="), "{err}");
        assert!(check_secret_name("hydra", "1BAD_TOKEN").is_err());
    }

    #[test]
    fn set_creates_or_updates_and_refuses_an_empty_edit() {
        let created = set_profile("new", None, ProfileEdit::default()).unwrap();
        assert_eq!(created.action, "created");
        assert_eq!(
            created.profile.label, "new",
            "the label defaults to the slug"
        );
        let err = set_profile("hydra", Some(hydra()), ProfileEdit::default())
            .err()
            .unwrap();
        assert!(err.contains("nothing to change"), "{err}");
        let updated = set_profile(
            "hydra",
            Some(hydra()),
            ProfileEdit {
                label: Some(" Hydra 2 ".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(updated.action, "updated");
        assert_eq!(updated.profile.label, "Hydra 2");
        assert_eq!(
            updated
                .profile
                .env
                .get("ANTHROPIC_AUTH_TOKEN")
                .map(String::as_str),
            Some(TOKEN),
            "an update keeps the secrets"
        );
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
    fn backend_is_set_by_name_and_shown() {
        assert_eq!(parse_backend("openai"), Ok(ProfileBackend::OpenAi));
        let err = parse_backend("gemini").unwrap_err();
        assert!(err.contains("claude, openai"), "{err}");

        let created = set_profile(
            "local",
            None,
            ProfileEdit {
                backend: Some(ProfileBackend::OpenAi),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(created.profile.backend, ProfileBackend::OpenAi);
        let p = created.profile;
        assert!(render_list(std::iter::once(&p), false).starts_with("local  local  (openai)\n"));
        assert!(render_show(&p, 0).contains("backend     openai\n"));
        let json = serde_json::to_value(ProfileJson::new(&p)).unwrap();
        assert_eq!(json["backend"], "openai");
    }

    #[test]
    fn secret_value_trims_newline_and_refuses_junk() {
        assert_eq!(
            parse_secret_value("T", &format!("{TOKEN}\r\n")).unwrap(),
            TOKEN
        );
        assert!(parse_secret_value("T", "\n").is_err());
        let err = parse_secret_value("T", "pw\nlogin: me\n").unwrap_err();
        assert!(err.contains("2 lines") && !err.contains("pw"), "{err}");
    }
}
