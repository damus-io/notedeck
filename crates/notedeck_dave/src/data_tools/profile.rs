//! Data tools that edit backend profiles — list, show, set, store or drop a
//! secret, and remove one — so `nd agentium profile …` is what
//! `agentium profile …` was.
//!
//! A backend profile is a named env overlay on the Claude backend, one
//! account-wide kind-31992 event per profile (see
//! [`agentium_core::config::BackendProfile`]). Reads answer from nostrdb
//! alone; writes build the next revision and publish it PNS-wrapped through
//! an embedded engine, as the run-config tools do ([`super::control`]).
//!
//! Nothing a tool answers carries a secret value: the JSON and the text both
//! go through [`BackendProfile::redacted_env`]. `secret set` takes the value
//! as an argument, which nd reads from a file or stdin (`--value-file -`) so
//! it never lands on a command line.

use std::collections::BTreeMap;

use agentium_core::cli::config::replacing;
use agentium_core::cli::profile::{
    check_secret_name, parse_secret_value, render_list, render_published, render_show, set_profile,
    take_profile, EnvAssign, ProfileEdit, ProfileJson,
};
use agentium_core::cli::term::now_secs;
use agentium_core::config::{validate_slug, BackendProfile};
use agentium_core::session_events::{
    build_backend_profile_delete_event_at, build_backend_profile_event_at, BuiltEvent,
};
use agentium_core::session_loader::load_backend_profiles_from_ndb;
use notedeck::{AppTool, RegisteredTool, ToolArg, ToolArgType, ToolContext, ToolSpec};
use serde::{Deserialize, Serialize, Serializer};

use super::control::{event_hex, Writer};
use super::{open_txn, TOOL_CATEGORY};

/// Every profile tool, ready to register.
pub(super) fn tools() -> Vec<RegisteredTool> {
    vec![
        RegisteredTool::new(ProfileList),
        RegisteredTool::new(ProfileShow),
        RegisteredTool::new(ProfileSet),
        RegisteredTool::new(SecretSet),
        RegisteredTool::new(SecretRm),
        RegisteredTool::new(ProfileRm),
    ]
}

/// The selected account's profiles, by slug.
fn profiles(cx: &ToolContext) -> Result<BTreeMap<String, BackendProfile>, String> {
    let txn = open_txn(cx)?;
    let author = *cx.accounts.selected_account_pubkey();
    Ok(load_backend_profiles_from_ndb(cx.ndb, &txn, &author))
}

/// The writer's profiles, read through its own engine's db.
fn writer_profiles(writer: &Writer) -> Result<BTreeMap<String, BackendProfile>, String> {
    let txn = writer.txn()?;
    Ok(load_backend_profiles_from_ndb(
        writer.engine.ndb(),
        &txn,
        &writer.author,
    ))
}

/// `slug`, checked as a profile slug.
fn checked_slug(slug: &str) -> Result<&str, String> {
    validate_slug(slug).map_err(|e| e.to_string())?;
    Ok(slug)
}

/// The slug argument every tool on one profile takes.
fn slug_arg() -> ToolArg {
    ToolArg::new(
        "slug",
        ToolArgType::String,
        "The profile's slug: 1-63 of a-z, 0-9 and -.",
    )
    .required(true)
    .positional()
}

/// The secret variable argument.
fn name_arg(what: &'static str) -> ToolArg {
    ToolArg::new("name", ToolArgType::String, what)
        .required(true)
        .positional()
}

/// Publish a built kind-31992 event as the writer, answering its id in hex.
fn publish(writer: &Writer, built: &BuiltEvent, slug: &str) -> Result<String, String> {
    writer
        .engine
        .publish_event(built)
        .map_err(|e| format!("publishing profile {slug}: {e:?}"))?;
    Ok(event_hex(built))
}

/// Build the next revision of `profile` and publish it.
fn publish_profile(writer: &Writer, profile: &BackendProfile) -> Result<String, String> {
    let built = build_backend_profile_event_at(
        profile,
        Some(replacing(profile.updated_at)),
        &writer.secret,
    )
    .map_err(|e| format!("building profile {}: {e}", profile.slug))?;
    publish(writer, &built, &profile.slug)
}

/// `agentium_profile_list`: every backend profile.
struct ProfileList;

/// [`ProfileList`]'s answer: the profiles in slug order.
struct Profiles(BTreeMap<String, BackendProfile>);

impl Serialize for Profiles {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.values().map(ProfileJson::new))
    }
}

#[derive(Deserialize)]
struct NoArgs {}

impl AppTool for ProfileList {
    type Args = NoArgs;
    type Output = Profiles;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_profile_list",
            "List the backend profiles — named env overlays on the Claude backend (endpoint, credentials, models) a session can run under — in slug order: `[{slug, label, backend, models, default_model, config_dir, env: [{name, value, secret}], updated_at}]`. A secret's value is always null.",
            vec![],
        )
        .titled("List backend profiles")
        .read_only()
        .command(&["agentium", "profile", "list"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, _args: Self::Args) -> Result<Self::Output, String> {
        profiles(cx).map(Profiles)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(render_list(out.0.values(), true))
    }
}

/// `agentium_profile_show`: one profile in full, secrets masked.
struct ProfileShow;

/// Arguments for the tools acting on one profile.
#[derive(Deserialize)]
struct SlugArgs {
    slug: String,
}

/// One profile, as [`ProfileJson`].
struct OneProfile(BackendProfile);

impl Serialize for OneProfile {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ProfileJson::new(&self.0).serialize(serializer)
    }
}

impl AppTool for ProfileShow {
    type Args = SlugArgs;
    type Output = OneProfile;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_profile_show",
            "Show one backend profile, secrets masked: `{slug, label, backend, models, default_model, config_dir, env: [{name, value, secret}], updated_at}`.",
            vec![slug_arg()],
        )
        .titled("Show backend profile")
        .read_only()
        .command(&["agentium", "profile", "show"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let slug = checked_slug(&args.slug)?;
        take_profile(&mut profiles(cx)?, slug).map(OneProfile)
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(render_show(&out.0, now_secs()))
    }
}

/// What `set` published: `{action, event_id, profile}`.
struct SetAnswer {
    action: &'static str,
    event_id: String,
    profile: BackendProfile,
}

impl Serialize for SetAnswer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde_json::json!({
            "action": self.action,
            "event_id": self.event_id,
            "profile": ProfileJson::new(&self.profile),
        })
        .serialize(serializer)
    }
}

/// `agentium_profile_set`: create a profile, or change one.
struct ProfileSet;

/// Arguments for [`ProfileSet`].
#[derive(Deserialize)]
struct SetArgs {
    slug: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    model: Vec<String>,
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    config_dir: Option<String>,
    #[serde(default)]
    env: Vec<String>,
    #[serde(default)]
    unset: Vec<String>,
}

impl AppTool for ProfileSet {
    type Args = SetArgs;
    type Output = SetAnswer;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_profile_set",
            "Create a backend profile, or change one: a whole-profile replace that keeps every field the call leaves out. `model` replaces the whole model list; `default_model` and `config_dir` take \"\" to clear. A secret-looking var (its name has TOKEN, KEY, SECRET or PASSWORD) can only be cleared here (`NAME=`); store one with `agentium_profile_secret_set`. Answers `{action, event_id, profile}`, `action` being `created` or `updated`.",
            vec![
                slug_arg(),
                ToolArg::new("label", ToolArgType::String, "What the backend picker calls it. Defaults to the slug for a new profile."),
                ToolArg::new("model", ToolArgType::Strings, "A model id the picker offers, in order; replaces the whole list."),
                ToolArg::new("default_model", ToolArgType::String, "The model a new session starts on; one of `model`. \"\" clears it."),
                ToolArg::new("config_dir", ToolArgType::String, "The Claude config dir (`CLAUDE_CONFIG_DIR`) its sessions use. \"\" clears it, sharing ~/.claude."),
                ToolArg::new("env", ToolArgType::Strings, "`NAME=VALUE`, an env var its sessions get; `NAME=` removes NAME from the inherited environment."),
                ToolArg::new("unset", ToolArgType::Strings, "A var NAME to drop from the profile."),
            ],
        )
        .titled("Set backend profile")
        .command(&["agentium", "profile", "set"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let slug = checked_slug(&args.slug)?.to_owned();
        let edit = ProfileEdit {
            label: args.label,
            models: args.model,
            default_model: args.default_model,
            config_dir: args.config_dir,
            env: args
                .env
                .iter()
                .map(|assign| EnvAssign::parse(assign))
                .collect::<Result<_, _>>()?,
            unset: args.unset,
        };
        let writer = Writer::new(cx)?;
        let existing = writer_profiles(&writer)?.remove(&slug);
        let set = set_profile(&slug, existing, edit)?;
        let event_id = publish_profile(&writer, &set.profile)?;
        Ok(SetAnswer {
            action: set.action,
            event_id,
            profile: set.profile,
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        let what = format!("{} profile {}", out.action, out.profile.slug);
        Some(render_published(&what, &out.event_id))
    }
}

/// What a secret write published: `{action, event_id, slug, name}`.
#[derive(Serialize)]
struct SecretAnswer {
    action: &'static str,
    event_id: String,
    slug: String,
    name: String,
}

/// The text a secret write prints: the variable by name, never its value.
fn render_secret(verb: &str, out: &SecretAnswer) -> String {
    let what = format!("{verb} {} on profile {}", out.name, out.slug);
    render_published(&what, &out.event_id)
}

/// `agentium_profile_secret_set`: store one secret var on a profile.
struct SecretSet;

/// Arguments for [`SecretSet`].
#[derive(Deserialize)]
struct SecretSetArgs {
    slug: String,
    name: String,
    value: String,
}

impl AppTool for SecretSet {
    type Args = SecretSetArgs;
    type Output = SecretAnswer;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_profile_secret_set",
            "Store one secret env var (its name has TOKEN, KEY, SECRET or PASSWORD) on an existing backend profile. The value is one line and is never shown again: every read masks it. Answers `{action, event_id, slug, name}`.",
            vec![
                slug_arg(),
                name_arg("The secret variable, e.g. ANTHROPIC_AUTH_TOKEN."),
                ToolArg::new("value", ToolArgType::String, "The secret, one line. On a command line give it with --value-file - and pipe it in, so it stays out of shell history.")
                    .required(true)
                    .from_file("value-file"),
            ],
        )
        .titled("Set backend profile secret")
        .command(&["agentium", "profile", "secret", "set"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let slug = checked_slug(&args.slug)?;
        check_secret_name(slug, &args.name)?;
        let value = parse_secret_value(&args.name, &args.value)?;
        let writer = Writer::new(cx)?;
        let mut profile = take_profile(&mut writer_profiles(&writer)?, slug)?;
        profile.env.insert(args.name.clone(), value);
        Ok(SecretAnswer {
            action: "secret stored",
            event_id: publish_profile(&writer, &profile)?,
            slug: slug.to_owned(),
            name: args.name,
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(render_secret("stored", out))
    }
}

/// `agentium_profile_secret_rm`: drop one secret var from a profile.
struct SecretRm;

/// Arguments for [`SecretRm`].
#[derive(Deserialize)]
struct SecretRmArgs {
    slug: String,
    name: String,
}

impl AppTool for SecretRm {
    type Args = SecretRmArgs;
    type Output = SecretAnswer;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_profile_secret_rm",
            "Drop one secret env var from a backend profile. Answers `{action, event_id, slug, name}`.",
            vec![slug_arg(), name_arg("The secret variable to drop.")],
        )
        .titled("Remove backend profile secret")
        .command(&["agentium", "profile", "secret", "rm"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let slug = checked_slug(&args.slug)?;
        let writer = Writer::new(cx)?;
        let mut profile = take_profile(&mut writer_profiles(&writer)?, slug)?;
        if profile.env.remove(&args.name).is_none() {
            return Err(format!("profile '{slug}' has no {}", args.name));
        }
        Ok(SecretAnswer {
            action: "secret removed",
            event_id: publish_profile(&writer, &profile)?,
            slug: slug.to_owned(),
            name: args.name,
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        Some(render_secret("removed", out))
    }
}

/// `agentium_profile_rm`: delete a profile.
struct ProfileRm;

/// What `rm` published: `{action, event_id, slug}`.
#[derive(Serialize)]
struct RmAnswer {
    action: &'static str,
    event_id: String,
    slug: String,
}

impl AppTool for ProfileRm {
    type Args = SlugArgs;
    type Output = RmAnswer;

    fn spec(&self) -> ToolSpec {
        ToolSpec::new(
            "agentium_profile_rm",
            "Delete a backend profile (a tombstone revision). Answers `{action, event_id, slug}`.",
            vec![slug_arg()],
        )
        .titled("Remove backend profile")
        .command(&["agentium", "profile", "rm"])
        .filed_under(TOOL_CATEGORY)
    }

    fn call(&self, cx: &mut ToolContext, args: Self::Args) -> Result<Self::Output, String> {
        let slug = checked_slug(&args.slug)?;
        let writer = Writer::new(cx)?;
        let profile = take_profile(&mut writer_profiles(&writer)?, slug)?;
        let built = build_backend_profile_delete_event_at(
            slug,
            Some(replacing(profile.updated_at)),
            &writer.secret,
        )
        .map_err(|e| format!("building the profile delete: {e}"))?;
        Ok(RmAnswer {
            action: "removed",
            event_id: publish(&writer, &built, slug)?,
            slug: slug.to_owned(),
        })
    }

    fn render(&self, out: &Self::Output) -> Option<String> {
        let what = format!("removed profile {}", out.slug);
        Some(render_published(&what, &out.event_id))
    }
}
