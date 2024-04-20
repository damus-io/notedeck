use nostrdb::ProfileRecord;

pub struct ProfilePreview<'a> {
    profile: &'a ProfileRecord<'a>,
}

impl<'a> ProfilePreview<'a> {
    pub fn new(profile: &'a ProfileRecord<'a>) -> Self {
        ProfilePreview { profile }
    }
}

impl<'a> egui::Widget for ProfilePreview<'a> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        ui.horizontal(|ui| {
            ui.label("Profile");
            let name = if let Some(name) = crate::profile::get_profile_name(self.profile) {
                name
            } else {
                "nostrich"
            };
            ui.label(name);
        })
        .response
    }
}

mod previews {
    use super::*;
    use crate::test_data::test_profile_record;
    use crate::ui::{Preview, View};
    use egui::Widget;

    pub struct ProfilePreviewPreview<'a> {
        profile: ProfileRecord<'a>,
    }

    impl<'a> ProfilePreviewPreview<'a> {
        pub fn new() -> Self {
            let profile = test_profile_record();
            ProfilePreviewPreview { profile }
        }
    }

    impl<'a> Default for ProfilePreviewPreview<'a> {
        fn default() -> Self {
            ProfilePreviewPreview::new()
        }
    }

    impl<'a> View for ProfilePreviewPreview<'a> {
        fn ui(&mut self, ui: &mut egui::Ui) {
            ProfilePreview::new(&self.profile).ui(ui);
        }
    }

    impl<'a> Preview for ProfilePreview<'a> {
        /// A preview of the profile preview :D
        type Prev = ProfilePreviewPreview<'a>;

        fn preview() -> Self::Prev {
            ProfilePreviewPreview::new()
        }
    }
}
