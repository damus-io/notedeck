use egui::{FontData, FontDefinitions, FontTweak};
use std::collections::BTreeMap;
use tracing::debug;

pub enum NamedFontFamily {
    Medium,
    Bold,
    Emoji,
}

impl NamedFontFamily {
    pub fn as_str(&mut self) -> &'static str {
        match self {
            Self::Bold => "bold",
            Self::Medium => "medium",
            Self::Emoji => "emoji",
        }
    }

    pub fn as_family(&mut self) -> egui::FontFamily {
        egui::FontFamily::Name(self.as_str().into())
    }
}

// Use gossip's approach to font loading. This includes japanese fonts
// for rending stuff from japanese users.
pub fn setup_fonts(ctx: &egui::Context) {
    let mut font_data: BTreeMap<String, FontData> = BTreeMap::new();
    let mut families = BTreeMap::new();

    font_data.insert(
        "Onest".to_owned(),
        FontData::from_static(include_bytes!(
            "../assets/fonts/onest/OnestRegular1602-hint.ttf"
        )),
    );

    font_data.insert(
        "OnestMedium".to_owned(),
        FontData::from_static(include_bytes!(
            "../assets/fonts/onest/OnestMedium1602-hint.ttf"
        )),
    );

    font_data.insert(
        "DejaVuSans".to_owned(),
        FontData::from_static(include_bytes!("../assets/fonts/DejaVuSansSansEmoji.ttf")),
    );

    font_data.insert(
        "OnestBold".to_owned(),
        FontData::from_static(include_bytes!(
            "../assets/fonts/onest/OnestBold1602-hint.ttf"
        )),
    );

    /*
    font_data.insert(
        "DejaVuSansBold".to_owned(),
        FontData::from_static(include_bytes!(
            "../assets/fonts/DejaVuSans-Bold-SansEmoji.ttf"
        )),
    );

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
    */

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

    font_data.insert(
        "NotoSansCJK".to_owned(),
        FontData::from_static(include_bytes!("../assets/fonts/NotoSansCJK-Regular.ttc")),
    );

    font_data.insert(
        "NotoSansThai".to_owned(),
        FontData::from_static(include_bytes!("../assets/fonts/NotoSansThai-Regular.ttf")),
    );

    // Some good looking emojis. Use as first priority:
    font_data.insert(
        "NotoEmoji".to_owned(),
        FontData::from_static(include_bytes!("../assets/fonts/NotoEmoji-Regular.ttf")).tweak(
            FontTweak {
                scale: 1.1, // make them a touch larger
                y_offset_factor: 0.0,
                y_offset: 0.0,
                baseline_offset_factor: 0.0,
            },
        ),
    );

    let base_fonts = vec![
        "DejaVuSans".to_owned(),
        "NotoEmoji".to_owned(),
        "NotoSansCJK".to_owned(),
        "NotoSansThai".to_owned(),
    ];

    let mut proportional = vec!["Onest".to_owned()];
    proportional.extend(base_fonts.clone());

    let mut medium = vec!["OnestMedium".to_owned()];
    medium.extend(base_fonts.clone());

    let mut mono = vec!["Inconsolata".to_owned()];
    mono.extend(base_fonts.clone());

    let mut bold = vec!["OnestBold".to_owned()];
    bold.extend(base_fonts.clone());

    let emoji = vec!["NotoEmoji".to_owned()];

    families.insert(egui::FontFamily::Proportional, proportional);
    families.insert(egui::FontFamily::Monospace, mono);
    families.insert(
        egui::FontFamily::Name(NamedFontFamily::Medium.as_str().into()),
        medium,
    );
    families.insert(
        egui::FontFamily::Name(NamedFontFamily::Bold.as_str().into()),
        bold,
    );
    families.insert(
        egui::FontFamily::Name(NamedFontFamily::Emoji.as_str().into()),
        emoji,
    );

    debug!("fonts: {:?}", families);

    let defs = FontDefinitions {
        font_data,
        families,
    };

    ctx.set_fonts(defs);
}
