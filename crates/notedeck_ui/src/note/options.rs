use crate::ProfilePic;
use bitflags::bitflags;

bitflags! {
    // Attributes can be applied to flags types
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct NoteOptions: u64 {
        const ActionBar       = 1 << 0;
        const NotePreviews    = 1 << 1;
        const SmallPfp        = 1 << 2;
        const MediumPfp       = 1 << 3;
        const Wide            = 1 << 4;
        const SelectableText  = 1 << 5;
        const Textmode        = 1 << 6;
        const OptionsButton   = 1 << 7;
        const HideMedia       = 1 << 8;
        /// Scramble text so that its not distracting during development
        const ScrambleText    = 1 << 9;
        /// Is this a note preview?
        const Preview         = 1 << 10;
        /// Is the content truncated? If the length is over a certain size it
        /// will end with a ... and a "Show more" button.
        const Truncate        = 1 << 11;
    }
}

impl Default for NoteOptions {
    fn default() -> NoteOptions {
        NoteOptions::OptionsButton
            | NoteOptions::NotePreviews
            | NoteOptions::ActionBar
            | NoteOptions::Truncate
    }
}

impl NoteOptions {
    pub fn has(self, flag: NoteOptions) -> bool {
        (self & flag) == flag
    }

    pub fn new(is_universe_timeline: bool) -> Self {
        let mut options = NoteOptions::default();
        options.set(NoteOptions::HideMedia, is_universe_timeline);
        options
    }

    pub fn pfp_size(&self) -> i8 {
        if self.has(NoteOptions::SmallPfp) {
            ProfilePic::small_size()
        } else if self.has(NoteOptions::MediumPfp) {
            ProfilePic::medium_size()
        } else {
            ProfilePic::default_size()
        }
    }
}
