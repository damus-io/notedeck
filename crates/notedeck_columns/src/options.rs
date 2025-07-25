use bitflags::bitflags;

bitflags! {
    // Attributes can be applied to flags types
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct AppOptions: u64 {
        /// Explicitly enable/disable column persistence for the sessions
        const TmpColumns    = 1 << 0;

        /// Debug mode for debug ui controls
        const Debug         = 1 << 1;

        /// Should we explicitly disable since optimization?
        const SinceOptimize = 1 << 2;

        /// Should we scroll to top on the active column?
        const ScrollToTop   = 1 << 3;

        /// Are we showing fullscreen media?
        const FullscreenMedia  = 1 << 4;
    }
}

impl Default for AppOptions {
    fn default() -> AppOptions {
        AppOptions::SinceOptimize
    }
}
