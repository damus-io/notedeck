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
    }
}

impl Default for NoteOptions {
    fn default() -> NoteOptions {
        NoteOptions::options_button | NoteOptions::note_previews | NoteOptions::actionbar
    }
}

macro_rules! create_setter {
    ($fn_name:ident, $option:ident) => {
        #[inline]
        pub fn $fn_name(&mut self, enable: bool) {
            if enable {
                *self |= NoteOptions::$option;
            } else {
                *self &= !NoteOptions::$option;
            }
        }
    };
}

impl NoteOptions {
    create_setter!(set_small_pfp, small_pfp);
    create_setter!(set_medium_pfp, medium_pfp);
    create_setter!(set_note_previews, note_previews);
    create_setter!(set_selectable_text, selectable_text);
    create_setter!(set_textmode, textmode);
    create_setter!(set_actionbar, actionbar);
    create_setter!(set_wide, wide);
    create_setter!(set_options_button, options_button);
    create_setter!(set_hide_media, hide_media);

    pub fn new(is_universe_timeline: bool) -> Self {
        let mut options = NoteOptions::default();
        options.set_hide_media(is_universe_timeline);
        options
    }

    #[inline]
    pub fn has_actionbar(self) -> bool {
        (self & NoteOptions::actionbar) == NoteOptions::actionbar
    }

    #[inline]
    pub fn has_hide_media(self) -> bool {
        (self & NoteOptions::hide_media) == NoteOptions::hide_media
    }

    #[inline]
    pub fn has_selectable_text(self) -> bool {
        (self & NoteOptions::selectable_text) == NoteOptions::selectable_text
    }

    #[inline]
    pub fn has_textmode(self) -> bool {
        (self & NoteOptions::textmode) == NoteOptions::textmode
    }

    #[inline]
    pub fn has_note_previews(self) -> bool {
        (self & NoteOptions::note_previews) == NoteOptions::note_previews
    }

    #[inline]
    pub fn has_small_pfp(self) -> bool {
        (self & NoteOptions::small_pfp) == NoteOptions::small_pfp
    }

    #[inline]
    pub fn has_medium_pfp(self) -> bool {
        (self & NoteOptions::medium_pfp) == NoteOptions::medium_pfp
    }

    #[inline]
    pub fn has_wide(self) -> bool {
        (self & NoteOptions::wide) == NoteOptions::wide
    }

    #[inline]
    pub fn has_options_button(self) -> bool {
        (self & NoteOptions::options_button) == NoteOptions::options_button
    }

    pub fn pfp_size(&self) -> f32 {
        if self.has_small_pfp() {
            ProfilePic::small_size()
        } else if self.has_medium_pfp() {
            ProfilePic::medium_size()
        } else {
            ProfilePic::default_size()
        }
    }
}
