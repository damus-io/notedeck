use crate::NotedeckTextStyle;

pub const NARROW_SCREEN_WIDTH: f32 = 550.0;
/// Determine if the screen is narrow. This is useful for detecting mobile
/// contexts, but with the nuance that we may also have a wide android tablet.

pub fn richtext_small<S>(text: S) -> egui::RichText
where
    S: Into<String>,
{
    egui::RichText::new(text).text_style(NotedeckTextStyle::Small.text_style())
}

pub fn is_narrow(ctx: &egui::Context) -> bool {
    let screen_size = ctx.input(|c| c.screen_rect().size());
    screen_size.x < NARROW_SCREEN_WIDTH
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
