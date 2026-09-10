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

        /// Are we running as tests?
        const Tests = 1 << 3;

        /// Use the OS secure store (keychain) for secrets instead of the
        /// default plaintext file-based storage. Opt-in via `--use-keystore`.
        const UseKeystore = 1 << 4;

        /// Simulate is_compiled_as_mobile ?
        const Mobile = 1 << 6;

        /// Show the native window titlebar?
        const ShowTitle = 1 << 7;

        /// Update all apps every frame, even if they haven't been opened yet
        const AllAppsActive = 1 << 8;

        /// Run in headless mode: drive every app's per-frame `update()` loop
        /// without a display stack (no eframe window, no winit event loop).
        /// Implies [`AllAppsActive`](Self::AllAppsActive) since a headless run
        /// renders nothing, so every app's background loop should stay active.
        const Headless = 1 << 9;
    }
}

impl Default for NotedeckOptions {
    fn default() -> Self {
        NotedeckOptions::empty()
    }
}
