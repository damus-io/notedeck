use bitflags::bitflags;

bitflags! {
    // Attributes can be applied to flags types
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct NoteOptions: u32 {
        const actionbar     = 0b00000001;
        const note_previews = 0b00000010;
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
