use crate::{app_style::emoji_font_family, decks::Deck};

/// State for UI creating/editing deck
pub struct DeckState {
    pub deck_name: String,
    pub selected_glyph: Option<char>,
    pub selecting_glyph: bool,
    pub warn_no_title: bool,
    pub warn_no_icon: bool,
    glyph_options: Option<Vec<char>>,
}

impl DeckState {
    pub fn load(&mut self, deck: &Deck) {
        self.deck_name = deck.name.clone();
        self.selected_glyph = Some(deck.icon);
    }

    pub fn from_deck(deck: &Deck) -> Self {
        let deck_name = deck.name.clone();
        let selected_glyph = Some(deck.icon);
        Self {
            deck_name,
            selected_glyph,
            ..Default::default()
        }
    }

    pub fn clear(&mut self) {
        *self = Default::default();
    }

    pub fn get_glyph_options(&mut self, ui: &egui::Ui) -> &Vec<char> {
        self.glyph_options
            .get_or_insert_with(|| available_characters(ui, emoji_font_family()))
    }
}

impl Default for DeckState {
    fn default() -> Self {
        Self {
            deck_name: Default::default(),
            selected_glyph: Default::default(),
            selecting_glyph: true,
            warn_no_icon: Default::default(),
            warn_no_title: Default::default(),
            glyph_options: Default::default(),
        }
    }
}

fn available_characters(ui: &egui::Ui, family: egui::FontFamily) -> Vec<char> {
    ui.fonts(|f| {
        f.lock()
            .fonts
            .font(&egui::FontId::new(10.0, family)) // size is arbitrary for getting the characters
            .characters()
            .iter()
            .map(|(chr, _v)| chr)
            .filter(|chr| !chr.is_whitespace() && !chr.is_ascii_control())
            .copied()
            .collect()
    })
}
