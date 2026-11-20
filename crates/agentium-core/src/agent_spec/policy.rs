//! The one approval table: capability class by containment.
//!
//! A tool declares what it may do ([`Capability`], on its
//! [`ToolSpec`](crate::tool::ToolSpec)); the runner its toolset uses decides
//! where it runs ([`Containment`]). [`base_approval`] maps the pair to
//! [`Approval`], and an [`AgentSpec`](super::AgentSpec)'s [`Policy`] adjusts
//! the result: tightening freely, widening only by naming the tool and why.
//!
//! Read the table by column and containment relaxes exactly two rows,
//! `MutateWorkspace` and `Shell`: a guest is where edits and commands run
//! unasked. (A guest shell still reaches the network, so
//! [`AgentSpec::validate`](super::AgentSpec::validate) keeps it away from
//! private data rather than this table.) `ActAsUser` never relaxes because the user's key is not
//! in a guest; `Egress` never relaxes because a guest boundary does nothing
//! against exfiltration, the agent already holds what it would leak; and
//! `MutateSharedRepo` is impossible under review containment because the
//! repository's `.git` is mounted read-only there, which the kernel enforces
//! rather than a parse of a shell string.
//!
//! ## The session's permission mode
//!
//! A native-loop session runs in one of dave's permission modes (Manual,
//! Plan, Accept Edits, Auto, Bypass: the Ctrl+M cycle), and
//! [`Policy::resolve_in`] reads it the way Claude Code does, so a coder on
//! the host runs at full power and the mode, not a sandbox, decides what
//! asks:
//!
//! | mode | what changes |
//! |---|---|
//! | Manual | nothing: the table and the spec's policy |
//! | Accept Edits, Auto | `MutateWorkspace` runs unasked in-process and on the host; `Shell` still asks |
//! | Plan | every write (`MutateWorkspace`, `MutateSharedRepo`, `ActAsUser`, `Shell`) is refused |
//! | Bypass | every call that may run, runs unasked |
//!
//! **Auto relaxes exactly what Accept Edits does.** Claude Code's auto mode
//! puts each remaining call to a classifier; the native loop has none, and
//! Auto is the mode a new session starts in, so `Shell`, `Egress`,
//! `ActAsUser` and `MutateSharedRepo` keep asking under it rather than
//! running unread.
//!
//! The mode relaxes the table, not the spec: a spec's tighten still wins, so
//! a spec that gates its edits keeps asking under Accept Edits. Plan and
//! Bypass are caps on the result, since Plan means no writes whatever the
//! spec widened, and Bypass means no asking whatever it tightened (as
//! everywhere in dave). Impossible stays impossible under every mode.

use serde::{Deserialize, Serialize};

use crate::tool::Capability;

/// Where a tool runs, as far as approval cares: the column of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Containment {
    /// Inside notedeck's own process: the app tools (headway, notebook), web
    /// research, a sub-agent call. Same trust as the host: it holds the
    /// user's keys.
    InProcess,
    /// A host subprocess with the user's own filesystem and network.
    Host,
    /// A microVM guest with the worktree mounted read-write and the main
    /// repository's `.git` read-only.
    SandboxedReview,
    /// A microVM guest that may also write the repository's `.git`.
    SandboxedCommit,
}

impl Containment {
    /// Every containment, in table order.
    pub const ALL: [Containment; 4] = [
        Containment::InProcess,
        Containment::Host,
        Containment::SandboxedReview,
        Containment::SandboxedCommit,
    ];

    /// The spelling on a session's `containment` tag.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InProcess => "in_process",
            Self::Host => "host",
            Self::SandboxedReview => "sandboxed_review",
            Self::SandboxedCommit => "sandboxed_commit",
        }
    }

    /// Whether tools under this containment run in a microVM guest, so a
    /// host that can't boot one can't run them.
    pub fn is_sandboxed(self) -> bool {
        matches!(self, Self::SandboxedReview | Self::SandboxedCommit)
    }

    /// Read a `containment` tag back; `None` for a spelling this build
    /// doesn't know, which a reader treats as unknown, never as a default.
    pub fn from_tag_str(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.as_str() == tag)
    }
}

/// What happens when the agent calls a tool. Ordered by restriction, so the
/// stricter of two is their `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approval {
    /// Runs without asking.
    Auto,
    /// Waits for the user to allow it.
    Gated,
    /// Never runs, and the model is told so. Structural where the table says
    /// it, so no widening lifts it.
    Impossible,
}

/// A session's permission mode, as approval reads it (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionMode {
    /// Ask for everything the table and the spec gate.
    Manual,
    /// Read and research only: every write is refused.
    Plan,
    /// Edits to the workspace run unasked.
    AcceptEdits,
    /// As [`AcceptEdits`](Self::AcceptEdits) for the native loop, which has
    /// no classifier to put the rest to.
    Auto,
    /// Nothing asks.
    Bypass,
}

impl SessionMode {
    /// Every mode, in the order dave's UI cycles them, bypass last.
    pub const ALL: [SessionMode; 5] = [
        SessionMode::Manual,
        SessionMode::Plan,
        SessionMode::AcceptEdits,
        SessionMode::Auto,
        SessionMode::Bypass,
    ];

    /// The mode a canonical wire spelling names (see
    /// [`PERMISSION_MODES`](crate::permission_mode::PERMISSION_MODES)), or
    /// `None` for one this build doesn't know.
    pub fn from_wire(mode: &str) -> Option<Self> {
        match mode {
            "default" => Some(Self::Manual),
            "plan" => Some(Self::Plan),
            "accept_edits" => Some(Self::AcceptEdits),
            "auto" => Some(Self::Auto),
            "bypass" => Some(Self::Bypass),
            _ => None,
        }
    }

    /// The table's cell under this mode: Accept Edits and Auto relax a gated
    /// `MutateWorkspace` in-process or on the host, and nothing else.
    fn relax(self, class: Capability, containment: Containment, cell: Approval) -> Approval {
        let edits_unasked = matches!(self, Self::AcceptEdits | Self::Auto)
            && class == Capability::MutateWorkspace
            && matches!(containment, Containment::InProcess | Containment::Host);
        if edits_unasked && cell == Approval::Gated {
            return Approval::Auto;
        }
        cell
    }

    /// The final word on `approval`, after the spec's policy: Plan refuses
    /// every write and Bypass asks for nothing; impossible stays impossible.
    fn cap(self, class: Capability, approval: Approval) -> Approval {
        match self {
            Self::Plan if class.writes() => Approval::Impossible,
            Self::Bypass if approval == Approval::Gated => Approval::Auto,
            _ => approval,
        }
    }
}

/// The table's cell for `class` under `containment`, before any spec
/// policy.
pub fn base_approval(class: Capability, containment: Containment) -> Approval {
    use Approval::*;
    use Capability::*;
    use Containment::*;
    match (class, containment) {
        (Observe | Navigate, _) => Auto,
        (MutateWorkspace | Shell, InProcess | Host) => Gated,
        (MutateWorkspace | Shell, SandboxedReview | SandboxedCommit) => Auto,
        (MutateSharedRepo, SandboxedReview) => Impossible,
        (MutateSharedRepo, InProcess | Host | SandboxedCommit) => Gated,
        (ActAsUser | Egress, _) => Gated,
    }
}

/// A spec's adjustments to the table. Widening runs first and tightening
/// last, so a tighten always wins over a widen of the same tool.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    /// Named exceptions that loosen a tool's approval.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub widen: Vec<Widen>,
    /// Restrictions by tool or by class.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tighten: Vec<Tighten>,
}

/// Loosen one tool, named, with the reason written down. There is no way to
/// widen a whole class: that is a different table, not a spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Widen {
    /// The tool's name as the model sees it, e.g. `headway_comment_card`.
    pub tool: String,
    /// The approval it gets instead: `auto` or `gated`.
    pub to: Approval,
    /// Why this spec may do it unasked, for whoever reads the spec next.
    pub why: String,
}

/// Restrict a tool, or every tool of a class. Names exactly one of `tool`
/// and `class`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tighten {
    /// One tool, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Every tool of this class.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<Capability>,
    /// The approval it gets at least: `gated` or `impossible`.
    pub to: Approval,
}

/// Why a [`Policy`] can't be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    /// A widen names no tool.
    #[error("a widen must name its tool")]
    WidenUnnamed,
    /// A widen gives no reason.
    #[error("widening {0} needs a non-empty `why`")]
    WidenWithoutReason(String),
    /// A widen to `impossible` is a tighten written in the wrong list.
    #[error("widening {0} to impossible: put it under `tighten`")]
    WidenToImpossible(String),
    /// A tighten names neither a tool nor a class, or both.
    #[error("a tighten names exactly one of `tool` and `class`")]
    TightenSelector,
    /// A tighten to `auto` would loosen.
    #[error("a tighten can't be to auto: name the tool under `widen` instead")]
    TightenToAuto,
}

impl Policy {
    /// Check the rules are well formed: every widen named and explained and
    /// never to `impossible`, every tighten naming one target and never to
    /// `auto`.
    pub fn validate(&self) -> Result<(), PolicyError> {
        for widen in &self.widen {
            if widen.tool.trim().is_empty() {
                return Err(PolicyError::WidenUnnamed);
            }
            if widen.why.trim().is_empty() {
                return Err(PolicyError::WidenWithoutReason(widen.tool.clone()));
            }
            if widen.to == Approval::Impossible {
                return Err(PolicyError::WidenToImpossible(widen.tool.clone()));
            }
        }
        for tighten in &self.tighten {
            if tighten.tool.is_some() == tighten.class.is_some() {
                return Err(PolicyError::TightenSelector);
            }
            if tighten.to == Approval::Auto {
                return Err(PolicyError::TightenToAuto);
            }
        }
        Ok(())
    }

    /// The approval for the tool `tool` of class `class` under
    /// `containment`: the table's cell, then any widen naming the tool
    /// (which can't lift an `impossible` cell), then every matching tighten.
    /// This is [`resolve_in`](Self::resolve_in) under
    /// [`SessionMode::Manual`], which changes nothing.
    pub fn resolve(&self, tool: &str, class: Capability, containment: Containment) -> Approval {
        self.resolve_in(tool, class, containment, SessionMode::Manual)
    }

    /// [`resolve`](Self::resolve) in a session running in `mode`: the mode
    /// relaxes the table's cell before the policy reads it, and has the
    /// last word after (see the module docs).
    pub fn resolve_in(
        &self,
        tool: &str,
        class: Capability,
        containment: Containment,
        mode: SessionMode,
    ) -> Approval {
        let base = base_approval(class, containment);
        let cell = mode.relax(class, containment, base);
        let widened = match self.widen.iter().rfind(|w| w.tool == tool) {
            Some(w) if base != Approval::Impossible => w.to,
            _ => cell,
        };
        let tightened = self
            .tighten
            .iter()
            .filter(|t| t.tool.as_deref() == Some(tool) || t.class == Some(class))
            .fold(widened, |approval, t| approval.max(t.to));
        mode.cap(class, tightened)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Approval::*;
    use Capability::*;
    use Containment::*;

    /// One `#[test]` per cell of the table, so a changed cell fails by name.
    macro_rules! cells {
        ($($name:ident: $class:ident x $containment:ident => $approval:ident;)*) => {
            $(
                #[test]
                fn $name() {
                    assert_eq!(base_approval($class, $containment), $approval);
                }
            )*
            /// Every cell is listed above exactly once.
            #[test]
            fn every_cell_has_a_test() {
                let listed = [$(($class, $containment)),*];
                for class in Capability::ALL {
                    for containment in Containment::ALL {
                        let n = listed.iter().filter(|c| **c == (class, containment)).count();
                        assert_eq!(n, 1, "{class:?} x {containment:?} listed {n} times");
                    }
                }
            }
        };
    }

    cells! {
        observe_in_process: Observe x InProcess => Auto;
        observe_host: Observe x Host => Auto;
        observe_sandboxed_review: Observe x SandboxedReview => Auto;
        observe_sandboxed_commit: Observe x SandboxedCommit => Auto;

        navigate_in_process: Navigate x InProcess => Auto;
        navigate_host: Navigate x Host => Auto;
        navigate_sandboxed_review: Navigate x SandboxedReview => Auto;
        navigate_sandboxed_commit: Navigate x SandboxedCommit => Auto;

        mutate_workspace_in_process: MutateWorkspace x InProcess => Gated;
        mutate_workspace_host: MutateWorkspace x Host => Gated;
        mutate_workspace_sandboxed_review: MutateWorkspace x SandboxedReview => Auto;
        mutate_workspace_sandboxed_commit: MutateWorkspace x SandboxedCommit => Auto;

        mutate_shared_repo_in_process: MutateSharedRepo x InProcess => Gated;
        mutate_shared_repo_host: MutateSharedRepo x Host => Gated;
        mutate_shared_repo_sandboxed_review: MutateSharedRepo x SandboxedReview => Impossible;
        mutate_shared_repo_sandboxed_commit: MutateSharedRepo x SandboxedCommit => Gated;

        act_as_user_in_process: ActAsUser x InProcess => Gated;
        act_as_user_host: ActAsUser x Host => Gated;
        act_as_user_sandboxed_review: ActAsUser x SandboxedReview => Gated;
        act_as_user_sandboxed_commit: ActAsUser x SandboxedCommit => Gated;

        egress_in_process: Egress x InProcess => Gated;
        egress_host: Egress x Host => Gated;
        egress_sandboxed_review: Egress x SandboxedReview => Gated;
        egress_sandboxed_commit: Egress x SandboxedCommit => Gated;

        shell_in_process: Shell x InProcess => Gated;
        shell_host: Shell x Host => Gated;
        shell_sandboxed_review: Shell x SandboxedReview => Auto;
        shell_sandboxed_commit: Shell x SandboxedCommit => Auto;
    }

    /// Containment relaxes exactly the workspace and the shell: every other
    /// class but MutateSharedRepo reads the same down every column, and only
    /// those two get looser.
    #[test]
    fn containment_relaxes_only_the_workspace_and_the_shell() {
        for class in Capability::ALL {
            let host = base_approval(class, Host);
            for containment in Containment::ALL {
                let cell = base_approval(class, containment);
                if cell < host {
                    assert!(
                        matches!(class, MutateWorkspace | Shell),
                        "{class:?} relaxed under {containment:?}"
                    );
                }
            }
        }
    }

    fn widen(tool: &str, to: Approval) -> Widen {
        Widen {
            tool: tool.to_string(),
            to,
            why: "test".to_string(),
        }
    }

    fn tighten_tool(tool: &str, to: Approval) -> Tighten {
        Tighten {
            tool: Some(tool.to_string()),
            class: None,
            to,
        }
    }

    fn tighten_class(class: Capability, to: Approval) -> Tighten {
        Tighten {
            tool: None,
            class: Some(class),
            to,
        }
    }

    #[test]
    fn a_named_widen_loosens_only_that_tool() {
        let policy = Policy {
            widen: vec![widen("headway_comment_card", Auto)],
            tighten: vec![],
        };
        assert_eq!(
            policy.resolve("headway_comment_card", ActAsUser, InProcess),
            Auto
        );
        assert_eq!(
            policy.resolve("headway_move_card", ActAsUser, InProcess),
            Gated
        );
    }

    #[test]
    fn a_widen_never_lifts_an_impossible_cell() {
        let policy = Policy {
            widen: vec![widen("git_commit", Auto)],
            tighten: vec![],
        };
        assert_eq!(
            policy.resolve("git_commit", MutateSharedRepo, SandboxedReview),
            Impossible
        );
    }

    #[test]
    fn a_tighten_beats_a_widen_of_the_same_tool() {
        let policy = Policy {
            widen: vec![widen("headway_comment_card", Auto)],
            tighten: vec![tighten_class(ActAsUser, Gated)],
        };
        assert_eq!(
            policy.resolve("headway_comment_card", ActAsUser, InProcess),
            Gated
        );
    }

    #[test]
    fn a_tighten_only_ever_restricts() {
        // Tightening an already-impossible cell to gated leaves it impossible.
        let policy = Policy {
            widen: vec![],
            tighten: vec![tighten_tool("git_commit", Gated)],
        };
        assert_eq!(
            policy.resolve("git_commit", MutateSharedRepo, SandboxedReview),
            Impossible
        );
        // And a class tighten reaches every tool of the class.
        let policy = Policy {
            widen: vec![],
            tighten: vec![tighten_class(MutateWorkspace, Impossible)],
        };
        assert_eq!(
            policy.resolve("Edit", MutateWorkspace, SandboxedCommit),
            Impossible
        );
        assert_eq!(policy.resolve("Read", Observe, SandboxedCommit), Auto);
    }

    #[test]
    fn validate_rejects_unnamed_unexplained_or_backwards_rules() {
        let bad = |policy: Policy| policy.validate().unwrap_err();
        let mut unexplained = widen("x", Auto);
        unexplained.why = " ".to_string();
        assert_eq!(
            bad(Policy {
                widen: vec![unexplained],
                tighten: vec![]
            }),
            PolicyError::WidenWithoutReason("x".to_string())
        );
        assert_eq!(
            bad(Policy {
                widen: vec![widen("", Auto)],
                tighten: vec![]
            }),
            PolicyError::WidenUnnamed
        );
        assert_eq!(
            bad(Policy {
                widen: vec![widen("x", Impossible)],
                tighten: vec![]
            }),
            PolicyError::WidenToImpossible("x".to_string())
        );
        assert_eq!(
            bad(Policy {
                widen: vec![],
                tighten: vec![tighten_tool("x", Auto)]
            }),
            PolicyError::TightenToAuto
        );
        let both = Tighten {
            tool: Some("x".to_string()),
            class: Some(Egress),
            to: Gated,
        };
        assert_eq!(
            bad(Policy {
                widen: vec![],
                tighten: vec![both]
            }),
            PolicyError::TightenSelector
        );
    }

    #[test]
    fn containment_tags_round_trip() {
        for containment in Containment::ALL {
            assert_eq!(
                Containment::from_tag_str(containment.as_str()),
                Some(containment)
            );
        }
        assert_eq!(Containment::from_tag_str("guest"), None);
    }

    /// The empty policy under `mode`, for a tool of `class` in
    /// `containment`.
    fn in_mode(class: Capability, containment: Containment, mode: SessionMode) -> Approval {
        Policy::default().resolve_in("t", class, containment, mode)
    }

    /// Manual changes nothing: `resolve` and `resolve_in(Manual)` agree on
    /// every cell, so a session that never set a mode asks as before.
    #[test]
    fn manual_mode_is_the_table() {
        for class in Capability::ALL {
            for containment in Containment::ALL {
                assert_eq!(
                    in_mode(class, containment, SessionMode::Manual),
                    base_approval(class, containment)
                );
            }
        }
    }

    /// Accept Edits and Auto relax exactly one gate: a workspace edit
    /// in-process or on the host. Publishing as the user, the network and
    /// the shared repository keep asking under both.
    #[test]
    fn accept_edits_and_auto_relax_only_host_edits() {
        for mode in [SessionMode::AcceptEdits, SessionMode::Auto] {
            for class in Capability::ALL {
                for containment in Containment::ALL {
                    let cell = base_approval(class, containment);
                    let relaxed =
                        class == MutateWorkspace && matches!(containment, InProcess | Host);
                    let want = if relaxed { Auto } else { cell };
                    assert_eq!(
                        in_mode(class, containment, mode),
                        want,
                        "{class:?} x {containment:?} under {mode:?}"
                    );
                }
            }
        }
    }

    /// Plan refuses every write wherever it runs, even one the spec
    /// widened, and leaves reads and research alone.
    #[test]
    fn plan_refuses_every_write() {
        for class in Capability::ALL {
            for containment in Containment::ALL {
                let want = if class.writes() {
                    Impossible
                } else {
                    base_approval(class, containment)
                };
                assert_eq!(in_mode(class, containment, SessionMode::Plan), want);
            }
        }
        let widened = Policy {
            widen: vec![widen("headway_comment_card", Auto)],
            tighten: vec![],
        };
        assert_eq!(
            widened.resolve_in(
                "headway_comment_card",
                ActAsUser,
                InProcess,
                SessionMode::Plan
            ),
            Impossible
        );
    }

    /// Bypass asks for nothing, but impossible stays impossible.
    #[test]
    fn bypass_asks_for_nothing_and_lifts_no_impossible() {
        for class in Capability::ALL {
            for containment in Containment::ALL {
                let want = match base_approval(class, containment) {
                    Impossible => Impossible,
                    _ => Auto,
                };
                assert_eq!(in_mode(class, containment, SessionMode::Bypass), want);
            }
        }
    }

    /// The mode relaxes the table, not the spec: a spec that gates its
    /// edits keeps asking under Accept Edits, and one that makes them
    /// impossible keeps them so.
    #[test]
    fn a_spec_tighten_beats_the_mode() {
        let gated = Policy {
            widen: vec![],
            tighten: vec![tighten_class(MutateWorkspace, Gated)],
        };
        let refused = Policy {
            widen: vec![],
            tighten: vec![tighten_tool("Bash", Impossible)],
        };
        for mode in [SessionMode::AcceptEdits, SessionMode::Auto] {
            assert_eq!(gated.resolve_in("Edit", MutateWorkspace, Host, mode), Gated);
            assert_eq!(refused.resolve_in("Bash", Shell, Host, mode), Impossible);
        }
        assert_eq!(
            refused.resolve_in("Bash", Shell, Host, SessionMode::Bypass),
            Impossible
        );
    }

    /// Every canonical wire spelling names a mode, and the cycle's order is
    /// the wire's.
    #[test]
    fn every_wire_mode_has_a_session_mode() {
        let modes: Vec<_> = crate::permission_mode::PERMISSION_MODES
            .iter()
            .map(|m| SessionMode::from_wire(m).expect("known mode"))
            .collect();
        assert_eq!(modes, SessionMode::ALL);
        assert_eq!(SessionMode::from_wire("manual"), None);
    }
}
