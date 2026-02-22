use std::collections::HashMap;

pub enum UiCommand {
    Label(String),
    Heading(String),
    Button(String),
    AddSpace(f32),
}

/// Render buffered commands into egui, returning button click events.
/// Keys are `button_key(text, occurrence)`.
pub fn render_commands(commands: &[UiCommand], ui: &mut egui::Ui) -> HashMap<String, bool> {
    let mut events = HashMap::new();
    let mut button_occ: HashMap<&str, u32> = HashMap::new();

    for cmd in commands {
        match cmd {
            UiCommand::Label(text) => {
                ui.label(text.as_str());
            }
            UiCommand::Heading(text) => {
                ui.heading(text.as_str());
            }
            UiCommand::Button(text) => {
                let occ = button_occ.entry(text.as_str()).or_insert(0);
                let key = button_key(text, *occ);
                *occ += 1;
                let clicked = ui.button(text.as_str()).clicked();
                events.insert(key, clicked);
            }
            UiCommand::AddSpace(px) => {
                ui.add_space(*px);
            }
        }
    }

    events
}

pub fn button_key(text: &str, occurrence: u32) -> String {
    if occurrence == 0 {
        text.to_string()
    } else {
        format!("{}#{}", text, occurrence)
    }
}
