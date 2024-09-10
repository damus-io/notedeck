use crate::ui::ProfilePic;
use bitflags::bitflags;

bitflags! {
    // Attributes can be applied to flags types
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct NoteOptions: u32 {
        const actionbar       = 0b00000001;
        const note_previews   = 0b00000010;
        const small_pfp       = 0b00000100;
        const medium_pfp      = 0b00001000;
        const wide            = 0b00010000;
        const selectable_text = 0b00100000;
        const textmode        = 0b01000000;
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

    #[inline]
    pub fn has_actionbar(self) -> bool {
        (self & NoteOptions::actionbar) == NoteOptions::actionbar
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

    pub fn pfp_size(&self) -> f32 {
        if self.has_small_pfp() {
            ProfilePic::small_size()
        } else if self.has_medium_pfp() {
            ProfilePic::medium_size()
        } else {
            ProfilePic::default_size()
        }
    }

    #[inline]
    pub fn has_wide(self) -> bool {
        (self & NoteOptions::wide) == NoteOptions::wide
    }

    #[inline]
    pub fn set_wide(&mut self, enable: bool) {
        if enable {
            *self |= NoteOptions::wide;
        } else {
            *self &= !NoteOptions::wide;
        }
    }
}
