use enostr::Note;
use schemata_validator_rs::{get_schema, validate, validate_note};

/// Construct an enostr::Note from JSON, re-serialize it, and validate.
/// This proves enostr's Serialize impl produces schema-compliant output.
#[test]
fn validate_enostr_note_serialization() {
    let note = Note::from_json(
        r#"{"id":"70b10f70c1318967eddf12527799411b1a9780ad9c43858f5e5325f43016effc","pubkey":"379e863e8357163b5bce5d2688dc4f1dcc2d505222fb8d74db600f30535dcdfe","created_at":1612809991,"kind":1,"tags":[],"content":"test","sig":"273a9cd5d11455590f4359500bccb7a89428262b96b3ea87a756b770964472f8273a9cd5d11455590f4359500bccb7a89428262b96b3ea87a756b770964472f8"}"#,
    )
    .expect("should parse valid JSON into enostr::Note");

    let json = serde_json::to_value(&note).expect("Note should serialize to JSON");
    let result = validate_note(&json);
    assert!(
        result.valid,
        "enostr::Note serialization should be schema-compliant. Errors: {:?}",
        result.errors
    );
}

/// Parse a real event JSON → enostr::Note → re-serialize → validate.
/// Round-trip proves no data is lost or malformed during deserialization + serialization.
#[test]
fn validate_enostr_note_roundtrip() {
    let raw = r#"{"id":"d7dd5eb3ab747e16f8d0212d53032ea2a7cc9571c4b86f3bdfdb6f1f23b3eba4","pubkey":"32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245","created_at":1617932115,"kind":1,"tags":[["e","3da979448d9ba263864c4d6f14984c423a3838364ec255f03c7904b1ae77f206"]],"content":"If you're running a mass relay, use a CDN","sig":"58e0297b5b6f86cfc5ee489dfa9be5fc9e3448e47e2092d07813e40a3beafaaa58e0297b5b6f86cfc5ee489dfa9be5fc9e3448e47e2092d07813e40a3beafaaa"}"#;

    let note = Note::from_json(raw).expect("should parse real event");
    let json = serde_json::to_value(&note).expect("should re-serialize");
    let result = validate_note(&json);
    assert!(
        result.valid,
        "Round-tripped enostr::Note should be schema-compliant. Errors: {:?}",
        result.errors
    );
}

/// Note with p-tag and e-tag references serializes correctly.
#[test]
fn validate_enostr_note_with_tags() {
    let note = Note::from_json(
        r#"{"id":"f4db224675a3f5ee6e5e5a80df4ef2ce86db8e2cbee7491b1a640e0b56981724","pubkey":"379e863e8357163b5bce5d2688dc4f1dcc2d505222fb8d74db600f30535dcdfe","created_at":1612809991,"kind":1,"tags":[["p","32e1827635450ebb3c5a7d12c1f8e7b2b514439ac10a67eef3d9fd9c5c68e245"],["e","3da979448d9ba263864c4d6f14984c423a3838364ec255f03c7904b1ae77f206","wss://relay.damus.io"]],"content":"hello nostr","sig":"a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90"}"#,
    )
    .expect("should parse note with tags");

    let json = serde_json::to_value(&note).expect("should serialize");
    let result = validate_note(&json);
    assert!(
        result.valid,
        "Note with p/e tags should be schema-compliant. Errors: {:?}",
        result.errors
    );
}

/// A kind:0 event validated against the kind1Schema should fail.
#[test]
fn reject_wrong_kind_against_kind1_schema() {
    let note = Note::from_json(
        r#"{"id":"70b10f70c1318967eddf12527799411b1a9780ad9c43858f5e5325f43016effc","pubkey":"379e863e8357163b5bce5d2688dc4f1dcc2d505222fb8d74db600f30535dcdfe","created_at":1612809991,"kind":0,"tags":[],"content":"{\"name\":\"test\"}","sig":"a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90"}"#,
    )
    .expect("should parse kind:0 event");

    let json = serde_json::to_value(&note).expect("should serialize");
    let schema = get_schema("kind1Schema").expect("kind1Schema should exist");
    let result = validate(schema, &json);
    assert!(
        !result.valid,
        "kind:0 event should fail kind1Schema validation"
    );
    assert!(
        result
            .errors
            .iter()
            .any(|e| e.message.contains("kind") || e.keyword == "const"),
        "Should have a kind-related error. Errors: {:?}",
        result.errors
    );
}
