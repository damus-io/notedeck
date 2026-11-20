//! `nd agentium spec`: set overrides a built-in field by field or whole from
//! a file, show and list say where each spec comes from, and rm brings the
//! built-in back, each write read back from nostrdb the way Dave loads
//! specs.

use super::*;
use agentium_core::agent_spec::builtin;
use agentium_core::agent_spec::record::{self, load_agent_spec_records_from_ndb, SpecRecord};
use std::collections::BTreeMap;

impl Live {
    /// Wait for the one event a write just published, then load the
    /// account's spec records as Dave does.
    fn specs_after_write(&mut self) -> BTreeMap<String, SpecRecord> {
        self.pending += 1;
        self.settle();
        let ndb = self.notedeck.app_context().ndb.clone();
        let txn = Transaction::new(&ndb).expect("txn");
        let author = FullKeypair::from_secret_bytes(&self.secret)
            .expect("key")
            .pubkey;
        load_agent_spec_records_from_ndb(&ndb, &txn, &author)
    }
}

/// set a built-in's model → show/list call it an override → set a new spec
/// from a file → rm the override, which brings the built-in back.
#[test]
fn set_show_list_rm() {
    let mut live = live();
    let listed = live.call("agentium_spec_list", json!({}));
    assert_eq!(listed.json[0]["slug"], "butler");
    assert_eq!(listed.json[0]["source"], "builtin");

    let set = live.call(
        "agentium_spec_set",
        json!({ "slug": "butler", "model": "qwen3", "prompt": "Be brief." }),
    );
    assert_eq!(set.json["action"], "created");
    assert!(
        set.text.starts_with("created agent spec butler (event "),
        "{}",
        set.text
    );
    let records = live.specs_after_write();
    let butler = record::lookup("butler", &records).unwrap();
    assert_eq!(butler.model.model, "qwen3");
    assert_eq!(butler.prompt, "Be brief.");
    assert_eq!(
        butler.toolsets,
        builtin::find("butler").unwrap().toolsets,
        "the fields not set are the built-in's"
    );

    let shown = live.call("agentium_spec_show", json!({ "slug": "butler" }));
    assert_eq!(shown.json["source"], "account");
    assert_eq!(shown.json["overrides_builtin"], true);
    assert_eq!(shown.json["spec"]["model"]["model"], "qwen3");
    assert!(
        shown.text.contains("overrides the built-in"),
        "{}",
        shown.text
    );

    let mut scout = builtin::find("researcher").unwrap();
    scout.slug = "scout".into();
    let file = serde_json::to_string(&scout).unwrap();
    let created = live.call(
        "agentium_spec_set",
        json!({ "slug": "scout", "json": file }),
    );
    assert_eq!(created.json["action"], "created");
    assert_eq!(
        record::lookup("scout", &live.specs_after_write()).unwrap(),
        scout
    );
    let slugs: Vec<Value> = live
        .call("agentium_spec_list", json!({}))
        .json
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["slug"].clone())
        .collect();
    assert_eq!(
        slugs,
        [
            json!("butler"),
            json!("dave-code"),
            json!("researcher"),
            json!("sandboxed-coder"),
            json!("scout")
        ]
    );

    let edited = live.call(
        "agentium_spec_set",
        json!({ "slug": "butler", "model": "qwen3.5" }),
    );
    assert_eq!(edited.json["action"], "updated");
    assert_eq!(
        record::lookup("butler", &live.specs_after_write())
            .unwrap()
            .prompt,
        "Be brief.",
        "an edit keeps the record's other fields"
    );

    let removed = live.call("agentium_spec_rm", json!({ "slug": "butler" }));
    assert_eq!(removed.json["action"], "removed");
    let records = live.specs_after_write();
    assert_eq!(
        record::lookup("butler", &records).unwrap(),
        builtin::find("butler").unwrap()
    );
}

/// A spec that doesn't validate is refused before anything is published,
/// as are a file whose slug isn't the one named, a slug nobody has, and rm
/// of a built-in the account never overrode.
#[test]
fn bad_writes_are_refused() {
    let mut live = live();
    let mut leaky = builtin::find("butler").unwrap();
    leaky.toolsets.push(agentium_core::agent_spec::Toolset::Web);
    let err = live
        .try_call(
            "agentium_spec_set",
            json!({ "slug": "butler", "json": serde_json::to_string(&leaky).unwrap() }),
        )
        .unwrap_err();
    assert!(err.contains("network"), "{err}");

    let err = live
        .try_call(
            "agentium_spec_set",
            json!({ "slug": "scout", "json": serde_json::to_string(&leaky).unwrap() }),
        )
        .unwrap_err();
    assert!(err.contains("not \"scout\""), "{err}");

    let err = live
        .try_call(
            "agentium_spec_set",
            json!({ "slug": "scout", "model": "qwen3" }),
        )
        .unwrap_err();
    assert!(err.contains("--file spec.json"), "{err}");

    let err = live
        .try_call("agentium_spec_rm", json!({ "slug": "butler" }))
        .unwrap_err();
    assert!(err.contains("built-in"), "{err}");
}
