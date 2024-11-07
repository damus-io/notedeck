use crate::decks::Deck;

/// State for UI creating/editing deck
#[derive(Default)]
pub struct DeckState {
    pub deck_name: String,
    pub selected_glyph: Option<char>,
    pub deleting: bool,
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
            deleting: false,
        }
    }

    pub fn clear(&mut self) {
        *self = Default::default();
    }
}
