//! UI interaction helpers for Messages end-to-end tests.

use std::time::Duration;

use egui_kittest::kittest::{Key, Queryable};

use super::{step_device_frames, DeviceHarness, TEST_TIMEOUT};

// Re-export generic UI helpers from the shared harness.
pub use notedeck_testing::ui::{click_enabled_label, wait_for_label};

/// Opens a new conversation on the sender entirely through the Messages UI.
pub fn open_conversation_via_ui(sender: &mut DeviceHarness, recipient_npub: &str) {
    wait_for_label(sender, "New Chat", TEST_TIMEOUT);
    click_enabled_label(sender, "New Chat");
    step_device_frames(sender, 2);

    wait_for_label(sender, "Search profiles...", TEST_TIMEOUT);
    sender.get_by_label("Search profiles...").click();
    sender.step();
    sender
        .get_by_label("Search profiles...")
        .type_text(recipient_npub);
    step_device_frames(sender, 2);

    wait_for_label(sender, recipient_npub, TEST_TIMEOUT);
    sender.get_by_label(recipient_npub).click();
    step_device_frames(sender, 3);
    wait_for_label(sender, "Message composer", TEST_TIMEOUT);
}

/// Sends one message through the Messages UI composer and Enter keypath.
pub fn send_message_via_ui(sender: &mut DeviceHarness, content: &str) {
    wait_for_label(sender, "Message composer", TEST_TIMEOUT);
    sender.get_by_label("Message composer").click();
    sender.step();
    sender.get_by_label("Message composer").type_text(content);
    sender.step();
    sender
        .get_by_label("Message composer")
        .key_press(Key::Enter);
    step_device_frames(sender, 2);
}

/// Opens a direct conversation and sends one message from `sender` to `recipient_npub`.
pub fn send_direct_message(sender: &mut DeviceHarness, recipient_npub: &str, content: &str) {
    open_conversation_via_ui(sender, recipient_npub);
    send_message_via_ui(sender, content);
    sender.step();
    std::thread::sleep(Duration::from_millis(25));
}

/// Builds a stable batch of unique message contents for one directed path.
pub fn build_direct_message_batch(sender: &str, recipient: &str, count: usize) -> Vec<String> {
    (1..=count)
        .map(|idx| format!("{sender}->{recipient}:{idx:03}"))
        .collect()
}
