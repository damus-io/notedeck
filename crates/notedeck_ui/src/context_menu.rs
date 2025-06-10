/// Context menu helpers (paste, etc)
use egui_winit::clipboard::Clipboard;

fn handle_paste(clipboard: &mut Clipboard, input: &mut String) {
    if let Some(text) = clipboard.get() {
        input.clear();
        input.push_str(&text);
    }
}

pub fn input_context(response: &egui::Response, clipboard: &mut Clipboard, input: &mut String) {
    response.context_menu(|ui| {
        if ui.button("Paste").clicked() {
            handle_paste(clipboard, input)
        }
    });

    if response.middle_clicked() {
        handle_paste(clipboard, input)
    }
}
