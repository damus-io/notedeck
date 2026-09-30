use crate::{ui, NotedeckTextStyle};
use egui::FontData;
use egui::FontDefinitions;
use egui::FontTweak;
use std::collections::BTreeMap;
use std::sync::Arc;
use strum_macros::EnumIter;

/// The font families notedeck registers with egui beyond egui's own
/// `Proportional`/`Monospace`.
///
/// egui names those with a `FontFamily::Name(Arc<str>)`, so building one from
/// [`as_str`](Self::as_str) allocates. Per-frame code should ask
/// [`StyleCache::font_family`](crate::StyleCache::font_family) for a prebuilt
/// one instead of building its own; see that method for why.
#[derive(Copy, Clone, Eq, PartialEq, Debug, EnumIter)]
pub enum NamedFontFamily {
    Medium,
    Bold,
    Emoji,
}

impl NamedFontFamily {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bold => "bold",
            Self::Medium => "medium",
            Self::Emoji => "emoji",
        }
    }
}

pub fn desktop_font_size(text_style: &NotedeckTextStyle) -> f32 {
    match text_style {
        NotedeckTextStyle::Heading => 24.0,
        NotedeckTextStyle::Heading2 => 22.0,
        NotedeckTextStyle::Heading3 => 20.0,
        NotedeckTextStyle::Heading4 => 14.0,
        NotedeckTextStyle::Body => 16.0,
        NotedeckTextStyle::Monospace => 13.0,
        NotedeckTextStyle::Button => 13.0,
        NotedeckTextStyle::Small => 12.0,
        NotedeckTextStyle::Tiny => 10.0,
        NotedeckTextStyle::NoteBody => 16.0,
    }
}

pub fn mobile_font_size(text_style: &NotedeckTextStyle) -> f32 {
    // TODO: tweak text sizes for optimal mobile viewing
    match text_style {
        NotedeckTextStyle::Heading => 24.0,
        NotedeckTextStyle::Heading2 => 22.0,
        NotedeckTextStyle::Heading3 => 20.0,
        NotedeckTextStyle::Heading4 => 14.0,
        NotedeckTextStyle::Body => 13.0,
        NotedeckTextStyle::Monospace => 13.0,
        NotedeckTextStyle::Button => 13.0,
        NotedeckTextStyle::Small => 12.0,
        NotedeckTextStyle::Tiny => 10.0,
        NotedeckTextStyle::NoteBody => 13.0,
    }
}

pub fn get_font_size(ctx: &egui::Context, text_style: &NotedeckTextStyle) -> f32 {
    if ui::is_narrow(ctx) {
        mobile_font_size(text_style)
    } else {
        desktop_font_size(text_style)
    }
}

/// Inconsolata is drawn this much larger than its nominal size, because at the
/// same point size it looks smaller than the proportional face (Onest).
const INCONSOLATA_SCALE: f32 = 1.22;

/// Inconsolata's hhea ascent and descent, in em (859 and -190 of 1000 units).
const INCONSOLATA_ASCENT_EM: f32 = 0.859;
const INCONSOLATA_DESCENT_EM: f32 = -0.190;

/// The vertical tweak that keeps scaled Inconsolata on its own baseline.
///
/// epaint takes a face's row metrics (ascent, row height) from its *unscaled*
/// size, then moves the scaled glyphs by `(1 - scale) / 2 * (ascent + descent)`
/// to "centre" them. With a scale of 1.22 that pushes every monospace glyph
/// about 1px below the baseline that the row metrics promise. This factor is
/// that push with its sign flipped, expressed as epaint wants it (a fraction of
/// the scaled size), so the two cancel. It works out to about -0.060.
///
/// The -0.18 used here before overshot and lifted mono ~2px above its
/// neighbours in a centred row. 0.0 left the push in, so inline code in
/// markdown prose sat 2-3px low. Both were measured, not eyeballed (see the
/// baseline tests in `notedeck_ui`'s `widget_tests`). Inline code also needs its
/// own row height (see `code_fmt` in `notedeck_ui::markdown`), because
/// Inconsolata's row is shorter than Onest's and a layout job bottom-aligns
/// each run in its row.
const INCONSOLATA_Y_OFFSET_FACTOR: f32 =
    -(INCONSOLATA_SCALE - 1.0) * 0.5 * (INCONSOLATA_ASCENT_EM + INCONSOLATA_DESCENT_EM)
        / INCONSOLATA_SCALE;

// Use gossip's approach to font loading. This includes japanese fonts
// for rending stuff from japanese users.
pub fn setup_fonts(ctx: &egui::Context) {
    let mut font_data: BTreeMap<String, Arc<FontData>> = BTreeMap::new();
    let mut families = BTreeMap::new();

    font_data.insert(
        "Onest".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../../assets/fonts/onest/OnestRegular1602-hint.ttf"
        ))),
    );

    font_data.insert(
        "OnestMedium".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../../assets/fonts/onest/OnestMedium1602-hint.ttf"
        ))),
    );

    font_data.insert(
        "DejaVuSans".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../../assets/fonts/DejaVuSansSansEmoji.ttf"
        ))),
    );

    font_data.insert(
        "OnestBold".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../../assets/fonts/onest/OnestBold1602-hint.ttf"
        ))),
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
        Arc::new(
            FontData::from_static(include_bytes!(
                "../../../assets/fonts/Inconsolata-Regular.ttf"
            ))
            .tweak(FontTweak {
                scale: INCONSOLATA_SCALE,
                y_offset_factor: INCONSOLATA_Y_OFFSET_FACTOR,
                y_offset: 0.0,
                baseline_offset_factor: 0.0,
            }),
        ),
    );

    font_data.insert(
        "NotoSansCJK".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../../assets/fonts/NotoSansCJK-Regular.ttc"
        ))),
    );

    font_data.insert(
        "NotoSansThai".to_owned(),
        Arc::new(FontData::from_static(include_bytes!(
            "../../../assets/fonts/NotoSansThai-Regular.ttf"
        ))),
    );

    // Some good looking emojis. Use as first priority:
    font_data.insert(
        "NotoEmoji".to_owned(),
        Arc::new(
            FontData::from_static(include_bytes!(
                "../../../assets/fonts/NotoEmoji-Regular.ttf"
            ))
            .tweak(FontTweak {
                scale: 1.1, // make them a touch larger
                y_offset_factor: 0.0,
                y_offset: 0.0,
                baseline_offset_factor: 0.0,
            }),
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

    tracing::debug!("fonts: {:?}", families);

    let defs = FontDefinitions {
        font_data,
        families,
    };

    ctx.set_fonts(defs);
}
