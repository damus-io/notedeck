//! The Permissions card: the session's permission mode, how its prompts were
//! answered, and its always-allow rules, each with a Revoke button.
//!
//! The rules are the session's runtime allowlist
//! (`AgenticSessionData::runtime_allows`), which lives in memory on the
//! device that granted them: a remote viewer's rules are the ones it granted
//! itself, which answer the host's prompts from here, and the host's own are
//! never sent. Revoking one works the same either way.

use crate::inspector::{PermissionCounts, PermissionLabels, PermissionMemo};
use crate::messages::PermissionDecision;
use crate::session::RuntimeAllow;
use crate::ui::dave::permission_mode_badge;
use crate::ui::DaveAction;
use claude_agent_sdk_rs::PermissionMode;
use notedeck::{tr, tr_plural, Localization};
use std::collections::{BTreeSet, HashMap};
use uuid::Uuid;

use super::{heading_ui, key_ui, value_ui, weak_ui, MONO_SIZE};

/// The session's permission state the card reads, borrowed from its
/// `AgenticSessionData`. `None` on a chat-mode session, which has none.
#[derive(Clone, Copy)]
pub struct PermissionFacts<'a> {
    /// The "Allow Always" grants, in the order the card lists them.
    pub rules: &'a BTreeSet<RuntimeAllow>,
    /// Decisions reached before their chat row caught up.
    pub responded: &'a HashMap<Uuid, PermissionDecision>,
}

/// The Permissions card's body, and the rule a Revoke click asked to drop.
pub fn permissions_ui(
    memo: &mut PermissionMemo,
    mode: PermissionMode,
    facts: Option<PermissionFacts<'_>>,
    chat: &[crate::Message],
    is_remote: bool,
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) -> Option<DaveAction> {
    let counts = PermissionCounts::of(chat, facts.map(|f| f.responded));
    let labels = memo.get(counts, i18n, build_labels);

    egui::Grid::new("dave_inspector_permissions")
        .num_columns(2)
        .spacing(egui::vec2(
            notedeck::tokens::SPACING_MD,
            notedeck::tokens::SPACING_XS,
        ))
        .show(ui, |ui| {
            key_ui(
                ui,
                &tr!(
                    i18n,
                    "Mode",
                    "Dave session inspector permissions row: the session's permission mode"
                ),
            );
            permission_mode_badge(mode).show(ui);
            ui.end_row();

            key_ui(
                ui,
                &tr!(
                    i18n,
                    "Asked",
                    "Dave session inspector permissions row: prompts a user was asked to answer"
                ),
            );
            ui.horizontal(|ui| {
                value_ui(ui, &labels.asked);
                if !labels.breakdown.is_empty() {
                    weak_ui(ui, &labels.breakdown);
                }
            });
            ui.end_row();

            key_ui(
                ui,
                &tr!(
                    i18n,
                    "Auto-accepted",
                    "Dave session inspector permissions row: prompts accepted without asking"
                ),
            );
            value_ui(ui, &labels.auto_accepted);
            ui.end_row();
        });

    heading_ui(
        ui,
        &tr!(
            i18n,
            "Always allowed this session",
            "Dave session inspector permissions: heading over the session's always-allow rules"
        ),
    );
    let Some(facts) = facts.filter(|f| !f.rules.is_empty()) else {
        weak_ui(
            ui,
            &tr!(
                i18n,
                "No rules yet. Allow Always on a prompt adds one.",
                "Dave session inspector permissions: the session has no always-allow rules"
            ),
        );
        return None;
    };

    let mut action = None;
    for rule in facts.rules {
        if rule_ui(rule, i18n, ui) {
            action = Some(DaveAction::RevokeAllow(rule.clone()));
        }
    }
    if is_remote {
        weak_ui(
            ui,
            &tr!(
                i18n,
                "Rules granted on this device. The host keeps its own.",
                "Dave session inspector permissions: a remote session's rules are this viewer's, not the host's"
            ),
        );
    }
    action
}

/// One rule, in Claude Code's spelling, with its Revoke button on the right.
/// Returns whether Revoke was clicked.
fn rule_ui(rule: &RuntimeAllow, i18n: &mut Localization, ui: &mut egui::Ui) -> bool {
    egui::Frame::new()
        .fill(ui.visuals().panel_fill)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(notedeck::tokens::RADIUS_SM)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                rule_text_ui(rule, ui);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.small_button(tr!(
                        i18n,
                        "Revoke",
                        "Dave session inspector permissions: remove an always-allow rule"
                    ))
                    .clicked()
                })
                .inner
            })
            .inner
        })
        .inner
}

/// `Bash(git:*)` or `Edit`, in mono. A command rule is drawn as three
/// adjacent labels, so it shows without being formatted every frame.
fn rule_text_ui(rule: &RuntimeAllow, ui: &mut egui::Ui) {
    let mono =
        |text: &str| egui::RichText::new(text).font(egui::FontId::monospace(MONO_SIZE + 1.0));
    match rule {
        RuntimeAllow::Command(bin) => {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label(mono("Bash(").weak());
            ui.label(mono(bin));
            ui.label(mono(":*)").weak());
        }
        RuntimeAllow::Tool(tool) => {
            ui.label(mono(tool));
        }
    }
}

/// The card's strings for `counts`.
fn build_labels(counts: PermissionCounts, i18n: &mut Localization) -> PermissionLabels {
    let mut parts: Vec<String> = Vec::new();
    if counts.allowed > 0 {
        parts.push(tr!(
            i18n,
            "{count} allowed",
            "Dave session inspector permissions: prompts a user allowed",
            count = counts.allowed
        ));
    }
    if counts.denied > 0 {
        parts.push(tr!(
            i18n,
            "{count} denied",
            "Dave session inspector permissions: prompts a user denied or exited",
            count = counts.denied
        ));
    }
    if counts.unanswered > 0 {
        parts.push(tr!(
            i18n,
            "{count} unanswered",
            "Dave session inspector permissions: prompts nobody has answered",
            count = counts.unanswered
        ));
    }
    PermissionLabels {
        asked: tr_plural!(
            i18n,
            "{count} prompt",
            "{count} prompts",
            "Dave session inspector permissions: how many prompts a user was asked",
            counts.asked() as i64,
        ),
        breakdown: parts.join(" · "),
        auto_accepted: counts.auto_accepted.to_string(),
    }
}
