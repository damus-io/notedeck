use egui::{Context, FontFamily, FontId, TextStyle};

use strum_macros::EnumIter;

use crate::fonts::get_font_size;

#[derive(Copy, Clone, Eq, PartialEq, Debug, EnumIter)]
pub enum NotedeckTextStyle {
    Heading,
    Heading2,
    Heading3,
    Heading4,
    Body,
    Monospace,
    Button,
    Small,
    Tiny,
    NoteBody,
}

impl NotedeckTextStyle {
    pub fn text_style(&self) -> TextStyle {
        match self {
            Self::Heading => TextStyle::Heading,
            Self::Heading2 => TextStyle::Name("Heading2".into()),
            Self::Heading3 => TextStyle::Name("Heading3".into()),
            Self::Heading4 => TextStyle::Name("Heading4".into()),
            Self::Body => TextStyle::Body,
            Self::Monospace => TextStyle::Monospace,
            Self::Button => TextStyle::Button,
            Self::Small => TextStyle::Small,
            Self::Tiny => TextStyle::Name("Tiny".into()),
            Self::NoteBody => TextStyle::Name("NoteBody".into()),
        }
    }

    pub fn font_family(&self) -> FontFamily {
        match self {
            Self::Heading => FontFamily::Proportional,
            Self::Heading2 => FontFamily::Proportional,
            Self::Heading3 => FontFamily::Proportional,
            Self::Heading4 => FontFamily::Proportional,
            Self::Body => FontFamily::Proportional,
            Self::Monospace => FontFamily::Monospace,
            Self::Button => FontFamily::Proportional,
            Self::Small => FontFamily::Proportional,
            Self::Tiny => FontFamily::Proportional,
            Self::NoteBody => FontFamily::Proportional,
        }
    }

    pub fn get_font_id(&self, ctx: &Context) -> FontId {
        FontId::new(get_font_size(ctx, self), self.font_family())
    }

    pub fn get_bolded_font(&self, ctx: &Context) -> FontId {
        FontId::new(
            get_font_size(ctx, self),
            egui::FontFamily::Name(crate::NamedFontFamily::Bold.as_str().into()),
        )
    }
}
