use egui::{FontData, FontDefinitions, FontFamily};

pub fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    let our_font: String = "onest".to_owned();

    // Install my own font (maybe supporting non-latin characters):
    fonts.font_data.insert(
        our_font.clone(),
        FontData::from_static(include_bytes!(
            "../assets/fonts/onest/OnestRegular1602-hint.ttf"
        )),
    ); // .ttf and .otf supported

    // Put my font first (highest priority):
    fonts
        .families
        .get_mut(&FontFamily::Proportional)
        .unwrap()
        .insert(0, our_font);

    // Put my font as last fallback for monospace:
    //fonts.families.get_mut(&FontFamily::Monospace).unwrap()
    //.push("onest".to_owned());

    ctx.set_fonts(fonts);
}
