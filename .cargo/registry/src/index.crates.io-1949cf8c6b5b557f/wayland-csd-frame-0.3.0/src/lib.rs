#![deny(rust_2018_idioms)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(improper_ctypes, improper_ctypes_definitions)]
#![deny(clippy::all)]
#![deny(missing_debug_implementations)]
#![deny(missing_docs)]
#![forbid(unsafe_code)]
#![cfg_attr(feature = "cargo-clippy", deny(warnings))]
#![cfg_attr(docsrs, feature(doc_auto_cfg))]

//! The interface for wayland client side decorations (CSD).
//!
//! The crate is intended to be used by libraries providing client
//! side decorations for the xdg-shell protocol.
//!
//! Examples could be found in [`client toolkit`] and [`sctk-adwaita`].
//!
//! [`client toolkit`]: https://github.com/smithay/client-toolkit
//! [`sctk-adwaita`]: https://github.com/PolyMeilex/sctk-adwaita

use std::num::NonZeroU32;
use std::time::Duration;

use bitflags::bitflags;
use wayland_backend::client::ObjectId;

#[doc(inline)]
pub use cursor_icon::{CursorIcon, ParseError as CursorIconParseError};

/// The interface for the client side decorations.
pub trait DecorationsFrame: Sized {
    /// Emulate click on the decorations.
    ///
    /// The `click` is a variant of click to use, see [`FrameClick`] for more
    /// information. `timestamp` is the time when event happened.
    ///
    /// The return value is a [`FrameAction`] you should apply, this action
    /// could be ignored.
    ///
    /// The location of the click is the one passed to
    /// [`Self::click_point_moved`].
    fn on_click(
        &mut self,
        timestamp: Duration,
        click: FrameClick,
        pressed: bool,
    ) -> Option<FrameAction>;

    /// Emulate pointer moved event on the decorations frame.
    ///
    /// The `x` and `y` are location in the surface local coordinates relative
    /// to the `surface`. `timestamp` is the time when event happened.
    ///
    /// The return value is the new cursor icon you should apply to provide
    /// better visual feedback for the user. However, you might want to
    /// ignore it, if you're using touch events to drive the movements.
    fn click_point_moved(
        &mut self,
        timestamp: Duration,
        surface_id: &ObjectId,
        x: f64,
        y: f64,
    ) -> Option<CursorIcon>;

    /// All clicks left the decorations.
    ///
    /// This function should be called when input leaves the decorations.
    fn click_point_left(&mut self);

    /// Update the state of the frame.
    ///
    /// The state is usually obtained from the `xdg_toplevel::configure` event.
    fn update_state(&mut self, state: WindowState);

    /// Update the window manager capabilites.
    ///
    /// The capabilites are usually obtained from the
    /// `xdg_toplevel::wm_capabilities` event.
    fn update_wm_capabilities(&mut self, wm_capabilities: WindowManagerCapabilities);

    /// Resize the window to the new size.
    ///
    /// The size must be without the borders, as in [`Self::subtract_borders]`
    /// were used on it.
    ///
    /// **Note:** The [`Self::update_state`] and
    /// [`Self::update_wm_capabilities`] **must be** applied before calling
    /// this function.
    ///
    /// # Panics
    ///
    /// Panics when resizing the hidden frame.
    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32);

    /// Set the scaling of the decorations frame.
    ///
    /// If the decorations frame is not supporting fractional scaling it'll
    /// `ceil` the scaling factor.
    fn set_scaling_factor(&mut self, scale_factor: f64);

    /// Return the coordinates of the top-left corner of the borders relative to
    /// the content.
    ///
    /// Values **must** thus be non-positive.
    fn location(&self) -> (i32, i32);

    /// Subtract the borders from the given `width` and `height`.
    ///
    /// `None` will be returned for the particular dimension when the given
    /// value for it was too small.
    fn subtract_borders(
        &self,
        width: NonZeroU32,
        height: NonZeroU32,
    ) -> (Option<NonZeroU32>, Option<NonZeroU32>);

    /// Add the borders to the given `width` and `height`.
    ///
    /// Passing zero for both width and height could be used to get the size
    /// of the decorations frame.
    fn add_borders(&self, width: u32, height: u32) -> (u32, u32);

    /// Whether the given frame is dirty and should be redrawn.
    fn is_dirty(&self) -> bool;

    /// Set the frame as hidden.
    ///
    /// The frame **must be** visible by default.
    fn set_hidden(&mut self, hidden: bool);

    /// Get the frame hidden state.
    ///
    /// Get the state of the last [`DecorationsFrame::set_hidden`].
    fn is_hidden(&self) -> bool;

    /// Mark the frame as resizable.
    ///
    /// By default the frame is resizable.
    fn set_resizable(&mut self, resizable: bool);

    /// Draw the decorations frame.
    ///
    /// Return `true` when the main surface must be redrawn as well. This
    /// usually happens when `sync` is being set on the internal subsurfaces and
    /// they've changed their size.
    ///
    /// The user of the frame **must** commit the base surface afterwards.
    fn draw(&mut self) -> bool;

    /// Set the frames title.
    fn set_title(&mut self, title: impl Into<String>);
}

/// The Frame action user should perform in responce to mouse click events.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub enum FrameAction {
    /// The window should be minimized.
    Minimize,
    /// The window should be maximized.
    Maximize,
    /// The window should be unmaximized.
    UnMaximize,
    /// The window should be closed.
    Close,
    /// An interactive move should be started.
    Move,
    /// An interactive resize should be started with the provided edge.
    Resize(ResizeEdge),
    /// Show window menu.
    ///
    /// The coordinates are relative to the base surface, as in should be
    /// directly passed to the `xdg_toplevel::show_window_menu`.
    ShowMenu(i32, i32),
}

/// The user clicked or touched the decoractions frame.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameClick {
    /// The user done normal click, likely with left mouse button or single
    /// finger touch.
    Normal,

    /// The user done right mouse click or some touch sequence that was treated
    /// as alternate click.
    ///
    /// The alternate click exists solely to provide alternative action, like
    /// show window menu when doing right mouse button cilck on the header
    /// decorations, nothing more.
    Alternate,
}

bitflags! {
    /// The configured state of the window.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct WindowState: u16 {
        /// The surface is maximized. The window geometry specified in the
        /// configure event must be obeyed by the client. The client should
        /// draw without shadow or other decoration outside of the window
        /// geometry.
        const MAXIMIZED    = 0b0000_0000_0000_0001;
        /// The surface is fullscreen. The window geometry specified in the
        /// configure event is a maximum; the client cannot resize beyond it.
        /// For a surface to cover the whole fullscreened area, the geometry
        /// dimensions must be obeyed by the client. For more details, see
        /// xdg_toplevel.set_fullscreen.
        const FULLSCREEN   = 0b0000_0000_0000_0010;
        /// The surface is being resized. The window geometry specified in the
        /// configure event is a maximum; the client cannot resize beyond it.
        /// Clients that have aspect ratio or cell sizing configuration can use
        /// a smaller size, however.
        const RESIZING     = 0b0000_0000_0000_0100;
        /// Client window decorations should be painted as if the window is
        /// active. Do not assume this means that the window actually has
        /// keyboard or pointer focus.
        const ACTIVATED    = 0b0000_0000_0000_1000;
        /// The window is currently in a tiled layout and the left edge is
        /// considered to be adjacent to another part of the tiling grid.
        const TILED_LEFT   = 0b0000_0000_0001_0000;
        /// The window is currently in a tiled layout and the right edge is
        /// considered to be adjacent to another part of the tiling grid.
        const TILED_RIGHT  = 0b0000_0000_0010_0000;
        /// The window is currently in a tiled layout and the top edge is
        /// considered to be adjacent to another part of the tiling grid.
        const TILED_TOP    = 0b0000_0000_0100_0000;
        /// The window is currently in a tiled layout and the bottom edge is
        /// considered to be adjacent to another part of the tiling grid.
        const TILED_BOTTOM = 0b0000_0000_1000_0000;
        /// An alias for all tiled bits set.
        const TILED        = Self::TILED_TOP.bits() | Self::TILED_LEFT.bits() | Self::TILED_RIGHT.bits() | Self::TILED_BOTTOM.bits();
        /// The surface is currently not ordinarily being repainted; for example
        /// because its content is occluded by another window, or its outputs are
        /// switched off due to screen locking.
        const SUSPENDED    = 0b0000_0001_0000_0000;
    }
}

bitflags! {
    /// The capabilities of the window manager.
    ///
    /// This is a hint to hide UI elements which provide functionality
    /// not supported by compositor.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct WindowManagerCapabilities : u16 {
        /// `show_window_menu` is available.
        const WINDOW_MENU = 0b0000_0000_0000_0001;
        /// Window can be maximized and unmaximized.
        const MAXIMIZE = 0b0000_0000_0000_0010;
        /// Window can be fullscreened and unfullscreened.
        const FULLSCREEN = 0b0000_0000_0000_0100;
        /// Window could be minimized.
        const MINIMIZE = 0b0000_0000_0000_1000;
    }
}

/// Which edge or corner is being dragged.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ResizeEdge {
    /// Nothing is being dragged.
    None,
    /// The top edge is being dragged.
    Top,
    /// The bottom edge is being dragged.
    Bottom,
    /// The left edge is being dragged.
    Left,
    /// The top left corner is being dragged.
    TopLeft,
    /// The bottom left corner is being dragged.
    BottomLeft,
    /// The right edge is being dragged.
    Right,
    /// The top right corner is being dragged.
    TopRight,
    /// The bottom right corner is being dragged.
    BottomRight,
}
