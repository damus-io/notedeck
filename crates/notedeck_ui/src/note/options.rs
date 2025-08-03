use crate::ProfilePic;
use bitflags::bitflags;

bitflags! {
    // Attributes can be applied to flags types
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct NoteOptions: u64 {
        const ActionBar       = 1 << 0;
        const HasNotePreviews = 1 << 1;
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
        const IsPreview       = 1 << 10;
        /// Is the content truncated? If the length is over a certain size it
        /// will end with a ... and a "Show more" button.
        const Truncate        = 1 << 11;
        /// Show note's client in the note content
        const ClientName  = 1 << 12;

        const RepliesNewestFirst = 1 << 13;

        /// Show note's full created at date at the bottom
        const FullCreatedDate  = 1 << 14;

        /// Note has a framed border
        const Framed = 1 << 15;

        /// Note has a framed border
        const UnreadIndicator = 1 << 16;
    }
}

impl Default for NoteOptions {
    fn default() -> NoteOptions {
        NoteOptions::OptionsButton
            | NoteOptions::HasNotePreviews
            | NoteOptions::ActionBar
            | NoteOptions::Truncate
    }
}

impl NoteOptions {
    pub fn new(is_universe_timeline: bool) -> Self {
        let mut options = NoteOptions::default();
        options.set(NoteOptions::HideMedia, is_universe_timeline);
        options
    }

    pub fn pfp_size(&self) -> i8 {
        if self.contains(NoteOptions::SmallPfp) {
            ProfilePic::small_size()
        } else if self.contains(NoteOptions::MediumPfp) {
            ProfilePic::medium_size()
        } else {
            ProfilePic::default_size()
        }
    }
}
