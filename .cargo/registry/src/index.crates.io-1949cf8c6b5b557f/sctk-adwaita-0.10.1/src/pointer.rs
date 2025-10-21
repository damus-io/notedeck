use std::time::Duration;

use smithay_client_toolkit::reexports::csd_frame::{
    CursorIcon, FrameAction, ResizeEdge, WindowManagerCapabilities, WindowState,
};

use crate::{
    buttons::ButtonKind,
    theme::{BORDER_SIZE, HEADER_SIZE},
};

/// Time to register the next click as a double click.
///
/// The value is the same as the default in gtk4.
const DOUBLE_CLICK_DURATION: Duration = Duration::from_millis(400);

/// The state of the mouse input inside the decorations frame.
#[derive(Debug, Default)]
pub(crate) struct MouseState {
    pub location: Location,

    /// The surface local location inside the surface.
    position: (f64, f64),

    /// The instant of the last click.
    last_normal_click: Option<Duration>,
}

impl MouseState {
    /// The normal click on decorations frame was made.
    pub fn click(
        &mut self,
        timestamp: Duration,
        pressed: bool,
        resizable: bool,
        state: &WindowState,
        wm_capabilities: &WindowManagerCapabilities,
    ) -> Option<FrameAction> {
        let maximized = state.contains(WindowState::MAXIMIZED);
        let action = match self.location {
            Location::Top if resizable => FrameAction::Resize(ResizeEdge::Top),
            Location::TopLeft if resizable => FrameAction::Resize(ResizeEdge::TopLeft),
            Location::Left if resizable => FrameAction::Resize(ResizeEdge::Left),
            Location::BottomLeft if resizable => FrameAction::Resize(ResizeEdge::BottomLeft),
            Location::Bottom if resizable => FrameAction::Resize(ResizeEdge::Bottom),
            Location::BottomRight if resizable => FrameAction::Resize(ResizeEdge::BottomRight),
            Location::Right if resizable => FrameAction::Resize(ResizeEdge::Right),
            Location::TopRight if resizable => FrameAction::Resize(ResizeEdge::TopRight),
            Location::Button(ButtonKind::Close) if !pressed => FrameAction::Close,
            Location::Button(ButtonKind::Maximize) if !pressed && !maximized => {
                FrameAction::Maximize
            }
            Location::Button(ButtonKind::Maximize) if !pressed && maximized => {
                FrameAction::UnMaximize
            }
            Location::Button(ButtonKind::Minimize) if !pressed => FrameAction::Minimize,
            Location::Head
                if pressed && wm_capabilities.contains(WindowManagerCapabilities::MAXIMIZE) =>
            {
                match self.last_normal_click.replace(timestamp) {
                    Some(last) if timestamp.saturating_sub(last) < DOUBLE_CLICK_DURATION => {
                        if maximized {
                            FrameAction::UnMaximize
                        } else {
                            FrameAction::Maximize
                        }
                    }
                    _ => FrameAction::Move,
                }
            }
            Location::Head if pressed => FrameAction::Move,
            _ => return None,
        };

        Some(action)
    }

    /// Alternative click on decorations frame was made.
    pub fn alternate_click(
        &mut self,
        pressed: bool,
        wm_capabilities: &WindowManagerCapabilities,
    ) -> Option<FrameAction> {
        // Invalidate the normal click.
        self.last_normal_click = None;

        match self.location {
            Location::Head | Location::Button(_)
                if pressed && wm_capabilities.contains(WindowManagerCapabilities::WINDOW_MENU) =>
            {
                Some(FrameAction::ShowMenu(
                    // XXX this could be one 1pt off when the frame is not maximized, but it's not
                    // like it really matters in the end.
                    self.position.0 as i32 - BORDER_SIZE as i32,
                    // We must offset it by header size for precise position.
                    self.position.1 as i32 - HEADER_SIZE as i32,
                ))
            }
            _ => None,
        }
    }

    /// The mouse moved inside the decorations frame.
    pub fn moved(&mut self, location: Location, x: f64, y: f64, resizable: bool) -> CursorIcon {
        self.location = location;
        self.position = (x, y);
        match self.location {
            _ if !resizable => CursorIcon::Default,
            Location::Top => CursorIcon::NResize,
            Location::TopRight => CursorIcon::NeResize,
            Location::Right => CursorIcon::EResize,
            Location::BottomRight => CursorIcon::SeResize,
            Location::Bottom => CursorIcon::SResize,
            Location::BottomLeft => CursorIcon::SwResize,
            Location::Left => CursorIcon::WResize,
            Location::TopLeft => CursorIcon::NwResize,
            _ => CursorIcon::Default,
        }
    }

    /// The mouse left the decorations frame.
    pub fn left(&mut self) {
        // Reset only the location.
        self.location = Location::None;
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub enum Location {
    #[default]
    None,
    Head,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
    TopLeft,
    Button(ButtonKind),
}
