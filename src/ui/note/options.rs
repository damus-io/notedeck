use crate::ui::ProfilePic;
use bitflags::bitflags;

bitflags! {
    // Attributes can be applied to flags types
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct NoteOptions: u32 {
        const actionbar     = 0b00000001;
        const note_previews = 0b00000010;
        const small_pfp     = 0b00000100;
        const medium_pfp    = 0b00001000;
        const wide          = 0b00010000;
    }
}

impl NoteOptions {
    #[inline]
    pub fn has_actionbar(self) -> bool {
        (self & NoteOptions::actionbar) == NoteOptions::actionbar
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

    #[inline]
    pub fn set_small_pfp(&mut self, enable: bool) {
        if enable {
            *self |= NoteOptions::small_pfp;
        } else {
            *self &= !NoteOptions::small_pfp;
        }
    }

    #[inline]
    pub fn set_medium_pfp(&mut self, enable: bool) {
        if enable {
            *self |= NoteOptions::medium_pfp;
        } else {
            *self &= !NoteOptions::medium_pfp;
        }
    }

    #[inline]
    pub fn set_note_previews(&mut self, enable: bool) {
        if enable {
            *self |= NoteOptions::note_previews;
        } else {
            *self &= !NoteOptions::note_previews;
        }
    }

    #[inline]
    pub fn set_actionbar(&mut self, enable: bool) {
        if enable {
            *self |= NoteOptions::actionbar;
        } else {
            *self &= !NoteOptions::actionbar;
        }
    }
}
