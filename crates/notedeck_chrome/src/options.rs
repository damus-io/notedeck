use bitflags::bitflags;

bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ChromeOptions: u64 {
        /// Is the chrome currently open?
        const NoOptions = 0;

        /// Is the chrome currently open?
        const IsOpen = 1 << 0;

        /// Are we simulating a virtual keyboard? This is mostly for debugging
        /// if we are too lazy to open up a real mobile device with soft
        /// keyboard
        const VirtualKeyboard = 1 << 1;

        /// Are we showing the memory debug window?
        const MemoryDebug = 1 << 2;

        /// Repaint debug
        const RepaintDebug = 1 << 3;

        /// We need soft keyboard visibility
        const KeyboardVisibility = 1 << 4;
    }
}

impl Default for ChromeOptions {
    fn default() -> Self {
        let mut options = ChromeOptions::NoOptions;
        options.set(
            ChromeOptions::IsOpen,
            !notedeck::ui::is_compiled_as_mobile(),
        );
        options
    }
}
