/// UI components for displaying query call information.
///
/// These components render the parameters of a nostr query in a visual format,
/// using pill labels to show search terms, authors, limits, and other filter criteria.
use super::pill::{pill_label, pill_label_ui};
use crate::tools::QueryCall;
use nostrdb::{Ndb, Transaction};
use notedeck::{Images, MediaJobSender};
use notedeck_ui::ProfilePic;

/// Render query call parameters as pill labels
pub fn query_call_ui(
    cache: &mut Images,
    ndb: &Ndb,
    query: &QueryCall,
    jobs: &MediaJobSender,
    ui: &mut egui::Ui,
) {
    ui.spacing_mut().item_spacing.x = 8.0;
    if let Some(pubkey) = query.author() {
        let txn = Transaction::new(ndb).unwrap();
        pill_label_ui(
            "author",
            move |ui| {
                ui.add(
                    &mut ProfilePic::from_profile_or_default(
                        cache,
                        jobs,
                        ndb.get_profile_by_pubkey(&txn, pubkey.bytes())
                            .ok()
                            .as_ref(),
                    )
                    .size(ProfilePic::small_size() as f32),
                );
            },
            ui,
        );
    }

    if let Some(limit) = query.limit {
        pill_label("limit", &limit.to_string(), ui);
    }

    if let Some(since) = query.since {
        pill_label("since", &since.to_string(), ui);
    }

    if let Some(kind) = query.kind {
        pill_label("kind", &kind.to_string(), ui);
    }

    if let Some(until) = query.until {
        pill_label("until", &until.to_string(), ui);
    }

    if let Some(search) = query.search.as_ref() {
        pill_label("search", search, ui);
    }
}
