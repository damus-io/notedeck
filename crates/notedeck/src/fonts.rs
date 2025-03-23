use crate::{ui, NotedeckTextStyle};

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
    }
}

pub fn get_font_size(ctx: &egui::Context, text_style: &NotedeckTextStyle) -> f32 {
    if ui::is_narrow(ctx) {
        mobile_font_size(text_style)
    } else {
        desktop_font_size(text_style)
    }
}
