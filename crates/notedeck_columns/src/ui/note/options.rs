use crate::ui::ProfilePic;
use bitflags::bitflags;

bitflags! {
    // Attributes can be applied to flags types
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct NoteOptions: u64 {
        const actionbar       = 0b0000000000000001;
        const note_previews   = 0b0000000000000010;
        const small_pfp       = 0b0000000000000100;
        const medium_pfp      = 0b0000000000001000;
        const wide            = 0b0000000000010000;
        const selectable_text = 0b0000000000100000;
        const textmode        = 0b0000000001000000;
        const options_button  = 0b0000000010000000;
        const hide_media      = 0b0000000100000000;

        /// Scramble text so that its not distracting during development
        const scramble_text   = 0b0000001000000000;
    }
}

impl Default for NoteOptions {
    fn default() -> NoteOptions {
        NoteOptions::options_button | NoteOptions::note_previews | NoteOptions::actionbar
    }
}

macro_rules! create_bit_methods {
    ($fn_name:ident, $has_name:ident, $option:ident) => {
        #[inline]
        pub fn $fn_name(&mut self, enable: bool) {
            if enable {
                *self |= NoteOptions::$option;
            } else {
                *self &= !NoteOptions::$option;
            }
        }

        #[inline]
        pub fn $has_name(self) -> bool {
            (self & NoteOptions::$option) == NoteOptions::$option
        }
    };
}

impl NoteOptions {
    create_bit_methods!(set_small_pfp, has_small_pfp, small_pfp);
    create_bit_methods!(set_medium_pfp, has_medium_pfp, medium_pfp);
    create_bit_methods!(set_note_previews, has_note_previews, note_previews);
    create_bit_methods!(set_selectable_text, has_selectable_text, selectable_text);
    create_bit_methods!(set_textmode, has_textmode, textmode);
    create_bit_methods!(set_actionbar, has_actionbar, actionbar);
    create_bit_methods!(set_wide, has_wide, wide);
    create_bit_methods!(set_options_button, has_options_button, options_button);
    create_bit_methods!(set_hide_media, has_hide_media, hide_media);
    create_bit_methods!(set_scramble_text, has_scramble_text, scramble_text);

    pub fn new(is_universe_timeline: bool) -> Self {
        let mut options = NoteOptions::default();
        options.set_hide_media(is_universe_timeline);
        options
    }

    pub fn pfp_size(&self) -> i8 {
        if self.has_small_pfp() {
            ProfilePic::small_size()
        } else if self.has_medium_pfp() {
            ProfilePic::medium_size()
        } else {
            ProfilePic::default_size()
        }
    }
}
