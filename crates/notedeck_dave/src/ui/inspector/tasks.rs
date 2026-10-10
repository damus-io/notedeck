//! The Tasks card: the agent's harness task list, the latest TodoWrite or
//! TaskCreate/TaskUpdate fold, finished items included, under a `done/total`
//! header and a progress bar.
//!
//! Not background tasks, which the Background card shows. The list is the
//! chat's latest `Message::TodoUpdate`, the same one the chat's task sidebar
//! draws, and each row is drawn by the sidebar's [`todo_row_ui`]. The rows
//! borrow from that message's JSON; the only built string is the header,
//! memoized on the counts.

use crate::inspector::LabelMemo;
use crate::ui::task_list::{latest_todos, todo_row_ui, TodoCounts, TodoItem, TodoStatus};
use crate::Message;
use notedeck::{tr, Localization};

use super::rate_limits::fraction_bar_ui;
use super::{value_ui, weak_ui, VALUE_SIZE};

/// Colour of the done fraction, the row checkmark's green.
const DONE_COLOR: egui::Color32 = egui::Color32::from_rgb(34, 197, 94);

/// The Tasks card's body.
pub fn tasks_ui(
    header: &mut LabelMemo<TodoCounts>,
    chat: &[Message],
    i18n: &mut Localization,
    ui: &mut egui::Ui,
) {
    let Some(todos) = latest_todos(chat).filter(|todos| !todos.is_empty()) else {
        weak_ui(
            ui,
            &tr!(
                i18n,
                "No tasks yet",
                "Dave session inspector tasks: the agent has made no task list"
            ),
        );
        return;
    };

    let counts = TodoCounts::of(todos);
    ui.horizontal(|ui| {
        value_ui(ui, header.get(counts, i18n, header_text));
        // The item under way, as it is happening: "Running the tests".
        let active = todos
            .iter()
            .map(TodoItem::from_json)
            .find(|todo| todo.status == TodoStatus::InProgress);
        if let Some(active) = active {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(active.display_text())
                        .font(egui::FontId::proportional(VALUE_SIZE))
                        .weak(),
                )
                .truncate(),
            );
        }
    });
    fraction_bar_ui(ui, counts.done as f32 / counts.total as f32, DONE_COLOR);
    ui.add_space(notedeck::tokens::SPACING_XS);

    for todo in todos {
        todo_row_ui(ui, &TodoItem::from_json(todo));
    }
}

/// "3/5": done over total. A bare fraction reads the same in every locale,
/// and the bar beside it says what it counts.
fn header_text(counts: TodoCounts, _i18n: &mut Localization) -> String {
    format!("{}/{}", counts.done, counts.total)
}
