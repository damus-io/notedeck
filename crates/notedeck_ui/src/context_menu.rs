/// Context menu helpers (paste, etc)
use egui_winit::clipboard::Clipboard;

#[derive(Copy, Clone, Eq, PartialEq)]
pub enum PasteBehavior {
    Clear,
    Append,
}

fn handle_paste(clipboard: &mut Clipboard, input: &mut String, paste_behavior: PasteBehavior) {
    if let Some(text) = clipboard.get() {
        // if called with clearing_input_context, then we clear before
        // we paste. Useful for certain fields like passwords, etc
        match paste_behavior {
            PasteBehavior::Clear => input.clear(),
            PasteBehavior::Append => {}
        }
        input.push_str(&text);
    }
}

pub fn input_context(
    ui: &mut egui::Ui,
    response: &egui::Response,
    clipboard: &mut Clipboard,
    input: &mut String,
    paste_behavior: PasteBehavior,
) {
    response.context_menu(|ui| {
        if ui.button("Paste").clicked() {
            handle_paste(clipboard, input, paste_behavior);
            ui.close_menu();
        }

        if ui.button("Copy").clicked() {
            clipboard.set_text(input.to_owned());
            ui.close_menu();
        }

        if ui.button("Cut").clicked() {
            clipboard.set_text(input.to_owned());
            input.clear();
            ui.close_menu();
        }
    });

    if response.middle_clicked() {
        handle_paste(clipboard, input, paste_behavior)
    }

    // for keyboard visibility
    crate::include_input(ui, response)
}
