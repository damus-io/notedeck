/// Default frame margin
pub const FRAME_MARGIN: i8 = 8;

/// Spacing in pixels needed at the top of sidebars/panels on macOS to avoid
/// overlapping with the window traffic lights (close/minimize/maximize buttons).
/// The traffic lights are positioned ~12px from the top and are ~12px tall,
/// so we need at least 24px clearance, with some margin for comfort.
pub const MACOS_TRAFFIC_LIGHT_SPACING: f32 = 28.0;

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimum spacing required to clear macOS traffic lights.
    /// Traffic lights are ~12px from top + ~12px tall = ~24px minimum.
    const MIN_MACOS_TRAFFIC_LIGHT_CLEARANCE: f32 = 24.0;

    #[test]
    fn macos_traffic_light_spacing_is_sufficient() {
        assert!(
            MACOS_TRAFFIC_LIGHT_SPACING >= MIN_MACOS_TRAFFIC_LIGHT_CLEARANCE,
            "macOS traffic light spacing ({MACOS_TRAFFIC_LIGHT_SPACING}px) must be at least \
             {MIN_MACOS_TRAFFIC_LIGHT_CLEARANCE}px to avoid overlapping with window controls"
        );
    }
}
