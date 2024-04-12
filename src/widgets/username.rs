use crate::fonts::NamedFontFamily;
use egui::{Color32, RichText, Widget};
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
            ui.spacing_mut().item_spacing.x = 0.0;

            let color = if self.pk_colored {
                Some(pk_color(self.pk))
            } else {
                None
            };

            if let Some(profile) = self.profile {
                if let Some(prof) = profile.record.profile() {
                    if prof.display_name().is_some() && prof.display_name().unwrap() != "" {
                        ui_abbreviate_name(ui, prof.display_name().unwrap(), self.abbrev, color);
                    } else if let Some(name) = prof.name() {
                        ui_abbreviate_name(ui, name, self.abbrev, color);
                    }
                }
            } else {
                let mut txt = RichText::new("nostrich").family(NamedFontFamily::Medium.as_family());
                if let Some(col) = color {
                    txt = txt.color(col)
                }
                ui.label(txt);
            }
        })
        .response
    }
}

fn colored_name(name: &str, color: Option<Color32>) -> RichText {
    let mut txt = RichText::new(name).family(NamedFontFamily::Medium.as_family());

    if let Some(color) = color {
        txt = txt.color(color);
    }

    txt
}

fn ui_abbreviate_name(ui: &mut egui::Ui, name: &str, len: usize, color: Option<Color32>) {
    let should_abbrev = name.len() > len;
    let name = if should_abbrev {
        let closest = crate::abbrev::floor_char_boundary(name, len);
        &name[..closest]
    } else {
        name
    };

    ui.label(colored_name(name, color));

    if should_abbrev {
        ui.label(colored_name("...", color));
    }
}

fn pk_color(pk: &[u8; 32]) -> Color32 {
    Color32::from_rgb(pk[8], pk[10], pk[12])
}
