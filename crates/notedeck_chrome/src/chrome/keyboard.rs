//! The soft keyboard: its open/close animation state machine, the keyboard
//! visibility logic that shifts the chrome up when an input would be covered,
//! and the debug virtual keyboard.

use crate::ChromeOptions;
use egui::{Color32, Rect};
use notedeck::{AppContext, NotedeckOptions, SoftKeyboardContext};

pub(super) fn virtual_keyboard_ui(ui: &mut egui::Ui, rect: egui::Rect) {
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, 0.0, Color32::from_black_alpha(200));

    ui.put(rect, |ui: &mut egui::Ui| {
        ui.centered_and_justified(|ui| {
            ui.label("This is a keyboard");
        })
        .response
    });
}

pub(super) struct SoftKeyboardAnim {
    pub(super) skb_rect: Option<Rect>,
    pub(super) anim_height: f32,
}

#[derive(Copy, Default, Clone, Eq, PartialEq, Debug)]
pub(super) enum AnimState {
    /// It finished opening
    Opened,

    /// We started to open
    StartOpen,

    /// We started to close
    StartClose,

    /// We finished openning
    FinishedOpen,

    /// We finished to close
    FinishedClose,

    /// It finished closing
    #[default]
    Closed,

    /// We are animating towards open
    Opening,

    /// We are animating towards close
    Closing,
}

impl SoftKeyboardAnim {
    /// Advance the FSM based on current (anim_height) vs target (skb_rect.height()).
    /// Start*/Finished* are one-tick edge states used for signaling.
    fn changed(&self, state: AnimState) -> AnimState {
        const EPS: f32 = 0.01;

        let target = self.skb_rect.map_or(0.0, |r| r.height());
        let current = self.anim_height;

        let done = (current - target).abs() <= EPS;
        let going_up = target > current + EPS;
        let going_down = current > target + EPS;
        let target_is_closed = target <= EPS;

        match state {
            // Resting states: emit a Start* edge only when a move is requested,
            // and pick direction by the sign of (target - current).
            AnimState::Opened => {
                if done {
                    AnimState::Opened
                } else if going_up {
                    AnimState::StartOpen
                } else {
                    AnimState::StartClose
                }
            }
            AnimState::Closed => {
                if done {
                    AnimState::Closed
                } else if going_up {
                    AnimState::StartOpen
                } else {
                    AnimState::StartClose
                }
            }

            // Edge → flow
            AnimState::StartOpen => AnimState::Opening,
            AnimState::StartClose => AnimState::Closing,

            // Flow states: finish when we hit the target; if the target jumps across,
            // emit the opposite Start* to signal a reversal.
            AnimState::Opening => {
                if done {
                    if target_is_closed {
                        AnimState::FinishedClose
                    } else {
                        AnimState::FinishedOpen
                    }
                } else if going_down {
                    // target moved below current mid-flight → reversal
                    AnimState::StartClose
                } else {
                    AnimState::Opening
                }
            }
            AnimState::Closing => {
                if done {
                    if target_is_closed {
                        AnimState::FinishedClose
                    } else {
                        AnimState::FinishedOpen
                    }
                } else if going_up {
                    // target moved above current mid-flight → reversal
                    AnimState::StartOpen
                } else {
                    AnimState::Closing
                }
            }

            // Finish edges collapse to the stable resting states on the next tick.
            AnimState::FinishedOpen => AnimState::Opened,
            AnimState::FinishedClose => AnimState::Closed,
        }
    }
}

/// How "open" the softkeyboard is. This is an animated value
fn soft_keyboard_anim(
    ui: &mut egui::Ui,
    ctx: &mut AppContext,
    chrome_options: &mut ChromeOptions,
) -> SoftKeyboardAnim {
    let skb_ctx = if chrome_options.contains(ChromeOptions::VirtualKeyboard) {
        SoftKeyboardContext::Virtual
    } else {
        SoftKeyboardContext::Platform {
            ppp: ui.ctx().pixels_per_point(),
        }
    };

    // move screen up if virtual keyboard intersects with input_rect
    let screen_rect = ui.ctx().screen_rect();
    let mut skb_rect: Option<Rect> = None;

    let keyboard_height =
        if let Some(vkb_rect) = ctx.soft_keyboard_rect(screen_rect, skb_ctx.clone()) {
            skb_rect = Some(vkb_rect);
            vkb_rect.height()
        } else {
            0.0
        };

    let anim_height =
        ui.ctx()
            .animate_value_with_time(egui::Id::new("keyboard_anim"), keyboard_height, 0.1);

    SoftKeyboardAnim {
        anim_height,
        skb_rect,
    }
}

fn try_toggle_virtual_keyboard(
    ctx: &egui::Context,
    options: NotedeckOptions,
    chrome_options: &mut ChromeOptions,
) {
    // handle virtual keyboard toggle here because why not
    if options.contains(NotedeckOptions::Debug) && ctx.input(|i| i.key_pressed(egui::Key::F1)) {
        chrome_options.toggle(ChromeOptions::VirtualKeyboard);
    }
}

/// All the logic which handles our keyboard visibility
pub(super) fn keyboard_visibility(
    ui: &mut egui::Ui,
    ctx: &mut AppContext,
    options: &mut ChromeOptions,
    soft_kb_anim_state: &mut AnimState,
) -> SoftKeyboardAnim {
    try_toggle_virtual_keyboard(ui.ctx(), ctx.args.options, options);

    let soft_kb_anim = soft_keyboard_anim(ui, ctx, options);

    let prev_state = *soft_kb_anim_state;
    let current_state = soft_kb_anim.changed(prev_state);
    *soft_kb_anim_state = current_state;

    if prev_state != current_state {
        tracing::debug!("soft kb state {prev_state:?} -> {current_state:?}");
    }

    match current_state {
        // we finished
        AnimState::FinishedOpen => {}

        // on first open, we setup our scroll target
        AnimState::StartOpen => {
            // when we first open the keyboard, check to see if the target soft
            // keyboard rect (the height at full open) intersects with any
            // input response rects from last frame
            //
            // If we do, then we set a bit that we need keyboard visibility.
            // We will use this bit to resize the screen based on the soft
            // keyboard animation state
            if let Some(skb_rect) = soft_kb_anim.skb_rect {
                if let Some(input_rect) = notedeck_ui::input_rect(ui) {
                    options.set(
                        ChromeOptions::KeyboardVisibility,
                        input_rect.intersects(skb_rect),
                    )
                }
            }
        }

        AnimState::FinishedClose => {
            // clear last input box position state
            notedeck_ui::clear_input_rect(ui);
        }

        AnimState::Closing => {}
        AnimState::Opened => {}
        AnimState::Closed => {}
        AnimState::Opening => {}
        AnimState::StartClose => {}
    };

    soft_kb_anim
}
