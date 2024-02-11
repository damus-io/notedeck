use egui::{FontData, FontDefinitions, FontFamily, FontTweak};
use std::collections::BTreeMap;

pub fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let _families = BTreeMap::<String, FontData>::new();

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

// Use gossip's approach to font loading. This includes japanese fonts
// for rending stuff from japanese users.
pub fn setup_gossip_fonts(ctx: &egui::Context) {
    let mut font_data: BTreeMap<String, FontData> = BTreeMap::new();
    let mut families = BTreeMap::new();

    font_data.insert(
        "DejaVuSans".to_owned(),
        FontData::from_static(include_bytes!("../assets/fonts/DejaVuSansSansEmoji.ttf")),
    );
    font_data.insert(
        "DejaVuSansBold".to_owned(),
        FontData::from_static(include_bytes!(
            "../assets/fonts/DejaVuSans-Bold-SansEmoji.ttf"
        )),
    );

    if cfg!(feature = "lang-cjk") {
        font_data.insert(
            "NotoSansCJK".to_owned(),
            FontData::from_static(include_bytes!("../assets/fonts/NotoSansCJK-Regular.ttc")),
        );
    }

    font_data.insert(
        "Inconsolata".to_owned(),
        FontData::from_static(include_bytes!("../assets/fonts/Inconsolata-Regular.ttf")).tweak(
            FontTweak {
                scale: 1.22,            // This font is smaller than DejaVuSans
                y_offset_factor: -0.18, // and too low
                y_offset: 0.0,
                baseline_offset_factor: 0.0,
            },
        ),
    );

    // Some good looking emojis. Use as first priority:
    font_data.insert(
        "NotoEmoji-Regular".to_owned(),
        FontData::from_static(include_bytes!("../assets/fonts/NotoEmoji-Regular.ttf")).tweak(
            FontTweak {
                scale: 1.1, // make them a touch larger
                y_offset_factor: 0.0,
                y_offset: 0.0,
                baseline_offset_factor: 0.0,
            },
        ),
    );

    let mut proportional = vec!["DejaVuSans".to_owned(), "NotoEmoji-Regular".to_owned()];
    if cfg!(feature = "lang-cjk") {
        proportional.push("NotoSansCJK".to_owned());
    }

    families.insert(FontFamily::Proportional, proportional);

    families.insert(
        FontFamily::Monospace,
        vec!["Inconsolata".to_owned(), "NotoEmoji-Regular".to_owned()],
    );

    families.insert(
        FontFamily::Name("Bold".into()),
        vec!["DejaVuSansBold".to_owned()],
    );

    let defs = FontDefinitions {
        font_data,
        families,
    };

    ctx.set_fonts(defs);
}
