//! UI for rendering AskUserQuestion tool calls from Claude Code

use crate::messages::{AskUserQuestionInput, PermissionRequest, QuestionAnswer};
use std::collections::HashMap;
use uuid::Uuid;

use super::badge;
use super::keybind_hint;
use super::DaveAction;

/// Render an AskUserQuestion tool call with selectable options
///
/// Shows one question at a time with numbered options.
/// Returns a `DaveAction::QuestionResponse` when the user submits all answers.
pub fn ask_user_question_ui(
    request: &PermissionRequest,
    questions: &AskUserQuestionInput,
    answers_map: &mut HashMap<Uuid, Vec<QuestionAnswer>>,
    index_map: &mut HashMap<Uuid, usize>,
    ui: &mut egui::Ui,
) -> Option<DaveAction> {
    let mut action = None;
    let inner_margin = 12.0;
    let corner_radius = 8.0;

    let num_questions = questions.questions.len();

    // Get or initialize answer state for this request
    let answers = answers_map
        .entry(request.id)
        .or_insert_with(|| vec![QuestionAnswer::default(); num_questions]);

    // Get current question index
    let current_idx = *index_map.entry(request.id).or_insert(0);

    // Ensure we have a valid index
    if current_idx >= num_questions {
        // All questions answered, shouldn't happen but handle gracefully
        return None;
    }

    let question = &questions.questions[current_idx];

    // Ensure we have an answer entry for this question
    while answers.len() <= current_idx {
        answers.push(QuestionAnswer::default());
    }

    egui::Frame::new()
        .fill(ui.visuals().widgets.noninteractive.bg_fill)
        .inner_margin(inner_margin)
        .corner_radius(corner_radius)
        .stroke(egui::Stroke::new(1.0, ui.visuals().selection.stroke.color))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                // Progress indicator if multiple questions
                if num_questions > 1 {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!(
                                "Question {} of {}",
                                current_idx + 1,
                                num_questions
                            ))
                            .weak()
                            .size(11.0),
                        );
                    });
                    ui.add_space(4.0);
                }

                // Header badge and question text
                ui.horizontal(|ui| {
                    badge::StatusBadge::new(&question.header)
                        .variant(badge::BadgeVariant::Info)
                        .show(ui);
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new(&question.question).strong());
                });

                ui.add_space(8.0);

                // Check for number key presses
                let pressed_number = ui.input(|i| {
                    for n in 1..=9 {
                        let key = match n {
                            1 => egui::Key::Num1,
                            2 => egui::Key::Num2,
                            3 => egui::Key::Num3,
                            4 => egui::Key::Num4,
                            5 => egui::Key::Num5,
                            6 => egui::Key::Num6,
                            7 => egui::Key::Num7,
                            8 => egui::Key::Num8,
                            9 => egui::Key::Num9,
                            _ => unreachable!(),
                        };
                        if i.key_pressed(key) && !i.modifiers.shift && !i.modifiers.ctrl {
                            return Some(n);
                        }
                    }
                    None
                });

                // Options (numbered 1-N)
                let num_options = question.options.len();
                for (opt_idx, option) in question.options.iter().enumerate() {
                    let option_num = opt_idx + 1;
                    let is_selected = answers[current_idx].selected.contains(&opt_idx);
                    let other_is_selected = answers[current_idx].other_text.is_some();

                    // Handle keyboard selection
                    if pressed_number == Some(option_num) {
                        if question.multi_select {
                            if is_selected {
                                answers[current_idx].selected.retain(|&i| i != opt_idx);
                            } else {
                                answers[current_idx].selected.push(opt_idx);
                            }
                        } else {
                            answers[current_idx].selected = vec![opt_idx];
                            answers[current_idx].other_text = None;
                        }
                    }

                    ui.horizontal(|ui| {
                        // Number hint
                        keybind_hint(ui, &option_num.to_string());

                        if question.multi_select {
                            // Checkbox for multi-select
                            let mut checked = is_selected;
                            if ui.checkbox(&mut checked, "").changed() {
                                if checked {
                                    answers[current_idx].selected.push(opt_idx);
                                } else {
                                    answers[current_idx].selected.retain(|&i| i != opt_idx);
                                }
                            }
                        } else {
                            // Radio button for single-select
                            let selected = is_selected && !other_is_selected;
                            if ui.radio(selected, "").clicked() {
                                answers[current_idx].selected = vec![opt_idx];
                                answers[current_idx].other_text = None;
                            }
                        }

                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(&option.label));
                            ui.label(egui::RichText::new(&option.description).weak().size(11.0));
                        });
                    });

                    ui.add_space(4.0);
                }

                // "Other" option (numbered as last option + 1)
                let other_num = num_options + 1;
                let other_selected = answers[current_idx].other_text.is_some();

                // Handle keyboard selection for "Other"
                if pressed_number == Some(other_num) {
                    if question.multi_select {
                        if other_selected {
                            answers[current_idx].other_text = None;
                        } else {
                            answers[current_idx].other_text = Some(String::new());
                        }
                    } else {
                        answers[current_idx].selected.clear();
                        answers[current_idx].other_text = Some(String::new());
                    }
                }

                ui.horizontal(|ui| {
                    // Number hint for "Other"
                    keybind_hint(ui, &other_num.to_string());

                    if question.multi_select {
                        let mut checked = other_selected;
                        if ui.checkbox(&mut checked, "").changed() {
                            if checked {
                                answers[current_idx].other_text = Some(String::new());
                            } else {
                                answers[current_idx].other_text = None;
                            }
                        }
                    } else if ui.radio(other_selected, "").clicked() {
                        answers[current_idx].selected.clear();
                        answers[current_idx].other_text = Some(String::new());
                    }

                    ui.label("Other:");

                    // Text input for "Other"
                    if let Some(text) = &mut answers[current_idx].other_text {
                        ui.add(
                            egui::TextEdit::singleline(text)
                                .desired_width(200.0)
                                .hint_text("Type your answer..."),
                        );
                    }
                });

                // Submit button
                ui.add_space(8.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let button_text_color = ui.visuals().widgets.active.fg_stroke.color;

                    let is_last_question = current_idx == num_questions - 1;
                    let button_label = if is_last_question { "Submit" } else { "Next" };

                    let submit_response = badge::ActionButton::new(
                        button_label,
                        egui::Color32::from_rgb(34, 139, 34),
                        button_text_color,
                    )
                    .keybind("\u{21B5}") // ↵ enter symbol
                    .show(ui);

                    if submit_response.clicked()
                        || ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift)
                    {
                        if is_last_question {
                            // All questions answered, submit
                            action = Some(DaveAction::QuestionResponse {
                                request_id: request.id,
                                answers: answers.clone(),
                            });
                        } else {
                            // Move to next question
                            index_map.insert(request.id, current_idx + 1);
                        }
                    }
                });
            });
        });

    action
}

/// Render a compact summary of an answered AskUserQuestion
///
/// Shows the question header(s) and selected answer(s) in a single line.
/// Uses pre-computed AnswerSummary to avoid per-frame allocations.
pub fn ask_user_question_summary_ui(summary: &crate::messages::AnswerSummary, ui: &mut egui::Ui) {
    let inner_margin = 8.0;
    let corner_radius = 6.0;

    egui::Frame::new()
        .fill(ui.visuals().widgets.noninteractive.bg_fill)
        .inner_margin(inner_margin)
        .corner_radius(corner_radius)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for (idx, entry) in summary.entries.iter().enumerate() {
                    // Add separator between questions
                    if idx > 0 {
                        ui.separator();
                    }

                    // Header badge
                    badge::StatusBadge::new(&entry.header)
                        .variant(badge::BadgeVariant::Info)
                        .show(ui);

                    // Pre-computed answer text
                    ui.label(
                        egui::RichText::new(&entry.answer)
                            .color(egui::Color32::from_rgb(100, 180, 100)),
                    );
                }
            });
        });
}
