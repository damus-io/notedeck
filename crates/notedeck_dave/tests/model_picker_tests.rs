use agentium_core::config::BackendProfile;
use notedeck::test_harness::PressKey;
use notedeck_dave::backend::traits::{BackendType, Model};
use notedeck_dave::backend::BackendChoice;
use notedeck_dave::config::AiMode;
use notedeck_dave::session::SessionManager;
use notedeck_dave::ui::backend_picker::picker_entries;
use notedeck_dave::ui::directory_picker::DirectoryPicker;
use notedeck_dave::ui::scene::AgentScene;
use notedeck_dave::ui::{backend_picker_overlay_ui, PickerEntry, PickerKey};
use notedeck_dave::update::create_session_with_cwd;
use std::collections::HashMap;
use std::path::PathBuf;

struct PickerState {
    entries: Vec<PickerEntry>,
    selected_models: HashMap<PickerKey, usize>,
    result: Option<BackendChoice>,
}

/// Drive the picker UI over `entries`, with the first row's model index set
/// to `model_index`, press the number key for row `row`, and return the
/// resulting selection.
fn pick_row(entries: Vec<PickerEntry>, row: usize, model_index: usize) -> BackendChoice {
    let selected_models = HashMap::from([(entries[0].key.clone(), model_index)]);
    let key = [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3][row];
    let mut harness = notedeck::test_harness::lenient_builder().build_ui_state(
        |ui, state: &mut PickerState| {
            state.result =
                backend_picker_overlay_ui(&state.entries, &mut state.selected_models, ui);
        },
        PickerState {
            entries,
            selected_models,
            result: None,
        },
    );

    harness.run();
    harness.press_key(key);
    harness.step();

    harness
        .state()
        .result
        .clone()
        .expect("picker should return a selection")
}

/// [`pick_row`] on a picker of `bt` alone, pressing its key.
fn pick(bt: BackendType, model_index: usize) -> BackendChoice {
    pick_row(picker_entries(&[bt], []), 0, model_index)
}

/// The account's `hydra` profile, as `agentium profile set hydra` makes it.
fn hydra() -> BackendProfile {
    let mut p = BackendProfile::new("hydra", "Hydra");
    p.models = vec!["glm-5.3-flash".into(), "qwen3.8-flash-next".into()];
    p.default_model = Some("glm-5.3-flash".into());
    p
}

/// A lone Claude with a profile shows the profile as its own row; picking it
/// starts a session under the profile on its default model.
#[test]
fn test_picker_profile_row_starts_a_profile_session() {
    let entries = picker_entries(&[BackendType::Claude], [&hydra()]);
    assert_eq!(entries.len(), 2, "plain Claude, then Claude under hydra");
    let choice = pick_row(entries, 1, 0);
    assert_eq!(
        choice,
        BackendChoice {
            backend: BackendType::Claude,
            profile: Some("hydra".into()),
            model: Model::Custom("glm-5.3-flash".into()),
        }
    );

    let mut mgr = SessionManager::new();
    let id = create_session_with_cwd(
        &mut mgr,
        &mut DirectoryPicker::new(),
        &mut AgentScene::new(),
        false,
        AiMode::Agentic,
        PathBuf::from("/tmp"),
        "localhost",
        choice.backend,
        choice.model,
    );
    assert_eq!(
        mgr.get(id).unwrap().details.resolve_model().as_deref(),
        Some("glm-5.3-flash")
    );
}

/// The first row's model pick doesn't leak into the profile row, though both
/// are `BackendType::Claude`.
#[test]
fn test_picker_rows_on_one_backend_keep_their_own_model() {
    let entries = picker_entries(&[BackendType::Claude], [&hydra()]);
    let plain = pick_row(entries.clone(), 0, 1);
    assert_eq!((plain.profile, plain.model), (None, Model::Opus));
    let profiled = pick_row(entries, 1, 1);
    assert_eq!(profiled.model, Model::Custom("glm-5.3-flash".into()));
}

/// Test: index 0 in the picker returns Model::Default (backend picks its own).
#[test]
fn test_picker_default_returns_default() {
    let bt = BackendType::Claude;
    let BackendChoice {
        backend: picked_backend,
        model: picked_model,
        ..
    } = pick(bt, 0);

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
    let BackendChoice {
        backend: picked_backend,
        model: picked_model,
        ..
    } = pick(bt, 1);

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
    let BackendChoice {
        backend: picked_backend,
        model: picked_model,
        ..
    } = pick(bt, 2);

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
    let BackendChoice {
        backend: picked_backend,
        model: picked_model,
        ..
    } = pick(bt, 1);
    let id = create_session_with_cwd(
        &mut mgr,
        &mut dir_picker,
        &mut scene,
        false,
        AiMode::Agentic,
        PathBuf::from("/tmp"),
        "localhost",
        picked_backend,
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
    let BackendChoice {
        backend: picked_backend,
        model: picked_model,
        ..
    } = pick(bt, 0);
    let id = create_session_with_cwd(
        &mut mgr,
        &mut dir_picker,
        &mut scene,
        false,
        AiMode::Agentic,
        PathBuf::from("/tmp"),
        "localhost",
        picked_backend,
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
