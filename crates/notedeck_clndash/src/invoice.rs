use crate::event::LoadingState;
use crate::ui;
use lightning_invoice::Bolt11Invoice;
use notedeck::AppContext;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Deserialize, Serialize)]
pub struct Invoice {
    pub lastpay_index: Option<u64>,
    pub label: String,
    pub bolt11: Bolt11Invoice,
    pub payment_hash: String,
    pub amount_msat: u64,
    pub status: String,
    pub description: String,
    pub expires_at: u64,
    pub created_index: u64,
    pub updated_index: u64,
}

pub fn invoices_ui(
    ui: &mut egui::Ui,
    invoice_notes: &HashMap<String, [u8; 32]>,
    ctx: &mut AppContext,
    invoices: &LoadingState<Vec<Invoice>, lnsocket::Error>,
) {
    match invoices {
        LoadingState::Loading => {
            ui.label("loading invoices...");
        }

        LoadingState::Failed(err) => {
            ui.label(format!("failed to load invoices: {err}"));
        }

        LoadingState::Loaded(invoices) => {
            use egui_extras::{Column, TableBuilder};

            TableBuilder::new(ui)
                .column(Column::auto().resizable(true))
                .column(Column::remainder())
                .vscroll(false)
                .header(20.0, |mut header| {
                    header.col(|ui| {
                        ui.strong("description");
                    });
                    header.col(|ui| {
                        ui.strong("amount");
                    });
                })
                .body(|mut body| {
                    for invoice in invoices {
                        body.row(20.0, |mut row| {
                            row.col(|ui| {
                                if invoice.description.starts_with("{") {
                                    ui.label("Zap!").on_hover_ui_at_pointer(|ui| {
                                        ui::note_hover_ui(ui, &invoice.label, ctx, invoice_notes);
                                    });
                                } else {
                                    ui.label(&invoice.description);
                                }
                            });
                            row.col(|ui| match invoice.bolt11.amount_milli_satoshis() {
                                None => {
                                    ui.label("any");
                                }
                                Some(amt) => {
                                    ui.label(ui::human_verbose_sat(amt as i64));
                                }
                            });
                        });
                    }
                });
        }
    }
}
