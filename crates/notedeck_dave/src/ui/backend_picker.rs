//! The new-session backend picker: one row per local agentic backend, then one
//! per backend profile layered on Claude ("Claude Code · Hydra"), then one per
//! agent spec ("butler", "Dave Code"), which runs in the native loop on the
//! spec's model.
//!
//! The rows are built once, when the picker opens ([`picker_entries`]), and
//! carried in the overlay, so a frame only reads them.
//!
//! A spec whose coding tools run in a microVM guest ("Sandboxed Coder") is
//! a row on every host, but on one that can't boot guests the row says why
//! in place of its Start button, and its number key does nothing: the
//! session would only fail its first turn with the same reason.

use super::backend_icon;
use crate::backend::{BackendChoice, BackendType, Model};
use agentium_core::agent_spec::AgentSpec;
use agentium_core::config::BackendProfile;
use std::collections::HashMap;

/// Which picker row: a backend, and the profile or spec on it. Keys the
/// overlay's per-row model pick, so plain Claude and Claude under a profile
/// (both [`BackendType::Claude`]) keep their own.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PickerKey {
    /// The row's backend.
    pub backend: BackendType,
    /// The row's backend profile slug, `None` for the plain backend.
    pub profile: Option<String>,
    /// The row's agent spec slug, `None` for a row under no spec.
    pub spec: Option<String>,
}

/// One row of the backend picker, with everything a frame draws prebuilt.
#[derive(Clone, Debug)]
pub struct PickerEntry {
    /// Which row this is.
    pub key: PickerKey,
    /// Row text, keyboard shortcut included: "[2] Claude Code · Hydra".
    label: String,
    /// The models the row's dropdown offers. The first is what the row starts
    /// a session on when nothing else is picked.
    models: Vec<Model>,
    /// What the row's second line says in place of a model dropdown, for a
    /// row whose model isn't the user's to pick: a spec row names its
    /// spec's model ("agent spec · claude-sonnet-5-5").
    detail: Option<String>,
    /// Why this row can't start a session here, drawn in place of its
    /// Start button; `None` for a row that can.
    unavailable: Option<String>,
}

impl PickerEntry {
    /// A row for `backend` on its own models, with `Default` first.
    fn plain(backend: BackendType, idx: usize) -> Self {
        let models = std::iter::once(Model::Default)
            .chain(backend.available_models())
            .collect();
        Self {
            key: PickerKey {
                backend,
                profile: None,
                spec: None,
            },
            label: row_label(idx, backend.display_name()),
            models,
            detail: None,
            unavailable: None,
        }
    }

    /// A row for Claude under `profile`, on the profile's models with its
    /// default first. A profile that names no models offers `Default`, which
    /// leaves the pick to the profile's env (`ANTHROPIC_MODEL`).
    fn profile(profile: &BackendProfile, idx: usize) -> Self {
        let default = profile.default_model.as_deref();
        let mut models: Vec<Model> = default
            .into_iter()
            .chain(
                profile
                    .models
                    .iter()
                    .map(String::as_str)
                    .filter(|m| Some(*m) != default),
            )
            .map(|m| Model::Custom(m.to_string()))
            .collect();
        if models.is_empty() {
            models.push(Model::Default);
        }
        let name = format!(
            "{} \u{00B7} {}",
            BackendType::Claude.display_name(),
            profile.label
        );
        Self {
            key: PickerKey {
                backend: BackendType::Claude,
                profile: Some(profile.slug.clone()),
                spec: None,
            },
            label: row_label(idx, &name),
            models,
            detail: None,
            unavailable: None,
        }
    }

    /// A row for a session under `spec`, labelled by its
    /// [`display_name`](AgentSpec::display_name): its name, else its slug. It runs in the
    /// native loop on the spec's own model, so it offers no model to pick and
    /// names the spec's instead. A spec whose tools run in a guest is
    /// unavailable, for `no_sandbox`'s reason, when that is `Some`.
    fn spec(spec: &AgentSpec, idx: usize, no_sandbox: Option<&str>) -> Self {
        let choice = BackendChoice::under_spec(spec.slug.clone());
        Self {
            key: PickerKey {
                backend: choice.backend,
                profile: None,
                spec: choice.spec,
            },
            label: row_label(idx, spec.display_name()),
            models: vec![choice.model],
            detail: Some(format!("agent spec \u{00B7} {}", spec.model.model)),
            unavailable: no_sandbox
                .filter(|_| spec.containment().is_sandboxed())
                .map(str::to_owned),
        }
    }

    /// Why this row can't start a session here, or `None` when it can.
    pub fn unavailable(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    /// What starting this row on its `model_idx`th model asks for. An index
    /// past the end (a stale pick) starts on the row's first model.
    pub fn choice(&self, model_idx: usize) -> BackendChoice {
        BackendChoice {
            backend: self.key.backend,
            profile: self.key.profile.clone(),
            spec: self.key.spec.clone(),
            model: self.model(model_idx).clone(),
        }
    }

    /// The row's `model_idx`th model, or its first for an index past the end.
    fn model(&self, model_idx: usize) -> &Model {
        self.models.get(model_idx).unwrap_or(&self.models[0])
    }
}

/// "[n] name" for the first nine rows (which have a number key), else "name".
fn row_label(idx: usize, name: &str) -> String {
    if idx < 9 {
        format!("[{}] {name}", idx + 1)
    } else {
        name.to_string()
    }
}

/// The picker's rows: each of `available` plainly, then one per profile when
/// Claude is among them (a profile is an env overlay on the Claude backend),
/// then one per spec in `specs`. A spec row doesn't need `available` to hold
/// a backend: its turns run on the spec's own runner.
///
/// `no_sandbox` is why the host the session would start on can't boot a
/// microVM guest (`None` when it can, or when that isn't known here): each
/// spec row whose tools run in one is drawn unavailable with that reason.
pub fn picker_entries<'a>(
    available: &[BackendType],
    profiles: impl IntoIterator<Item = &'a BackendProfile>,
    specs: impl IntoIterator<Item = &'a AgentSpec>,
    no_sandbox: Option<&str>,
) -> Vec<PickerEntry> {
    let mut entries: Vec<PickerEntry> = available
        .iter()
        .enumerate()
        .map(|(idx, &bt)| PickerEntry::plain(bt, idx))
        .collect();
    if available.contains(&BackendType::Claude) {
        for profile in profiles {
            let idx = entries.len();
            entries.push(PickerEntry::profile(profile, idx));
        }
    }
    for spec in specs {
        let idx = entries.len();
        entries.push(PickerEntry::spec(spec, idx, no_sandbox));
    }
    entries
}

/// Render a single backend row in the picker: icon, name, model dropdown,
/// start button. Returns the row's choice if the user clicked Start on it.
fn backend_picker_row(
    entry: &PickerEntry,
    max_width: f32,
    selected_models: &mut HashMap<PickerKey, usize>,
    ui: &mut egui::Ui,
) -> Option<BackendChoice> {
    let model_idx = selected_models.get(&entry.key).copied().unwrap_or(0);
    let bt = entry.key.backend;

    // Backend row: icon + name + model dropdown + GO button
    let desired = egui::vec2(max_width, 52.0);
    let (rect, _) = ui.allocate_exact_size(desired, egui::Sense::hover());

    // Background
    let fill = ui.visuals().widgets.inactive.weak_bg_fill;
    ui.painter().rect_filled(rect, 8.0, fill);

    // Icon
    let icon_size = 20.0;
    let icon_x = rect.left() + 12.0;
    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(icon_x + icon_size / 2.0, rect.center().y - 6.0),
        egui::vec2(icon_size, icon_size),
    );
    backend_icon(bt).paint_at(ui, icon_rect);

    // Backend name + keyboard shortcut
    ui.painter().text(
        egui::pos2(icon_x + icon_size + 10.0, rect.center().y - 6.0),
        egui::Align2::LEFT_CENTER,
        &entry.label,
        egui::FontId::proportional(15.0),
        ui.visuals().text_color(),
    );

    // Model selector + Start button on the second line
    let controls_rect = egui::Rect::from_min_size(
        egui::pos2(icon_x + icon_size + 10.0, rect.center().y + 4.0),
        egui::vec2(max_width - icon_size - 34.0, 20.0),
    );

    let mut result = None;

    ui.scope_builder(egui::UiBuilder::new().max_rect(controls_rect), |ui| {
        // A row that can't start here says why where its controls go.
        if let Some(why) = &entry.unavailable {
            ui.add(
                egui::Label::new(egui::RichText::new(why).size(11.0).weak())
                    .truncate()
                    .selectable(false),
            )
            .on_hover_text(why);
            return;
        }
        ui.horizontal(|ui| {
            // Model dropdown, when the row offers a choice of models.
            if entry.models.len() > 1 {
                let combo_id = ui.scope_id().with("model").with(&entry.key);
                egui::ComboBox::from_id_salt(combo_id)
                    .selected_text(
                        egui::RichText::new(entry.model(model_idx).display_name()).size(11.0),
                    )
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        for (mi, m) in entry.models.iter().enumerate() {
                            if ui
                                .selectable_label(model_idx == mi, m.display_name())
                                .clicked()
                            {
                                selected_models.insert(entry.key.clone(), mi);
                            }
                        }
                    });
            } else if let Some(detail) = &entry.detail {
                ui.label(egui::RichText::new(detail).size(11.0).weak());
            }

            // Start button
            if ui
                .add(egui::Button::new(egui::RichText::new("Start").size(12.0)).corner_radius(4.0))
                .clicked()
            {
                result = Some(entry.choice(model_idx));
            }
        });
    });

    ui.add_space(6.0);

    result
}

/// Render the backend picker overlay UI over `entries` (see
/// [`picker_entries`]). Returns `Some` when the user has picked a row.
pub fn backend_picker_overlay_ui(
    entries: &[PickerEntry],
    selected_models: &mut HashMap<PickerKey, usize>,
    ui: &mut egui::Ui,
) -> Option<BackendChoice> {
    // Handle keyboard shortcuts: 1-9 for quick selection
    for (idx, entry) in entries.iter().enumerate().take(9) {
        let key = match idx {
            0 => egui::Key::Num1,
            1 => egui::Key::Num2,
            2 => egui::Key::Num3,
            3 => egui::Key::Num4,
            4 => egui::Key::Num5,
            5 => egui::Key::Num6,
            6 => egui::Key::Num7,
            7 => egui::Key::Num8,
            _ => egui::Key::Num9,
        };
        if entry.unavailable.is_none() && ui.input(|i| i.key_pressed(key)) {
            let model_idx = selected_models.get(&entry.key).copied().unwrap_or(0);
            return Some(entry.choice(model_idx));
        }
    }

    let is_narrow = notedeck::ui::is_narrow(ui.ctx());
    let mut selected = None;

    egui::Frame::new()
        .fill(ui.visuals().panel_fill)
        .inner_margin(egui::Margin::symmetric(if is_narrow { 16 } else { 40 }, 20))
        .show(ui, |ui| {
            ui.heading("Select Backend");
            ui.add_space(8.0);
            ui.label("Choose which AI backend to use for this session:");
            ui.add_space(16.0);

            let max_width = if is_narrow {
                ui.available_width()
            } else {
                400.0
            };

            ui.allocate_ui_with_layout(
                egui::vec2(max_width, ui.available_height()),
                egui::Layout::top_down(egui::Align::LEFT),
                |ui| {
                    for entry in entries {
                        if let Some(choice) =
                            backend_picker_row(entry, max_width, selected_models, ui)
                        {
                            selected = Some(choice);
                        }
                    }
                },
            );
        });

    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hydra() -> BackendProfile {
        let mut p = BackendProfile::new("hydra", "Hydra");
        p.models = vec!["qwen3.8-flash-next".into(), "glm-5.3-flash".into()];
        p.default_model = Some("glm-5.3-flash".into());
        p
    }

    /// A lone Claude with one profile is two rows, so the picker shows; the
    /// profile row offers its own models with its default first.
    #[test]
    fn a_profile_is_its_own_row_on_its_own_models() {
        let entries = picker_entries(&[BackendType::Claude], [&hydra()], [], None);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].label, "[1] Claude Code");
        assert_eq!(entries[1].label, "[2] Claude Code \u{00B7} Hydra");
        assert_eq!(
            entries[1].models,
            vec![
                Model::Custom("glm-5.3-flash".into()),
                Model::Custom("qwen3.8-flash-next".into()),
            ]
        );
        assert_eq!(
            entries[1].choice(0),
            BackendChoice {
                backend: BackendType::Claude,
                profile: Some("hydra".into()),
                spec: None,
                model: Model::Custom("glm-5.3-flash".into()),
            }
        );
        // The plain row still starts on the backend's own default.
        assert_eq!(
            entries[0].choice(0),
            BackendChoice::plain(BackendType::Claude)
        );
    }

    /// Profiles ride Claude: with no Claude backend there's no profile row.
    #[test]
    fn no_profile_rows_without_claude() {
        let entries = picker_entries(&[BackendType::Codex], [&hydra()], [], None);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key.profile, None);
    }

    /// A profile with no models offers `Default`, leaving the model to its env.
    #[test]
    fn a_profile_without_models_offers_default() {
        let bare = BackendProfile::new("bare", "Bare");
        let entries = picker_entries(&[BackendType::Claude], [&bare], [], None);
        assert_eq!(entries[1].models, vec![Model::Default]);
    }

    /// Plain Claude and Claude under a profile are both `BackendType::Claude`,
    /// and each keeps its own model pick.
    #[test]
    fn selected_models_tell_two_claude_rows_apart() {
        let entries = picker_entries(&[BackendType::Claude], [&hydra()], [], None);
        let mut selected = HashMap::new();
        selected.insert(entries[0].key.clone(), 2);
        selected.insert(entries[1].key.clone(), 1);
        let pick = |e: &PickerEntry| e.choice(selected.get(&e.key).copied().unwrap_or(0));
        assert_eq!(pick(&entries[0]).model, Model::Sonnet);
        assert_eq!(pick(&entries[0]).profile, None);
        assert_eq!(
            pick(&entries[1]).model,
            Model::Custom("qwen3.8-flash-next".into())
        );
        assert_eq!(pick(&entries[1]).profile.as_deref(), Some("hydra"));
    }

    /// Every built-in spec is a row after the backends and profiles,
    /// labelled by its name (else its slug) and naming its model; picking one starts a
    /// native-loop session under that spec, even on a host whose only
    /// backend is Claude.
    #[test]
    fn a_spec_is_a_row_that_starts_a_session_under_it() {
        use agentium_core::agent_spec::builtin;
        let specs: Vec<AgentSpec> = builtin::SLUGS
            .iter()
            .filter_map(|s| builtin::find(s))
            .collect();
        let entries = picker_entries(&[BackendType::Claude], [&hydra()], &specs, None);
        assert_eq!(entries.len(), 2 + builtin::SLUGS.len());
        let butler = &entries[2];
        assert_eq!(butler.label, "[3] butler");
        let model = &builtin::find("butler").unwrap().model.model;
        assert_eq!(
            butler.detail.as_deref(),
            Some(format!("agent spec \u{00B7} {model}").as_str())
        );
        assert_eq!(butler.choice(0), BackendChoice::under_spec("butler"));
        assert_eq!(butler.choice(0).backend, BackendType::OpenAI);
        // A spec row keys its own model pick apart from the backend rows.
        assert_ne!(butler.key, entries[0].key);
        assert_eq!(entries[3].key.spec.as_deref(), Some("researcher"));
        // A spec with a name is labelled by it, keyed by its slug.
        let dave_code = entries
            .iter()
            .find(|e| e.key.spec.as_deref() == Some("dave-code"))
            .expect("Dave Code has a row");
        assert!(
            dave_code.label.ends_with("] Dave Code"),
            "{}",
            dave_code.label
        );
    }

    /// On a host that can't boot guests, the Sandboxed Coder is still a
    /// row, drawn with the reason and no Start; every other spec row
    /// starts as before. Where guests boot, it is an ordinary row.
    #[test]
    fn a_sandboxed_spec_row_says_why_it_cant_start() {
        use agentium_core::agent_spec::builtin;
        let specs: Vec<AgentSpec> = builtin::SLUGS
            .iter()
            .filter_map(|s| builtin::find(s))
            .collect();
        let why = "microVM sandboxes need KVM Linux or Apple-Silicon macOS";
        let entries = picker_entries(&[BackendType::Claude], [], &specs, Some(why));
        let row = |slug: &str| {
            entries
                .iter()
                .find(|e| e.key.spec.as_deref() == Some(slug))
                .unwrap_or_else(|| panic!("{slug} has a row"))
        };
        let sandboxed = row("sandboxed-coder");
        assert!(sandboxed.label.ends_with("] Sandboxed Coder"));
        assert_eq!(sandboxed.unavailable(), Some(why));
        for slug in ["butler", "researcher", "dave-code"] {
            assert_eq!(row(slug).unavailable(), None, "{slug}");
        }

        let entries = picker_entries(&[BackendType::Claude], [], &specs, None);
        assert!(entries.iter().all(|e| e.unavailable().is_none()));
    }
}
