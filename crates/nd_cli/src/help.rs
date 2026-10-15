//! `nd`'s help table, laid out by [`cli_term::help`] so its pages read like
//! `headway --help` and `agentium --help`.
//!
//! [`CLI`] is also the list of command names: parsing looks a name up here
//! before dispatching, and holds the run to the flags the command's page
//! lists, so a command or flag can't ship undocumented.

use cli_term::help::{Cli, CommandHelp, GlobalOption};

const TOOLS: &str = "TOOLS";
const SERVE: &str = "SERVE";

/// The whole help surface.
pub const CLI: Cli = Cli {
    bin: "nd",
    about: "list and call the running notedeck's tools over its local socket",
    groups: &[TOOLS, SERVE],
    commands: COMMANDS,
    global_hint: "--version",
};

/// The options every run takes.
pub const GLOBALS: &[GlobalOption] = &[GlobalOption {
    flag: "--version",
    summary: "Print nd's version, and the running notedeck's",
    details: "Print nd's own version and, when a notedeck is listening, the version it answers with",
}];

/// What `nd help options` prints after the flags: the exit codes and the
/// socket, which hold for every command.
pub const OPTIONS_FOOTER: &str = "\
EXIT CODES:
    0   the call worked (or there was nothing to call)
    1   the tool answered with an error, or the command line was wrong
    2   notedeck isn't running: nothing is listening on the socket

SOCKET:
    $NOTEDECK_SOCKET, else notedeck's per-user socket (the last notedeck to
    start owns it). nd fetches the tool list from it on every run, so it always
    matches the running app.
";

const COMMANDS: &[CommandHelp] = &[
    CommandHelp {
        name: "tools",
        group: TOOLS,
        summary: "List the running notedeck's tools, or show one",
        usage: &["tools [--json] [<pattern>]", "tools [--json] <name>"],
        details: "\
With no argument, every tool, grouped by category: its name, whether it's a
data tool or drives the window (ui), whether it's read-only (ro), and its
title. A <pattern> keeps the tools whose name, title or description matches
it (a smart-case regex). A <name> that is exactly a tool's prints that tool's
whole description and its arguments.",
        options: &[("--json", "The tool list (or the one tool) as JSON")],
        examples: &["nd tools", "nd tools headway", "nd tools chrome_open"],
    },
    CommandHelp {
        name: "call",
        group: TOOLS,
        summary: "Call one of the running notedeck's tools",
        usage: &["call <name> [<key>=<value>...] [--json <object> | --json-file <path|->]"],
        details: "\
Each <key>=<value> is read as the type the tool's schema gives <key>: text for
a string, a number, true/false, or JSON for an object or array. A key the tool
doesn't take, or a required one left out, fails before anything is sent, with
the tool's arguments listed. --json (or --json-file) gives the arguments as
one object instead; pairs passed beside it override its keys.

A tool's data prints as JSON (indented on a terminal) and exits 0. A tool
that acts on the window prints nothing and exits 0. A tool error goes to
stderr and exits 1.",
        options: &[
            ("--json <object>", "The arguments as one JSON object"),
            (
                "--json-file <path|->",
                "Read that JSON object from a file, or stdin for -",
            ),
        ],
        examples: &[
            "nd call headway_open_review_queue",
            "nd call chrome_open reference=headway:notedeck/balcony-warm-action",
            "nd call chrome_open --json '{\"reference\":\"agentium:evolve-rule-carry\"}'",
        ],
    },
    CommandHelp {
        name: "mcp",
        group: SERVE,
        summary: "Serve the running notedeck's tools to an MCP client on stdio",
        usage: &["mcp [--ui] [--read-only]"],
        details: "\
Speaks MCP (newline-delimited JSON-RPC) on stdin and stdout until stdin
closes. tools/list is the socket's tool list and tools/call calls the tool,
each over a fresh connection, so the server can start before notedeck does.
A tool's data comes back as JSON text (and structuredContent for an object),
a window action as a short \"done\", and a tool error as an isError result.

Only data tools are exposed unless --ui is passed, so an agent can't drive
the window by default; a hidden tool can't be called by name either.",
        options: &[
            ("--ui", "Also expose the tools that act on the window"),
            ("--read-only", "Expose only the tools that change nothing"),
        ],
        examples: &[
            "claude mcp add notedeck -- nd mcp",
            "claude mcp add notedeck-ui -- nd mcp --ui",
            "nd mcp --read-only",
        ],
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_holds_its_invariants() {
        CLI.check().unwrap();
    }

    #[test]
    fn every_command_is_on_the_overview() {
        let overview = CLI.overview(GLOBALS);
        for cmd in COMMANDS {
            assert!(overview.contains(cmd.name), "{}", cmd.name);
        }
        assert!(overview.contains("--version"));
    }
}
