use reqwest::Url;

use crate::{storage::FileDirectoryInteractor, FileWriterFactory};

use super::padding;

#[derive(Default)]
pub struct SupportView {}

impl SupportView {
    pub fn show(&mut self, ui: &mut egui::Ui) {
        padding(8.0, ui, |ui| {
            if let Ok(interactor) = FileWriterFactory::new(crate::FileWriterType::Log).build() {
                show_internal(interactor, ui);
            }
        });
    }
}

fn show_internal(interactor: FileDirectoryInteractor, ui: &mut egui::Ui) {
    let mut contents = None;
    if let Ok(Some(most_recent_name)) = interactor.get_most_recent() {
        if let Ok(file_contents) = interactor.get_file(most_recent_name) {
            contents = Some(file_contents);
        }
    }

    // let mailto_url = SupportMailtoBuilder::new("support@damus.io".to_owned())
    //     .with_subject("Help Needed".to_owned())
    //     .with_content(contents)
    //     .build();
    ui.horizontal(|ui| {
        ui.label("Run into a bug? ".to_owned());
        ui.hyperlink_to("click here", "mailto:support@damus.io?subject=Need%20Help&body=body%20here.");
        ui.label("  to open your default email client and send an email to Damus Support with your logs attached.".to_owned());
    });

    ui.add_space(16.0);
    ui.label("alternatively, you can manually email support@damus.io with your logs attached which can be found in:".to_owned());
    ui.label(format!("{:?}", interactor.get_directory()));
}

struct SupportMailtoBuilder {
    content: Option<String>,
    address: String,
    subject: Option<String>,
}

impl SupportMailtoBuilder {
    fn new(address: String) -> Self {
        Self {
            content: None,
            address,
            subject: None,
        }
    }

    // will be truncated so the whole URL is at most 2000 characters
    pub fn with_content(mut self, content: Option<String>) -> Self {
        self.content = content;
        self
    }

    pub fn with_subject(mut self, subject: String) -> Self {
        self.subject = Some(subject);
        self
    }

    pub fn build(self) -> String {
        let mut url =
            Url::parse(&format!("mailto:{}", self.address)).expect("URL parse did not work");

        url.query_pairs_mut()
            .append_pair("subject", self.subject.as_deref().unwrap_or(""));
        url.query_pairs_mut()
            .append_pair("body", self.content.as_deref().unwrap_or(""));

        url.to_string()
    }
}
