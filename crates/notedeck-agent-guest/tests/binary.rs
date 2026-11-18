//! The `notedeck-agent` binary itself, over real stdio: what a guest runs.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use notedeck_agent_proto::call::{CodingCall, ReadInput, WriteInput};
use notedeck_agent_proto::wire::{GuestMessage, HostMessage, PROTOCOL_VERSION};

/// This worktree's build of the binary (see `bin_testing::worktree_bin`).
fn agent_bin() -> PathBuf {
    bin_testing::worktree_bin(
        Path::new(env!("CARGO_BIN_EXE_notedeck-agent")),
        "notedeck-agent-guest",
        |bin| {
            Command::new(bin)
                .arg("--version")
                .output()
                .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("protocol"))
        },
    )
}

#[test]
fn version_names_the_protocol() {
    let out = Command::new(agent_bin()).arg("--version").output().unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.contains(&format!("(protocol {PROTOCOL_VERSION})")),
        "{text}"
    );
}

/// A host's whole conversation: ping, write, read back, shut down, and the
/// process exits cleanly with paths relative to `--root`.
#[test]
fn the_binary_serves_a_session_on_stdio() {
    let root = tempfile::tempdir().unwrap();
    let mut child = Command::new(agent_bin())
        .arg("--root")
        .arg(root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut to_guest = child.stdin.take().unwrap();
    let mut from_guest = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut send = |message: HostMessage| {
        writeln!(to_guest, "{}", serde_json::to_string(&message).unwrap()).unwrap();
    };
    let mut recv =
        || -> GuestMessage { serde_json::from_str(&from_guest.next().unwrap().unwrap()).unwrap() };

    send(HostMessage::Ping { id: 0 });
    assert!(matches!(recv(), GuestMessage::Pong { id: 0, .. }));

    send(HostMessage::Call {
        id: 1,
        call: CodingCall::Write(WriteInput {
            file_path: "hello.txt".into(),
            content: "hello\n".into(),
        }),
    });
    assert!(matches!(recv(), GuestMessage::Result { id: 1, .. }));
    assert_eq!(
        std::fs::read_to_string(root.path().join("hello.txt")).unwrap(),
        "hello\n"
    );

    send(HostMessage::Call {
        id: 2,
        call: CodingCall::Read(ReadInput {
            file_path: "hello.txt".into(),
            offset: None,
            limit: None,
        }),
    });
    assert_eq!(
        recv(),
        GuestMessage::Result {
            id: 2,
            text: "     1\thello\n".into()
        }
    );

    send(HostMessage::Shutdown);
    assert!(child.wait().unwrap().success());
}
