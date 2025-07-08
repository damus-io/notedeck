use eframe::icon_data::from_png_bytes;
use egui::{include_image, Color32, IconData, Image};

pub fn app_icon() -> IconData {
    from_png_bytes(include_bytes!("../../../assets/damus-app-icon.png")).expect("icon")
}

pub fn add_account_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/add_account_icon_4x.png"
    ))
}

pub fn add_column_dark_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/add_column_dark_4x.png"
    ))
}

pub fn add_column_light_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/add_column_light_4x.png"
    ))
}

pub fn add_relay_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/add_relay_icon_4x.png"
    ))
}

pub fn algo_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/algo.png"))
}

pub fn columns_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/columns_80.png"))
}

pub fn connected_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/connected_icon_4x.png"
    ))
}

pub fn connecting_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/connecting_icon_4x.png"
    ))
}

pub fn damus_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/damus_rounded_80.png"))
}

pub fn delete_dark_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/column_delete_icon_4x.png"
    ))
}

pub fn delete_light_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/column_delete_icon_light_4x.png"
    ))
}

pub fn disconnected_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/disconnected_icon_4x.png"
    ))
}

pub fn edit_dark_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/edit_icon_4x_dark.png"
    ))
}

pub fn eye_dark_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/eye-dark.png"))
}

pub fn eye_light_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/eye-light.png"))
}

pub fn eye_slash_dark_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/eye-slash-dark.png"))
}

pub fn eye_slash_light_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/eye-slash-light.png"))
}

pub fn filled_zap_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/filled_zap_icon.svg"))
}

pub fn hashtag_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/hashtag_icon_4x.png"))
}

pub fn help_dark_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/help_icon_dark_4x.png"
    ))
}

pub fn help_light_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/help_icon_inverted_4x.png"
    ))
}

pub fn home_dark_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/home-toolbar.png"))
}

pub fn home_light_image() -> Image<'static> {
    home_dark_image().tint(Color32::BLACK)
}

pub fn home_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/home_icon_dark_4x.png"
    ))
}

pub fn key_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/key_4x.png"))
}

pub fn link_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/links_4x.png"))
}

pub fn new_message_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/newmessage_64.png"))
}

pub fn new_deck_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/new_deck_icon_4x_dark.png"
    ))
}

pub fn notifications_image(dark_mode: bool) -> Image<'static> {
    if dark_mode {
        crate::app_images::notifications_dark_image()
    } else {
        crate::app_images::notifications_light_image()
    }
}

pub fn notifications_dark_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/notifications_dark_4x.png"
    ))
}

pub fn notifications_light_image() -> Image<'static> {
    notifications_dark_image().tint(Color32::BLACK)
}
pub fn repost_dark_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/repost.svg"))
}

pub fn repost_light_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/repost_light.png"))
}

pub fn reply_dark_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/reply.svg"))
}

pub fn reply_light_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/reply.svg")).tint(Color32::BLACK)
}

pub fn profile_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/profile_icon_4x.png"))
}

pub fn settings_dark_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/settings_dark_4x.png"))
}

pub fn settings_light_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/settings_light_4x.png"
    ))
}

pub fn universe_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/universe_icon_dark_4x.png"
    ))
}

pub fn verified_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/verified_4x.png"))
}

pub fn media_upload_dark_image() -> Image<'static> {
    Image::new(include_image!(
        "../../../assets/icons/media_upload_dark_4x.png"
    ))
}

pub fn wallet_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/wallet-icon.svg"))
}

pub fn zap_image() -> Image<'static> {
    Image::new(include_image!("../../../assets/icons/zap.svg"))
}
