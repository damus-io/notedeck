//! The `nd agentium profile` CLI's cases (`profile_lands.rs` and the unit
//! tests beside `profile_cmd.rs`), ported to the tools that replace it: set,
//! store a secret, show and list it masked, then remove it, each write read
//! back from nostrdb the way Dave loads profiles.

use super::*;
use agentium_core::config::BackendProfile;
use agentium_core::session_loader::load_backend_profiles_from_ndb;
use std::collections::BTreeMap;

/// The secret `secret set` stores. Distinctive, so a leak is unmistakable.
const TOKEN: &str = "hydra-tok-4f1c2e9b7a0d63e5";

impl Live {
    /// Wait for the one event a write just published, then load the
    /// account's profiles as Dave does.
    fn profiles_after_write(&mut self) -> BTreeMap<String, BackendProfile> {
        self.pending += 1;
        self.settle();
        let ndb = self.notedeck.app_context().ndb.clone();
        let txn = Transaction::new(&ndb).expect("txn");
        let author = FullKeypair::from_secret_bytes(&self.secret)
            .expect("key")
            .pubkey;
        load_backend_profiles_from_ndb(&ndb, &txn, &author)
    }
}

/// Assert neither form of an answer carries [`TOKEN`].
fn no_leak(answer: &Answer) {
    assert!(
        !answer.text.contains(TOKEN) && !answer.json.to_string().contains(TOKEN),
        "the token leaked: {answer:?}"
    );
}

/// set → secret set → show/list (token masked, the rest shown) → secret rm
/// → rm, each write landing in nostrdb.
#[test]
fn set_secret_show_rm() {
    let mut live = live();
    let set = live.call(
        "agentium_profile_set",
        json!({
            "slug": "hydra",
            "label": "Hydra",
            "env": ["ANTHROPIC_BASE_URL=https://llm.hydra.host", "ANTHROPIC_API_KEY=", "ANTHROPIC_CUSTOM_HEADERS=a: b, c: d"],
            "model": ["glm-5.3-flash", "qwen3.8-flash-next"],
            "default_model": "glm-5.3-flash",
        }),
    );
    assert_eq!(set.json["action"], "created");
    assert!(
        set.text.starts_with("created profile hydra (event "),
        "{}",
        set.text
    );
    let profiles = live.profiles_after_write();
    let hydra = &profiles["hydra"];
    assert_eq!(hydra.models, ["glm-5.3-flash", "qwen3.8-flash-next"]);
    assert_eq!(
        hydra
            .env
            .get("ANTHROPIC_CUSTOM_HEADERS")
            .map(String::as_str),
        Some("a: b, c: d"),
        "a value with a comma is kept whole"
    );

    let stored = live.call(
        "agentium_profile_secret_set",
        json!({ "slug": "hydra", "name": "ANTHROPIC_AUTH_TOKEN", "value": format!("{TOKEN}\n") }),
    );
    no_leak(&stored);
    assert_eq!(stored.json["action"], "secret stored");
    assert!(
        stored
            .text
            .starts_with("stored ANTHROPIC_AUTH_TOKEN on profile hydra (event "),
        "{}",
        stored.text
    );
    let profiles = live.profiles_after_write();
    let hydra = &profiles["hydra"];
    assert_eq!(
        hydra.env.get("ANTHROPIC_AUTH_TOKEN").map(String::as_str),
        Some(TOKEN),
        "the newline is trimmed"
    );
    assert_eq!(
        hydra.env.get("ANTHROPIC_BASE_URL").map(String::as_str),
        Some("https://llm.hydra.host"),
        "the secret write keeps the set's fields"
    );

    let shown = live.call("agentium_profile_show", json!({ "slug": "hydra" }));
    no_leak(&shown);
    assert!(
        shown.text.contains("ANTHROPIC_AUTH_TOKEN      ********"),
        "{}",
        shown.text
    );
    assert!(
        shown.text.contains("https://llm.hydra.host"),
        "{}",
        shown.text
    );
    assert!(
        shown.text.contains("ANTHROPIC_API_KEY         (unset)"),
        "{}",
        shown.text
    );
    assert!(
        shown
            .text
            .contains("glm-5.3-flash (default), qwen3.8-flash-next"),
        "{}",
        shown.text
    );
    assert_eq!(shown.json["label"], "Hydra");
    assert_eq!(shown.json["default_model"], "glm-5.3-flash");

    let list = live.call("agentium_profile_list", json!({}));
    no_leak(&list);
    assert!(
        list.text.contains("hydra  Hydra  (claude)"),
        "{}",
        list.text
    );
    assert!(list.text.contains("ANTHROPIC_AUTH_TOKEN"), "{}", list.text);
    assert_eq!(list.json.as_array().map(Vec::len), Some(1));

    let dropped = live.call(
        "agentium_profile_secret_rm",
        json!({ "slug": "hydra", "name": "ANTHROPIC_AUTH_TOKEN" }),
    );
    assert_eq!(dropped.json["action"], "secret removed");
    let profiles = live.profiles_after_write();
    assert!(!profiles["hydra"].env.contains_key("ANTHROPIC_AUTH_TOKEN"));

    let removed = live.call("agentium_profile_rm", json!({ "slug": "hydra" }));
    assert!(
        removed.text.starts_with("removed profile hydra"),
        "{}",
        removed.text
    );
    assert!(
        live.profiles_after_write().is_empty(),
        "the tombstone removes it"
    );
    let list = live.call("agentium_profile_list", json!({}));
    assert_eq!(list.json, json!([]));
    assert!(
        list.text.starts_with("no backend profiles"),
        "{}",
        list.text
    );
}

/// A secret can't go through `env`, `secret set` refuses a plain name, a
/// junk value or a missing profile, and no refusal echoes the value.
#[test]
fn secrets_stay_out_of_plain_args_and_errors() {
    let mut live = live();
    let err = live
        .try_call(
            "agentium_profile_set",
            json!({ "slug": "hydra", "env": [format!("ANTHROPIC_AUTH_TOKEN={TOKEN}")] }),
        )
        .unwrap_err();
    assert!(err.contains("profile secret set"), "{err}");
    assert!(!err.contains(TOKEN), "the refusal must not echo it: {err}");

    let err = live
        .try_call(
            "agentium_profile_secret_set",
            json!({ "slug": "hydra", "name": "ANTHROPIC_AUTH_TOKEN", "value": TOKEN }),
        )
        .unwrap_err();
    assert!(err.starts_with("no profile 'hydra'"), "{err}");
    assert!(!err.contains(TOKEN), "{err}");

    live.call("agentium_profile_set", json!({ "slug": "hydra" }));
    live.profiles_after_write();
    let err = live
        .try_call(
            "agentium_profile_secret_set",
            json!({ "slug": "hydra", "name": "ANTHROPIC_BASE_URL", "value": "x" }),
        )
        .unwrap_err();
    assert!(err.contains("doesn't look secret"), "{err}");
    let err = live
        .try_call(
            "agentium_profile_secret_set",
            json!({ "slug": "hydra", "name": "ANTHROPIC_AUTH_TOKEN", "value": format!("{TOKEN}\nlogin: me") }),
        )
        .unwrap_err();
    assert!(err.contains("2 lines") && !err.contains(TOKEN), "{err}");

    let err = live
        .try_call("agentium_profile_set", json!({ "slug": "hydra" }))
        .unwrap_err();
    assert!(err.contains("nothing to change"), "{err}");
    let err = live
        .try_call("agentium_profile_show", json!({ "slug": "Hydra" }))
        .unwrap_err();
    assert!(err.contains("invalid profile slug"), "{err}");
}

/// The command paths, so `nd agentium profile …` reaches each; the reads
/// are read-only, the writes aren't, and the secret's value has a file form
/// so nd can read it from stdin.
#[test]
fn profile_tools_have_their_command_paths() {
    let paths: Vec<(String, bool)> = super::super::profile::tools()
        .iter()
        .map(|t| (t.spec().command_path().join(" "), t.spec().is_read_only()))
        .collect();
    let expected = [
        ("agentium profile list", true),
        ("agentium profile show", true),
        ("agentium profile set", false),
        ("agentium profile secret set", false),
        ("agentium profile secret rm", false),
        ("agentium profile rm", false),
    ];
    let expected: Vec<(String, bool)> = expected.iter().map(|(p, r)| (p.to_string(), *r)).collect();
    assert_eq!(paths, expected);

    let secret = super::super::profile::tools()
        .into_iter()
        .find(|t| t.name() == "agentium_profile_secret_set")
        .expect("secret set");
    let value = secret
        .spec()
        .arguments()
        .iter()
        .find(|a| a.name() == "value")
        .map(|a| a.cli().from_file)
        .expect("a value arg");
    assert_eq!(value, Some("value-file"));
}
