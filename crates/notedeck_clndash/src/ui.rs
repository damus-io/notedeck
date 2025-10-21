use crate::event::ConnectionState;
use crate::event::LoadingState;
use egui::Color32;
use egui::Label;
use egui::RichText;
use egui::Widget;
use notedeck::AppContext;
use std::collections::HashMap;

pub fn note_hover_ui(
    ui: &mut egui::Ui,
    label: &str,
    ctx: &mut AppContext,
    invoice_notes: &HashMap<String, [u8; 32]>,
) -> Option<notedeck::NoteAction> {
    let zap_req_id = invoice_notes.get(label)?;

    let Ok(txn) = nostrdb::Transaction::new(ctx.ndb) else {
        return None;
    };

    let Ok(zapreq_note) = ctx.ndb.get_note_by_id(&txn, zap_req_id) else {
        return None;
    };

    for tag in zapreq_note.tags() {
        let Some("e") = tag.get_str(0) else {
            continue;
        };

        let Some(target_id) = tag.get_id(1) else {
            continue;
        };

        let Ok(note) = ctx.ndb.get_note_by_id(&txn, target_id) else {
            return None;
        };

        let author = ctx
            .ndb
            .get_profile_by_pubkey(&txn, zapreq_note.pubkey())
            .ok();

        // TODO(jb55): make this less horrible
        let mut note_context = notedeck::NoteContext {
            ndb: ctx.ndb,
            accounts: ctx.accounts,
            img_cache: ctx.img_cache,
            note_cache: ctx.note_cache,
            zaps: ctx.zaps,
            pool: ctx.pool,
            job_pool: ctx.job_pool,
            unknown_ids: ctx.unknown_ids,
            clipboard: ctx.clipboard,
            i18n: ctx.i18n,
            global_wallet: ctx.global_wallet,
            video_store: ctx.video_store,
        };

        let mut jobs = notedeck::JobsCache::default();
        let options = notedeck_ui::NoteOptions::default();

        notedeck_ui::ProfilePic::from_profile_or_default(note_context.img_cache, author.as_ref())
            .ui(ui);

        let nostr_name = notedeck::name::get_display_name(author.as_ref());
        ui.label(format!("{} zapped you", nostr_name.name()));

        return notedeck_ui::NoteView::new(&mut note_context, &note, options, &mut jobs)
            .preview_style()
            .hide_media(true)
            .show(ui)
            .action;
    }

    None
}

pub fn get_info_ui(ui: &mut egui::Ui, info: &LoadingState<String, lnsocket::Error>) {
    ui.horizontal_wrapped(|ui| match info {
        LoadingState::Loading => {}
        LoadingState::Failed(err) => {
            ui.label(format!("failed to fetch node info: {err}"));
        }
        LoadingState::Loaded(info) => {
            ui.add(Label::new(info).wrap_mode(egui::TextWrapMode::Wrap));
        }
    });
}

pub fn connection_state_ui(ui: &mut egui::Ui, state: &ConnectionState) {
    match state {
        ConnectionState::Active => {
            ui.add(Label::new(RichText::new("Connected").color(Color32::GREEN)));
        }

        ConnectionState::Connecting => {
            ui.add(Label::new(
                RichText::new("Connecting").color(Color32::YELLOW),
            ));
        }

        ConnectionState::Dead(reason) => {
            ui.add(Label::new(
                RichText::new(format!("Disconnected: {reason}")).color(Color32::RED),
            ));
        }
    }
}

// ---------- helper ----------
pub fn human_sat(msat: i64) -> String {
    let sats = msat / 1000;
    if sats >= 1_000_000 {
        format!("{:.1}M", sats as f64 / 1_000_000.0)
    } else if sats >= 1_000 {
        format!("{:.1}k", sats as f64 / 1_000.0)
    } else {
        sats.to_string()
    }
}

pub fn human_verbose_sat(msat: i64) -> String {
    if msat < 1_000 {
        // less than 1 sat
        format!("{msat} msat")
    } else {
        let sats = msat / 1_000;
        if sats < 100_000_000 {
            // less than 1 BTC
            format!("{sats} sat")
        } else {
            let btc = sats / 100_000_000;
            format!("{btc} BTC")
        }
    }
}

pub fn delta_str(new: i64, old: i64) -> String {
    let d = new - old;
    match d.cmp(&0) {
        std::cmp::Ordering::Greater => format!("↑ {}", human_sat(d)),
        std::cmp::Ordering::Less => format!("↓ {}", human_sat(-d)),
        std::cmp::Ordering::Equal => "·".into(),
    }
}
