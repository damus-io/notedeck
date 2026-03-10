use egui_kittest::Harness;
use notedeck_dave::backend::traits::{BackendType, Model};
use notedeck_dave::config::AiMode;
use notedeck_dave::session::SessionManager;
use notedeck_dave::ui::backend_picker_overlay_ui;
use notedeck_dave::ui::directory_picker::DirectoryPicker;
use notedeck_dave::ui::scene::AgentScene;
use notedeck_dave::update::create_session_with_cwd;
use std::collections::HashMap;
use std::path::PathBuf;

struct PickerState {
    backends: Vec<BackendType>,
    selected_models: HashMap<BackendType, usize>,
    result: Option<(BackendType, Model)>,
}

/// Drive the picker UI with the given backend and model index, press the
/// hotkey for the first backend, and return the resulting selection.
fn pick(bt: BackendType, model_index: usize) -> (BackendType, Model) {
    let mut harness = Harness::new_ui_state(
        |ui, state: &mut PickerState| {
            state.result =
                backend_picker_overlay_ui(&state.backends, &mut state.selected_models, ui);
        },
        PickerState {
            backends: vec![bt],
            selected_models: HashMap::from([(bt, model_index)]),
            result: None,
        },
    );

    harness.run();
    harness.press_key(egui::Key::Num1);
    harness.step();

    harness
        .state()
        .result
        .clone()
        .expect("picker should return a selection")
}

/// Test: index 0 in the picker returns Model::Default (backend picks its own).
#[test]
fn test_picker_default_returns_default() {
    let bt = BackendType::Claude;
    let (picked_backend, picked_model) = pick(bt, 0);

    assert_eq!(picked_backend, bt);
    assert_eq!(picked_model, Model::Default);
    assert!(
        picked_model.to_model_id().is_none(),
        "Default should resolve to None (let CLI pick)"
    );
}

/// Test: index 1 returns Opus for Claude backend.
#[test]
fn test_picker_opus_override() {
    let bt = BackendType::Claude;
    let (picked_backend, picked_model) = pick(bt, 1);

    assert_eq!(picked_backend, bt);
    assert_eq!(picked_model, Model::Opus);
    assert!(
        picked_model
            .to_model_id()
            .unwrap()
            .starts_with("claude-opus"),
        "Opus should resolve to a claude-opus model ID"
    );
}

/// Test: index 2 returns Sonnet for Claude backend.
#[test]
fn test_picker_sonnet_override() {
    let bt = BackendType::Claude;
    let (picked_backend, picked_model) = pick(bt, 2);

    assert_eq!(picked_backend, bt);
    assert_eq!(picked_model, Model::Sonnet);
}

/// Test: picker result flows through create_session_with_cwd to session's resolve_model.
#[test]
fn test_picker_selection_flows_to_session() {
    let bt = BackendType::Claude;
    let mut mgr = SessionManager::new();
    let mut dir_picker = DirectoryPicker::new();
    let mut scene = AgentScene::new();

    // Opus override: resolve_model should return an opus model ID
    let (picked_backend, picked_model) = pick(bt, 1);
    let id = create_session_with_cwd(
        &mut mgr,
        &mut dir_picker,
        &mut scene,
        false,
        AiMode::Agentic,
        PathBuf::from("/tmp"),
        "localhost",
        picked_backend,
        None,
        picked_model,
    );

    let resolved = mgr.get(id).unwrap().details.resolve_model();
    assert!(
        resolved.is_some(),
        "resolve_model should return Some for explicit override"
    );
    assert!(
        resolved.unwrap().starts_with("claude-opus"),
        "resolved model should be an Opus model ID"
    );

    // Default: resolve_model should return None (CLI default)
    let (picked_backend, picked_model) = pick(bt, 0);
    let id = create_session_with_cwd(
        &mut mgr,
        &mut dir_picker,
        &mut scene,
        false,
        AiMode::Agentic,
        PathBuf::from("/tmp"),
        "localhost",
        picked_backend,
        None,
        picked_model,
    );

    assert!(
        mgr.get(id).unwrap().details.resolve_model().is_none(),
        "resolve_model should return None when no override is set"
    );
}

/// Test: Model::from_model_id roundtrips known models.
#[test]
fn test_model_from_model_id() {
    assert_eq!(
        Model::from_model_id("claude-opus-4-6-20250514"),
        Model::Opus
    );
    assert_eq!(
        Model::from_model_id("claude-sonnet-4-6-20250514"),
        Model::Sonnet
    );
    assert_eq!(
        Model::from_model_id("claude-haiku-4-5-20251001"),
        Model::Haiku
    );
    assert_eq!(
        Model::from_model_id("gpt-4.1-mini"),
        Model::Custom("gpt-4.1-mini".to_string())
    );
}
