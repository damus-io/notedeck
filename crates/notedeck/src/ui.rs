/// Determine if the screen is narrow. This is useful for detecting mobile
/// contexts, but with the nuance that we may also have a wide android tablet.
pub fn is_narrow(ctx: &egui::Context) -> bool {
    let screen_size = ctx.input(|c| c.screen_rect().size());
    screen_size.x < 550.0
}

pub fn is_oled() -> bool {
    is_compiled_as_mobile()
}

#[inline]
#[allow(unreachable_code)]
pub fn is_compiled_as_mobile() -> bool {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        true
    }

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        false
    }
}
