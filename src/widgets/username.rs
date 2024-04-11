use crate::fonts::NamedFontFamily;
use egui::{Color32, Label, RichText, Widget};
use nostrdb::ProfileRecord;

pub struct Username<'a> {
    profile: Option<&'a ProfileRecord<'a>>,
    pk: &'a [u8; 32],
    pk_colored: bool,
    abbrev: usize,
}

impl<'a> Username<'a> {
    pub fn pk_colored(mut self, pk_colored: bool) -> Self {
        self.pk_colored = pk_colored;
        self
    }

    pub fn abbreviated(mut self, amount: usize) -> Self {
        self.abbrev = amount;
        self
    }

    pub fn new(profile: Option<&'a ProfileRecord>, pk: &'a [u8; 32]) -> Self {
        let pk_colored = false;
        let abbrev: usize = 1000;
        Username {
            profile,
            pk,
            pk_colored,
            abbrev,
        }
    }
}

impl<'a> Widget for Username<'a> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        ui.horizontal(|ui| {
            //ui.spacing_mut().item_spacing.x = 0.0;
            if let Some(profile) = self.profile {
                if let Some(prof) = profile.record.profile() {
                    let color = if self.pk_colored {
                        Some(pk_color(self.pk))
                    } else {
                        None
                    };

                    if prof.display_name().is_some() && prof.display_name().unwrap() != "" {
                        ui_abbreviate_name(ui, prof.display_name().unwrap(), self.abbrev, color);
                    } else if let Some(name) = prof.name() {
                        ui_abbreviate_name(ui, name, self.abbrev, color);
                    }
                }
            } else {
                ui.strong("nostrich");
            }
        })
        .response
    }
}

fn ui_abbreviate_name(ui: &mut egui::Ui, name: &str, len: usize, color: Option<Color32>) {
    if name.len() > len {
        let closest = crate::abbrev::floor_char_boundary(name, len);
        ui.strong(&name[..closest]);
        ui.strong("...");
    } else {
        let mut txt = RichText::new(name).family(NamedFontFamily::Medium.as_family());
        if let Some(c) = color {
            txt = txt.color(c);
        }
        ui.add(Label::new(txt));
    }
}

fn pk_color(pk: &[u8; 32]) -> Color32 {
    Color32::from_rgb(pk[8], pk[10], pk[12])
}
