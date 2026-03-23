use schemata_validator_rs::{validate, get_schema, validate_note};
use serde_json::json;

#[test]
fn validate_kind1_event() {
    let event = json!({
        "id": "a".repeat(64),
        "pubkey": "b".repeat(64),
        "created_at": 1670000000u64,
        "kind": 1,
        "tags": [],
        "content": "Hello, Nostr!",
        "sig": "e".repeat(64) + &"f".repeat(64)
    });

    let result = validate_note(&event);
    assert!(result.valid, "Valid kind-1 event should pass. Errors: {:?}", result.errors);
}

#[test]
fn reject_kind1_missing_fields() {
    let event = json!({
        "kind": 1,
        "content": "hello"
        // missing id, pubkey, created_at, tags, sig
    });

    let result = validate_note(&event);
    assert!(!result.valid, "Missing required fields should fail");
    assert!(!result.errors.is_empty());
}

#[test]
fn validate_kind1_with_tags() {
    let event = json!({
        "id": "a".repeat(64),
        "pubkey": "b".repeat(64),
        "created_at": 1670000000u64,
        "kind": 1,
        "tags": [
            ["p", "c".repeat(64)],
            ["e", "d".repeat(64), "wss://relay.example.com"]
        ],
        "content": "hello nostr",
        "sig": "e".repeat(64) + &"f".repeat(64)
    });

    let result = validate_note(&event);
    assert!(result.valid, "Kind-1 with p/e tags should pass. Errors: {:?}", result.errors);
}

#[test]
fn reject_kind1_wrong_kind() {
    // Validate a kind:0 event directly against the kind1Schema
    let schema = get_schema("kind1Schema").expect("kind1Schema should exist");
    let event = json!({
        "id": "a".repeat(64),
        "pubkey": "b".repeat(64),
        "created_at": 1670000000u64,
        "kind": 0,
        "tags": [],
        "content": "Hello, Nostr!",
        "sig": "e".repeat(64) + &"f".repeat(64)
    });

    let result = validate(schema, &event);
    assert!(!result.valid, "kind:0 event should fail kind1Schema validation");
    assert!(
        result.errors.iter().any(|e| e.message.contains("kind") || e.keyword == "const"),
        "Should have a kind-related error. Errors: {:?}", result.errors
    );
}
