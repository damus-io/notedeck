//! The keymap's key-sequence syntax, vim-flavoured.
//!
//! A sequence is space-separated tokens, one per key press:
//!
//! - A single character is that key: `j`, `?`, `[`, `1`. An uppercase letter
//!   means Shift (`G` is Shift+g).
//! - A named key goes in angle brackets: `<Enter>`, `<Esc>`, `<Space>`, `<Tab>`,
//!   `<Delete>`, `<Up>`, `<F5>`… (anything [`Key::from_name`] knows, plus the
//!   vim spellings `<CR>`, `<BS>` and `<Del>`).
//! - Modifiers prefix the key inside the brackets: `<C-f>` (Ctrl), `<S-Space>`
//!   (Shift), `<A-x>` (Alt), `<D-w>` (Cmd/Super), combinable as `<C-S-x>`.
//!
//! Shifted symbols (`? ! : | + { }`) are written as themselves and match
//! however the platform reports them: egui may deliver `?` as
//! [`Key::Questionmark`] with Shift held, or as Shift+[`Key::Slash`] on a US
//! layout. Both [`KeyStroke::from_press`] and the parser fold these into one
//! canonical stroke (see [`canonical`]), so a binding written `?` matches
//! either.

use std::fmt;

use egui::Key;
use notedeck_ui::chord::KeyPress;

/// The longest key sequence a binding may have. Fixed so a [`KeySeq`] is
/// `Copy` and a pending chord never allocates.
pub const MAX_SEQ: usize = 4;

/// The modifiers a [`KeyStroke`] carries, as bits. egui's derived
/// `command` flag is not one of them: it aliases Ctrl or Cmd per platform, and
/// a keymap names the physical modifier.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Mods(u8);

impl Mods {
    /// No modifiers.
    pub const NONE: Mods = Mods(0);
    /// Ctrl (`C-`).
    pub const CTRL: Mods = Mods(1);
    /// Shift (`S-`).
    pub const SHIFT: Mods = Mods(2);
    /// Alt/Option (`A-`).
    pub const ALT: Mods = Mods(4);
    /// Cmd on macOS, Super elsewhere (`D-`).
    pub const CMD: Mods = Mods(8);

    /// Whether every bit of `other` is set.
    pub fn contains(self, other: Mods) -> bool {
        self.0 & other.0 == other.0
    }

    /// `self` without the bits of `other`.
    fn without(self, other: Mods) -> Mods {
        Mods(self.0 & !other.0)
    }

    /// The modifiers held with an egui key press.
    fn from_egui(m: egui::Modifiers) -> Mods {
        let mut mods = Mods::NONE;
        if m.ctrl {
            mods = mods | Mods::CTRL;
        }
        if m.shift {
            mods = mods | Mods::SHIFT;
        }
        if m.alt {
            mods = mods | Mods::ALT;
        }
        if m.mac_cmd {
            mods = mods | Mods::CMD;
        }
        mods
    }
}

impl std::ops::BitOr for Mods {
    type Output = Mods;

    fn bitor(self, rhs: Mods) -> Mods {
        Mods(self.0 | rhs.0)
    }
}

/// One key press as the keymap sees it: a key and its modifiers, in canonical
/// form (see [`canonical`]), so a binding and a press compare with `==`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyStroke {
    /// The key.
    pub key: Key,
    /// The modifiers held with it.
    pub mods: Mods,
}

impl KeyStroke {
    /// A canonical stroke of `key` with `mods`.
    pub fn new(key: Key, mods: Mods) -> Self {
        canonical(KeyStroke { key, mods })
    }

    /// The stroke an egui key press is, for matching against bindings.
    pub fn from_press(press: KeyPress) -> Self {
        Self::new(press.key, Mods::from_egui(press.modifiers))
    }

    /// Parse one token of the key syntax (see the module docs).
    pub fn from_token(token: &str) -> Result<Self, KeyParseError> {
        if token.is_empty() {
            return Err(KeyParseError::Empty);
        }
        let Some(inner) = token.strip_prefix('<') else {
            return single_char(token, Mods::NONE);
        };
        let Some(inner) = inner.strip_suffix('>') else {
            return Err(KeyParseError::Unclosed(token.to_owned()));
        };

        // Peel `X-` modifier prefixes while something is left after them, so
        // `<C-->` is Ctrl+Minus rather than a dangling modifier.
        let mut mods = Mods::NONE;
        let mut rest = inner;
        while let Some((prefix, tail)) = rest.split_once('-') {
            if tail.is_empty() {
                break;
            }
            mods = mods
                | match prefix {
                    "C" | "c" => Mods::CTRL,
                    "S" | "s" => Mods::SHIFT,
                    "A" | "a" | "M" | "m" => Mods::ALT,
                    "D" | "d" => Mods::CMD,
                    _ => {
                        return Err(KeyParseError::UnknownModifier {
                            token: token.to_owned(),
                            modifier: prefix.to_owned(),
                        })
                    }
                };
            rest = tail;
        }

        if rest.chars().count() == 1 {
            return single_char(rest, mods);
        }
        let key = match rest {
            "CR" | "Return" => Some(Key::Enter),
            "BS" => Some(Key::Backspace),
            "Del" => Some(Key::Delete),
            name => Key::from_name(name),
        };
        key.map(|key| KeyStroke::new(key, mods))
            .ok_or_else(|| KeyParseError::UnknownKey(token.to_owned()))
    }
}

/// A one-character token, with `mods` from any `<..>` around it. An uppercase
/// letter adds Shift.
fn single_char(token: &str, mods: Mods) -> Result<KeyStroke, KeyParseError> {
    let mut chars = token.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return Err(KeyParseError::UnknownKey(token.to_owned()));
    };
    let Some(key) = Key::from_name(token) else {
        return Err(KeyParseError::UnknownKey(token.to_owned()));
    };
    let mods = if c.is_ascii_uppercase() {
        mods | Mods::SHIFT
    } else {
        mods
    };
    Ok(KeyStroke::new(key, mods))
}

/// Fold the two ways a shifted symbol arrives into one: Shift+`/` becomes `?`
/// (and likewise `! : | + { }` on a US layout), and a key that is itself a
/// shifted glyph drops the Shift egui reports with it. Every [`KeyStroke`] is
/// built through this, so equality is all matching needs.
fn canonical(stroke: KeyStroke) -> KeyStroke {
    let shifted = stroke.mods.contains(Mods::SHIFT);
    let key = match (stroke.key, shifted) {
        (Key::Slash, true) => Key::Questionmark,
        (Key::Num1, true) => Key::Exclamationmark,
        (Key::Semicolon, true) => Key::Colon,
        (Key::Backslash, true) => Key::Pipe,
        (Key::Equals, true) => Key::Plus,
        (Key::OpenBracket, true) => Key::OpenCurlyBracket,
        (Key::CloseBracket, true) => Key::CloseCurlyBracket,
        (key, _) => key,
    };
    let mods = if is_shifted_glyph(key) {
        stroke.mods.without(Mods::SHIFT)
    } else {
        stroke.mods
    };
    KeyStroke { key, mods }
}

/// Keys whose glyph already implies Shift on common layouts.
fn is_shifted_glyph(key: Key) -> bool {
    matches!(
        key,
        Key::Questionmark
            | Key::Exclamationmark
            | Key::Colon
            | Key::Pipe
            | Key::Plus
            | Key::OpenCurlyBracket
            | Key::CloseCurlyBracket
    )
}

impl fmt::Display for KeyStroke {
    /// The stroke back in the keymap's own syntax, e.g. `G`, `?`, `<C-f>`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let glyph = key_glyph(self.key);
        let is_letter = glyph.is_some_and(|c| c.is_ascii_alphabetic());
        // A shifted letter prints as its uppercase form, so Shift only needs
        // spelling out on keys that aren't letters.
        let (glyph, mods) = match glyph {
            Some(c) if is_letter && self.mods.contains(Mods::SHIFT) => {
                (Some(c.to_ascii_uppercase()), self.mods.without(Mods::SHIFT))
            }
            _ => (glyph, self.mods),
        };
        if mods == Mods::NONE {
            if let Some(c) = glyph {
                return write!(f, "{c}");
            }
        }

        f.write_str("<")?;
        for (bit, prefix) in [
            (Mods::CTRL, "C-"),
            (Mods::SHIFT, "S-"),
            (Mods::ALT, "A-"),
            (Mods::CMD, "D-"),
        ] {
            if mods.contains(bit) {
                f.write_str(prefix)?;
            }
        }
        match glyph {
            Some(c) => write!(f, "{c}")?,
            None if self.key == Key::Escape => f.write_str("Esc")?,
            None => f.write_str(self.key.name())?,
        }
        f.write_str(">")
    }
}

/// The one ASCII character a key is written as, lowercase for letters, or
/// `None` for a key written by name (`<Enter>`, `<Up>`).
fn key_glyph(key: Key) -> Option<char> {
    if key == Key::Minus {
        // egui's symbol for it is the Unicode minus sign.
        return Some('-');
    }
    let mut chars = key.symbol_or_name().chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_graphic() => Some(c.to_ascii_lowercase()),
        _ => None,
    }
}

/// A key sequence of up to [`MAX_SEQ`] strokes, stored inline so it is `Copy`.
/// Bindings key on it, and the engine's pending chord prefix is one too.
#[derive(Clone, Copy, Debug)]
pub struct KeySeq {
    /// The strokes; only the first `len` are meaningful.
    strokes: [KeyStroke; MAX_SEQ],
    /// How many of `strokes` are in the sequence.
    len: u8,
}

/// The filler for unused [`KeySeq`] slots; never compared.
const NO_STROKE: KeyStroke = KeyStroke {
    key: Key::Escape,
    mods: Mods::NONE,
};

impl Default for KeySeq {
    fn default() -> Self {
        Self {
            strokes: [NO_STROKE; MAX_SEQ],
            len: 0,
        }
    }
}

impl KeySeq {
    /// Parse a whole sequence: space-separated tokens (see the module docs).
    pub fn parse(text: &str) -> Result<Self, KeyParseError> {
        let mut seq = KeySeq::default();
        for token in text.split_whitespace() {
            let stroke = KeyStroke::from_token(token)?;
            seq = seq.pushed(stroke).ok_or_else(|| KeyParseError::TooLong {
                keys: text.to_owned(),
            })?;
        }
        if seq.is_empty() {
            return Err(KeyParseError::Empty);
        }
        Ok(seq)
    }

    /// The strokes, in order.
    pub fn as_slice(&self) -> &[KeyStroke] {
        &self.strokes[..self.len as usize]
    }

    /// Whether the sequence has no strokes.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// This sequence with `stroke` appended, or `None` when it is already
    /// [`MAX_SEQ`] long.
    pub fn pushed(mut self, stroke: KeyStroke) -> Option<Self> {
        let slot = self.strokes.get_mut(self.len as usize)?;
        *slot = stroke;
        self.len += 1;
        Some(self)
    }

    /// A one-stroke sequence.
    pub fn single(stroke: KeyStroke) -> Self {
        let mut seq = Self::default();
        seq.strokes[0] = stroke;
        seq.len = 1;
        seq
    }
}

impl PartialEq for KeySeq {
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl Eq for KeySeq {}

impl fmt::Display for KeySeq {
    /// Space-separated tokens, the form [`KeySeq::parse`] reads.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, stroke) in self.as_slice().iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{stroke}")?;
        }
        Ok(())
    }
}

/// Why a key sequence didn't parse.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyParseError {
    /// No keys at all.
    Empty,
    /// More than [`MAX_SEQ`] keys.
    TooLong {
        /// The whole sequence as written.
        keys: String,
    },
    /// A `<` with no closing `>`.
    Unclosed(String),
    /// A modifier prefix other than `C- S- A- M- D-`.
    UnknownModifier {
        /// The whole token.
        token: String,
        /// The prefix that isn't a modifier.
        modifier: String,
    },
    /// A token that names no key.
    UnknownKey(String),
}

impl fmt::Display for KeyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "no keys given"),
            Self::TooLong { keys } => {
                write!(f, "'{keys}' is longer than {MAX_SEQ} keys")
            }
            Self::Unclosed(token) => write!(f, "'{token}' is missing its closing '>'"),
            Self::UnknownModifier { token, modifier } => write!(
                f,
                "'{modifier}-' in '{token}' is not a modifier (use C-, S-, A- or D-)"
            ),
            Self::UnknownKey(token) if token.chars().count() > 1 && !token.starts_with('<') => {
                write!(
                    f,
                    "'{token}' is not a key; separate keys with spaces and \
                     wrap named keys in <>, e.g. <Enter>"
                )
            }
            Self::UnknownKey(token) => write!(f, "'{token}' is not a key egui knows"),
        }
    }
}

impl std::error::Error for KeyParseError {}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Modifiers;

    fn stroke(token: &str) -> KeyStroke {
        KeyStroke::from_token(token).unwrap_or_else(|e| panic!("{token}: {e}"))
    }

    fn press(modifiers: Modifiers, key: Key) -> KeyStroke {
        KeyStroke::from_press(KeyPress { key, modifiers })
    }

    #[test]
    fn a_lowercase_letter_is_the_bare_key() {
        assert_eq!(stroke("j"), KeyStroke::new(Key::J, Mods::NONE));
        assert_eq!(stroke("j"), press(Modifiers::NONE, Key::J));
    }

    #[test]
    fn an_uppercase_letter_is_shift() {
        assert_eq!(stroke("G"), KeyStroke::new(Key::G, Mods::SHIFT));
        assert_eq!(stroke("G"), press(Modifiers::SHIFT, Key::G));
        assert_ne!(stroke("g"), press(Modifiers::SHIFT, Key::G));
    }

    #[test]
    fn digits_and_unshifted_symbols_are_their_keys() {
        assert_eq!(stroke("1").key, Key::Num1);
        assert_eq!(stroke("[").key, Key::OpenBracket);
        assert_eq!(stroke("]").key, Key::CloseBracket);
        assert_eq!(stroke("/").key, Key::Slash);
        assert_eq!(stroke("-").key, Key::Minus);
        assert_eq!(stroke("/").mods, Mods::NONE);
    }

    #[test]
    fn a_questionmark_matches_however_egui_reports_it() {
        let q = stroke("?");
        assert_eq!(q, KeyStroke::new(Key::Questionmark, Mods::NONE));
        assert_eq!(q, press(Modifiers::NONE, Key::Questionmark));
        assert_eq!(q, press(Modifiers::SHIFT, Key::Questionmark));
        assert_eq!(q, press(Modifiers::SHIFT, Key::Slash));
        assert_ne!(q, press(Modifiers::NONE, Key::Slash));
    }

    #[test]
    fn other_shifted_symbols_fold_the_same_way() {
        for (token, base) in [
            ("!", Key::Num1),
            (":", Key::Semicolon),
            ("|", Key::Backslash),
            ("+", Key::Equals),
            ("{", Key::OpenBracket),
            ("}", Key::CloseBracket),
        ] {
            assert_eq!(stroke(token), press(Modifiers::SHIFT, base), "{token}");
            assert_eq!(stroke(token).mods, Mods::NONE, "{token}");
        }
    }

    #[test]
    fn named_keys_go_in_brackets() {
        assert_eq!(stroke("<Enter>").key, Key::Enter);
        assert_eq!(stroke("<CR>").key, Key::Enter);
        assert_eq!(stroke("<Esc>").key, Key::Escape);
        assert_eq!(stroke("<Escape>").key, Key::Escape);
        assert_eq!(stroke("<Space>").key, Key::Space);
        assert_eq!(stroke("<Tab>").key, Key::Tab);
        assert_eq!(stroke("<Delete>").key, Key::Delete);
        assert_eq!(stroke("<Del>").key, Key::Delete);
        assert_eq!(stroke("<BS>").key, Key::Backspace);
        assert_eq!(stroke("<Up>").key, Key::ArrowUp);
        assert_eq!(stroke("<PageDown>").key, Key::PageDown);
        assert_eq!(stroke("<F5>").key, Key::F5);
        assert_eq!(stroke("<j>"), stroke("j"));
    }

    #[test]
    fn modifiers_prefix_the_key() {
        assert_eq!(stroke("<C-f>"), KeyStroke::new(Key::F, Mods::CTRL));
        assert_eq!(stroke("<C-f>"), press(Modifiers::CTRL, Key::F));
        assert_eq!(stroke("<S-Space>"), KeyStroke::new(Key::Space, Mods::SHIFT));
        assert_eq!(stroke("<A-x>"), press(Modifiers::ALT, Key::X));
        assert_eq!(stroke("<M-x>"), stroke("<A-x>"));
        assert_eq!(stroke("<D-w>"), press(Modifiers::MAC_CMD, Key::W));
        assert_eq!(
            stroke("<C-S-x>"),
            KeyStroke::new(Key::X, Mods::CTRL | Mods::SHIFT)
        );
        assert_eq!(stroke("<C-X>"), stroke("<C-S-x>"));
        assert_eq!(stroke("<C-->"), KeyStroke::new(Key::Minus, Mods::CTRL));
    }

    #[test]
    fn ctrl_on_linux_matches_a_ctrl_binding_not_a_bare_one() {
        // egui sets `command` alongside `ctrl` off macOS; it must not matter.
        let ctrl_f = press(Modifiers::CTRL | Modifiers::COMMAND, Key::F);
        assert_eq!(ctrl_f, stroke("<C-f>"));
        assert_ne!(ctrl_f, stroke("f"));
        assert_ne!(press(Modifiers::NONE, Key::F), stroke("<C-f>"));
    }

    #[test]
    fn bad_tokens_say_what_is_wrong() {
        assert_eq!(KeyStroke::from_token(""), Err(KeyParseError::Empty));
        assert_eq!(
            KeyStroke::from_token("<Enter"),
            Err(KeyParseError::Unclosed("<Enter".into()))
        );
        assert_eq!(
            KeyStroke::from_token("<Q-x>"),
            Err(KeyParseError::UnknownModifier {
                token: "<Q-x>".into(),
                modifier: "Q".into(),
            })
        );
        assert_eq!(
            KeyStroke::from_token("<Ent>"),
            Err(KeyParseError::UnknownKey("<Ent>".into()))
        );
        let err = KeyStroke::from_token("gg").unwrap_err();
        assert_eq!(err, KeyParseError::UnknownKey("gg".into()));
        assert!(
            err.to_string().contains("separate keys with spaces"),
            "{err}"
        );
        assert!(KeyStroke::from_token("é").is_err());
    }

    #[test]
    fn a_sequence_is_space_separated_tokens() {
        let seq = KeySeq::parse("g g").expect("parse");
        assert_eq!(seq.as_slice(), &[stroke("g"), stroke("g")]);
        let seq = KeySeq::parse("  ]  q ").expect("parse");
        assert_eq!(seq.as_slice(), &[stroke("]"), stroke("q")]);
        assert_eq!(KeySeq::parse("<C-f>").expect("parse").as_slice().len(), 1);
    }

    #[test]
    fn a_sequence_has_one_to_four_keys() {
        assert_eq!(KeySeq::parse(""), Err(KeyParseError::Empty));
        assert_eq!(KeySeq::parse("   "), Err(KeyParseError::Empty));
        assert!(KeySeq::parse("a b c d").is_ok());
        assert_eq!(
            KeySeq::parse("a b c d e"),
            Err(KeyParseError::TooLong {
                keys: "a b c d e".into()
            })
        );
    }

    #[test]
    fn a_sequence_prints_back_in_its_own_syntax() {
        for text in [
            "g g",
            "G",
            "?",
            "] q",
            "-",
            "<C-f>",
            "<S-Space>",
            "<Enter>",
            "<Up>",
            "<C-X>",
        ] {
            let seq = KeySeq::parse(text).expect("parse");
            assert_eq!(seq.to_string(), text);
            assert_eq!(KeySeq::parse(&seq.to_string()), Ok(seq));
        }
        assert_eq!(
            KeySeq::parse("<C-S-x>").expect("parse").to_string(),
            "<C-X>"
        );
        assert_eq!(KeySeq::parse("<Esc>").expect("parse").to_string(), "<Esc>");
    }

    #[test]
    fn equality_ignores_unused_slots() {
        let a = KeySeq::single(stroke("j"));
        let b = KeySeq::parse("j").expect("parse");
        assert_eq!(a, b);
        assert_ne!(a, a.pushed(stroke("j")).expect("room"));
    }
}
