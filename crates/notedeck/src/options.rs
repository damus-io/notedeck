use bitflags::bitflags;

bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct NotedeckOptions: u64 {
        // ===== Settings ======
        /// Are we on light theme?
        const LightTheme = 1 << 0;

        /// Debug controls, fps stats
        const Debug = 1 << 1;

        /// Show relay debug window?
        const RelayDebug = 1 << 2;

        /// Are we running as tests?
        const Tests = 1 << 3;

        /// Use keystore?
        const UseKeystore = 1 << 4;

        /// Show client on notes?
        const ShowClient = 1 << 5;

        /// Simulate is_compiled_as_mobile ?
        const Mobile = 1 << 6;

        // ===== Feature Flags ======
        /// Is notebook enabled?
        const FeatureNotebook = 1 << 32;
    }
}

impl Default for NotedeckOptions {
    fn default() -> Self {
        NotedeckOptions::UseKeystore
    }
}
