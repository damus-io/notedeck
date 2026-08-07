//! Regression snapshot for the AskUserQuestion permission widget.
//!
//! The question prompt is rendered inside the narrow Dave chat column. Long
//! question text, option labels/descriptions, and the "Other" text field must
//! wrap/flex to the column width — otherwise the frame grows past the right
//! edge and the Submit button (in a right-to-left layout) is pushed off-screen
//! and becomes unclickable on mobile. This test pins that behaviour at a
//! deliberately narrow width so it can't regress.

use agentium_core::messages::{
    PermissionRequest, PermissionView, QuestionAnswer, QuestionOption, QuestionSetInput,
    UserQuestion,
};
use egui_kittest::Harness;
use notedeck_dave::ui::ask_user_question_ui;
use std::collections::HashMap;
use uuid::Uuid;

/// A deliberately verbose question set: long question, long option labels and
/// descriptions, so wrapping is actually exercised at a narrow width.
fn long_question_set() -> QuestionSetInput {
    QuestionSetInput {
        questions: vec![UserQuestion {
            header: "Auth method".to_string(),
            question: "Which authentication approach should we use for the new mobile \
                onboarding flow, considering we also need to keep supporting the legacy \
                token system for existing users?"
                .to_string(),
            multi_select: false,
            options: vec![
                QuestionOption {
                    label: "OAuth with refresh tokens and automatic silent renewal".to_string(),
                    description: "Use the standard OAuth 2.0 authorization code flow with \
                        PKCE, storing refresh tokens securely and renewing access tokens \
                        silently before they expire."
                        .to_string(),
                },
                QuestionOption {
                    label: "Nostr key-based per-session signing".to_string(),
                    description: "Sign every request with the user's Nostr secret key, \
                        deriving a per-session token so the raw key never travels over the \
                        wire."
                        .to_string(),
                },
            ],
        }],
    }
}

/// Render `ask_user_question_ui` at a narrow (mobile-column) width with the
/// "Other" text field open, so the whole frame — including the Submit button —
/// must fit within the column.
fn ask_question_harness() -> Harness<'static> {
    let questions = long_question_set();
    let request = PermissionRequest::new(
        Uuid::from_u128(1),
        "AskUserQuestion".to_string(),
        serde_json::Value::Null,
        Some(PermissionView::QuestionSet(questions.clone())),
        None,
        None,
    );

    // Pre-seed the "Other" answer so its text field renders and its width is
    // exercised by the snapshot.
    let mut answers_map: HashMap<Uuid, Vec<QuestionAnswer>> = HashMap::new();
    answers_map.insert(
        request.id,
        vec![QuestionAnswer {
            selected: vec![],
            other_text: Some("a custom typed-in answer".to_string()),
        }],
    );
    let mut index_map: HashMap<Uuid, usize> = HashMap::new();

    Harness::builder()
        .with_size(egui::Vec2::new(320.0, 480.0))
        .renderer(notedeck::software_renderer())
        .build_ui(move |ui| {
            ask_user_question_ui(&request, &questions, &mut answers_map, &mut index_map, ui);
        })
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn test_ask_question_wrap_snapshot() {
    let mut harness = ask_question_harness();
    harness.run();
    harness.snapshot("ask_question_wrap");
}
