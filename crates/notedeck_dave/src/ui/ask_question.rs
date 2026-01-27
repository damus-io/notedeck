//! UI for rendering AskUserQuestion tool calls from Claude Code

use crate::messages::{AskUserQuestionInput, PermissionRequest, QuestionAnswer};
use std::collections::HashMap;
use uuid::Uuid;

use super::badge;
use super::DaveAction;

/// Render an AskUserQuestion tool call with selectable options
///
/// Returns a `DaveAction::QuestionResponse` when the user submits their answers.
pub fn ask_user_question_ui(
    request: &PermissionRequest,
    questions: &AskUserQuestionInput,
    answers_map: &mut HashMap<Uuid, Vec<QuestionAnswer>>,
    ui: &mut egui::Ui,
) -> Option<DaveAction> {
    let mut action = None;
    let inner_margin = 12.0;
    let corner_radius = 8.0;

    // Get or initialize answer state for this request
    let num_questions = questions.questions.len();
    let answers = answers_map
        .entry(request.id)
        .or_insert_with(|| vec![QuestionAnswer::default(); num_questions]);

    egui::Frame::new()
        .fill(ui.visuals().widgets.noninteractive.bg_fill)
        .inner_margin(inner_margin)
        .corner_radius(corner_radius)
        .stroke(egui::Stroke::new(
            1.0,
            ui.visuals().selection.stroke.color,
        ))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                for (q_idx, question) in questions.questions.iter().enumerate() {
                    // Ensure we have an answer entry for this question
                    if q_idx >= answers.len() {
                        answers.push(QuestionAnswer::default());
                    }

                    ui.add_space(4.0);

                    // Header badge and question text
                    ui.horizontal(|ui| {
                        badge::StatusBadge::new(&question.header)
                            .variant(badge::BadgeVariant::Info)
                            .show(ui);
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new(&question.question).strong());
                    });

                    ui.add_space(8.0);

                    // Options
                    for (opt_idx, option) in question.options.iter().enumerate() {
                        let is_selected = answers[q_idx].selected.contains(&opt_idx);
                        let other_is_selected = answers[q_idx].other_text.is_some();

                        ui.horizontal(|ui| {
                            if question.multi_select {
                                // Checkbox for multi-select
                                let mut checked = is_selected;
                                if ui.checkbox(&mut checked, "").changed() {
                                    if checked {
                                        answers[q_idx].selected.push(opt_idx);
                                    } else {
                                        answers[q_idx].selected.retain(|&i| i != opt_idx);
                                    }
                                }
                            } else {
                                // Radio button for single-select
                                let selected = is_selected && !other_is_selected;
                                if ui.radio(selected, "").clicked() {
                                    answers[q_idx].selected = vec![opt_idx];
                                    answers[q_idx].other_text = None;
                                }
                            }

                            ui.vertical(|ui| {
                                ui.label(egui::RichText::new(&option.label));
                                ui.label(
                                    egui::RichText::new(&option.description)
                                        .weak()
                                        .size(11.0),
                                );
                            });
                        });

                        ui.add_space(4.0);
                    }

                    // "Other" option
                    ui.horizontal(|ui| {
                        let other_selected = answers[q_idx].other_text.is_some();
                        if question.multi_select {
                            let mut checked = other_selected;
                            if ui.checkbox(&mut checked, "").changed() {
                                if checked {
                                    answers[q_idx].other_text = Some(String::new());
                                } else {
                                    answers[q_idx].other_text = None;
                                }
                            }
                        } else if ui.radio(other_selected, "").clicked() {
                            answers[q_idx].selected.clear();
                            answers[q_idx].other_text = Some(String::new());
                        }

                        ui.label("Other:");

                        // Text input for "Other"
                        if let Some(ref mut text) = answers[q_idx].other_text {
                            ui.add(
                                egui::TextEdit::singleline(text)
                                    .desired_width(200.0)
                                    .hint_text("Type your answer..."),
                            );
                        }
                    });

                    ui.add_space(8.0);
                    if q_idx < questions.questions.len() - 1 {
                        ui.separator();
                    }
                }

                // Submit button
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let button_text_color = ui.visuals().widgets.active.fg_stroke.color;
                    let submit_response = badge::ActionButton::new(
                        "Submit",
                        egui::Color32::from_rgb(34, 139, 34),
                        button_text_color,
                    )
                    .keybind("Enter")
                    .show(ui);

                    if submit_response.clicked()
                        || ui.input(|i| {
                            i.key_pressed(egui::Key::Enter) && !i.modifiers.shift
                        })
                    {
                        action = Some(DaveAction::QuestionResponse {
                            request_id: request.id,
                            answers: answers.clone(),
                        });
                    }
                });
            });
        });

    action
}
