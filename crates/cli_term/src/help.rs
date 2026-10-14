//! The help surface the CLIs share: a table of commands, printed two ways.
//!
//! `<bin> --help` prints the command list grouped under section headings — a
//! name and a one-line summary each — plus the options that apply to every
//! run. Everything command-specific lives on that command's own page, printed
//! by `<bin> <cmd> --help`, so a reader looking up one command isn't reading
//! past thirty unrelated flags to find its two.
//!
//! Each CLI keeps its own table of [`CommandHelp`] entries and wraps it in a
//! [`Cli`]; this module owns only the layout, so `headway` and `agentium` pages
//! read the same instead of drifting apart. Both layouts render to a `String`
//! (the CLIs `eprint!` it), which is what lets a test compare them byte for
//! byte.

/// One command's help: its line in the grouped list, and its own page.
pub struct CommandHelp {
    /// The name as typed, e.g. `move-board`.
    pub name: &'static str,
    /// The section heading the command is listed under in the overview, e.g.
    /// `"READING"`. Must be one of its [`Cli::groups`].
    pub group: &'static str,
    /// One line for the command list. No trailing period.
    pub summary: &'static str,
    /// The usage line(s) on the command's page, without the leading program
    /// name.
    pub usage: &'static [&'static str],
    /// The body of the command's page: what it does and why, pre-wrapped to the
    /// help column. Empty for a command the summary already covers.
    pub details: &'static str,
    /// The `(flag, description)` pairs this command reads — only its own; the
    /// global ones stay on the overview.
    pub options: &'static [(&'static str, &'static str)],
    /// Full command lines, printed verbatim.
    pub examples: &'static [&'static str],
}

/// A CLI's whole help table: its name, its sections, and its commands.
pub struct Cli {
    /// The program name, e.g. `headway`, printed before every usage line.
    pub bin: &'static str,
    /// The overview's one-line introduction, after `<bin> — `.
    pub about: &'static str,
    /// The section headings, in the order the overview prints them.
    pub groups: &'static [&'static str],
    /// Every command the CLI accepts. A CLI resolves command names through
    /// [`Cli::lookup`], so a command missing its entry is rejected rather than
    /// shipping with no help.
    pub commands: &'static [CommandHelp],
    /// A few of the global flags, named in each page's closing pointer to the
    /// overview, e.g. `"--board, --relay, ..."`.
    pub global_hint: &'static str,
}

/// The column command names and option flags are padded to, in both help
/// layouts. Wide enough for the longest of each (`move-board`, `-n, --count <k>`).
pub const NAME_COL: usize = 18;

/// The width help text wraps to, so an option's description hangs under itself
/// rather than running off an 80-column terminal.
pub const WIDTH: usize = 78;

impl Cli {
    /// The help entry for a command name, or `None` when the CLI has no such
    /// command.
    pub fn lookup(&self, name: &str) -> Option<&'static CommandHelp> {
        self.commands.iter().find(|c| c.name == name)
    }

    /// The grouped command list: `<bin> --help`.
    ///
    /// `globals` are the options every command takes, as `(flag, description)`
    /// pairs; `footer` is CLI-specific prose printed a blank line after them (a
    /// section with its own heading, newline-terminated), or empty for none.
    pub fn overview(&self, globals: &[(&str, &str)], footer: &str) -> String {
        let bin = self.bin;
        let mut out = format!(
            "\
{bin} — {about}

USAGE:
    {bin} [OPTIONS] <COMMAND> [ARGS...]
    {bin} <COMMAND> --help      Options and examples for one command

",
            about = self.about,
        );

        for group in self.groups {
            out.push_str(group);
            out.push('\n');
            for cmd in self.commands.iter().filter(|c| c.group == *group) {
                out.push_str(&format!(
                    "    {:<width$}{}\n",
                    cmd.name,
                    cmd.summary,
                    width = NAME_COL
                ));
            }
            out.push('\n');
        }

        out.push_str("OPTIONS (every command):\n");
        out.push_str(&options_block(globals));
        if !footer.is_empty() {
            out.push('\n');
            out.push_str(footer);
        }
        out.push_str(&format!(
            "\nRun `{bin} <command> --help` for that command's own options and examples.\n"
        ));
        out
    }

    /// One command's page: `<bin> <cmd> --help`.
    pub fn page(&self, cmd: &CommandHelp) -> String {
        let bin = self.bin;
        let mut out = format!("{bin} {} — {}\n\n", cmd.name, cmd.summary);

        out.push_str("USAGE:\n");
        for usage in cmd.usage {
            out.push_str(&format!("    {bin} {usage}\n"));
        }

        if !cmd.details.is_empty() {
            out.push('\n');
            for line in cmd.details.lines() {
                out.push_str(line);
                out.push('\n');
            }
        }

        if !cmd.options.is_empty() {
            out.push_str("\nOPTIONS:\n");
            out.push_str(&options_block(cmd.options));
        }

        if !cmd.examples.is_empty() {
            out.push_str("\nEXAMPLES:\n");
            for example in cmd.examples {
                out.push_str(&format!("    {example}\n"));
            }
        }

        out.push_str(&format!(
            "\nOptions every command takes ({}): {bin} --help\n",
            self.global_hint
        ));
        out
    }

    /// Hold a run to the flags `cmd` documents: `Err` naming the first of
    /// `passed` that is neither one of `globals` nor among `cmd`'s own
    /// [`options`](CommandHelp::options), e.g. `` `list` doesn't take --prompt
    /// (see `agentium list --help`) ``.
    ///
    /// `passed` is every flag the run gave, spelled as typed, in the order
    /// seen. Reading the accepted set off the help table is what keeps a
    /// command's page and its parser from drifting: a flag the parser reads but
    /// the page leaves out is rejected the first time anyone passes it.
    pub fn check_flags<S: AsRef<str>>(
        &self,
        cmd: &CommandHelp,
        globals: &[(&str, &str)],
        passed: &[S],
    ) -> Result<(), String> {
        let takes = |flag: &str| {
            globals
                .iter()
                .chain(cmd.options)
                .any(|(key, _)| option_names(key).any(|name| name == flag))
        };
        let Some(flag) = passed.iter().map(AsRef::as_ref).find(|f| !takes(f)) else {
            return Ok(());
        };
        Err(format!(
            "`{name}` doesn't take {flag} (see `{bin} {name} --help`)",
            name = cmd.name,
            bin = self.bin,
        ))
    }

    /// The table's invariants, for each CLI to assert from its own tests:
    /// command names are unique (a duplicate would shadow its twin in
    /// [`lookup`](Cli::lookup) and print twice), and every command's group is
    /// one the overview prints (or the entry would silently go missing from
    /// it).
    pub fn check(&self) -> Result<(), String> {
        for (i, cmd) in self.commands.iter().enumerate() {
            if self.commands[..i].iter().any(|c| c.name == cmd.name) {
                return Err(format!("duplicate help entry for '{}'", cmd.name));
            }
            if !self.groups.contains(&cmd.group) {
                return Err(format!(
                    "'{}' is filed under '{}', which `{} --help` doesn't print",
                    cmd.name, cmd.group, self.bin
                ));
            }
        }
        Ok(())
    }
}

/// The flag names an option key spells: `"-n, --last <n>"` gives `-n` and
/// `--last`. Each comma-separated part contributes its first word, and only a
/// word that starts with `-` counts, so a comma inside a value placeholder
/// (`"--role <r[,r…]>"`) adds nothing.
pub fn option_names(key: &str) -> impl Iterator<Item = &str> {
    key.split(',')
        .filter_map(|part| part.split_whitespace().next())
        .filter(|word| word.starts_with('-'))
}

/// Render `(flag, description)` pairs as an aligned, wrapped block — each line
/// already indented and newline-terminated, so a caller can print it as-is.
pub fn options_block(options: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (flag, description) in options {
        // A flag wider than the column gets its description on the next line
        // rather than pushing the whole block out of alignment.
        let body = wrapped(description, NAME_COL + 4);
        if flag.len() >= NAME_COL || body.is_empty() {
            out.push_str(&format!("    {flag}\n"));
            out.push_str(&body);
        } else {
            // The first line's indent is replaced by the padded flag; the
            // continuation lines keep theirs, so they hang under the text.
            out.push_str(&format!("    {flag:<NAME_COL$}{}", body.trim_start()));
        }
    }
    out
}

/// Wrap `text` to [`WIDTH`] columns, indenting every line by `indent` spaces.
/// Words longer than the remaining width simply overflow — a URL is more useful
/// unbroken than wrapped. Width is counted in characters, not bytes: the help
/// text is full of em dashes, and measuring those as three columns each would
/// wrap every line that holds one short.
pub fn wrapped(text: &str, indent: usize) -> String {
    let mut out = String::new();
    let mut line = String::new();
    let mut width = 0;
    for word in text.split_whitespace() {
        let word_width = word.chars().count();
        if !line.is_empty() && indent + width + 1 + word_width > WIDTH {
            out.push_str(&format!("{:indent$}{line}\n", ""));
            line.clear();
            width = 0;
        }
        if !line.is_empty() {
            line.push(' ');
            width += 1;
        }
        line.push_str(word);
        width += word_width;
    }
    if !line.is_empty() {
        out.push_str(&format!("{:indent$}{line}\n", ""));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: Cli = Cli {
        bin: "demo",
        about: "a demo CLI",
        groups: &["READING", "WRITING"],
        commands: &[
            CommandHelp {
                name: "show",
                group: "READING",
                summary: "Print things",
                usage: &["show [things...]"],
                details: "Shows things.",
                options: &[("--all", "Every thing")],
                examples: &["demo show --all"],
            },
            CommandHelp {
                name: "put",
                group: "WRITING",
                summary: "Put a thing",
                usage: &["put <thing>"],
                details: "",
                options: &[],
                examples: &[],
            },
        ],
        global_hint: "--db, ...",
    };

    /// A command's page carries its usage, details, options and examples
    /// sections, and an empty field drops its section rather than printing a
    /// bare heading.
    #[test]
    fn page_has_its_sections() {
        let show = FIXTURE.page(FIXTURE.lookup("show").unwrap());
        assert!(
            show.starts_with("demo show — Print things\n\nUSAGE:\n    demo show [things...]\n")
        );
        assert!(show.contains("\nShows things.\n"));
        assert!(show.contains("\nOPTIONS:\n    --all             Every thing\n"));
        assert!(show.contains("\nEXAMPLES:\n    demo show --all\n"));
        assert!(show.ends_with("\nOptions every command takes (--db, ...): demo --help\n"));

        let put = FIXTURE.page(FIXTURE.lookup("put").unwrap());
        assert!(!put.contains("OPTIONS:"));
        assert!(!put.contains("EXAMPLES:"));
        assert!(FIXTURE.lookup("nope").is_none());
    }

    /// The overview lists every command under its group, in group order.
    #[test]
    fn overview_groups_commands() {
        let out = FIXTURE.overview(&[("--db <path>", "Where")], "");
        let reading = out.find("READING\n    show").unwrap();
        let writing = out.find("WRITING\n    put").unwrap();
        assert!(reading < writing);
        assert!(out.contains("OPTIONS (every command):\n    --db <path>"));
        assert!(out.ends_with(
            "Run `demo <command> --help` for that command's own options and examples.\n"
        ));
    }

    /// `check` catches a duplicate name and a command filed under no printed
    /// group.
    #[test]
    fn check_catches_bad_tables() {
        assert_eq!(FIXTURE.check(), Ok(()));

        const DUP: Cli = Cli {
            commands: &[
                CommandHelp {
                    name: "x",
                    group: "READING",
                    summary: "",
                    usage: &[],
                    details: "",
                    options: &[],
                    examples: &[],
                },
                CommandHelp {
                    name: "x",
                    group: "READING",
                    summary: "",
                    usage: &[],
                    details: "",
                    options: &[],
                    examples: &[],
                },
            ],
            ..FIXTURE
        };
        assert!(DUP.check().unwrap_err().contains("duplicate"));

        const STRAY: Cli = Cli {
            groups: &["READING"],
            ..FIXTURE
        };
        assert!(STRAY.check().unwrap_err().contains("'put'"));
    }

    /// Option keys yield each flag they spell, whatever placeholder follows,
    /// and a comma inside a placeholder isn't read as a second flag. The keys
    /// are real ones from the agentium and headway tables.
    #[test]
    fn option_names_reads_every_spelling() {
        let names = |key| option_names(key).collect::<Vec<_>>();
        assert_eq!(names("--json"), ["--json"]);
        assert_eq!(names("--prompt-file <p>"), ["--prompt-file"]);
        assert_eq!(names("-i, --ignore-case"), ["-i", "--ignore-case"]);
        assert_eq!(names("-n, --last <n>"), ["-n", "--last"]);
        assert_eq!(names("--role <r[,r…]>"), ["--role"]);
        assert_eq!(names("-n, --count <k>"), ["-n", "--count"]);
    }

    /// A run passes when every flag is global or the command's own, and
    /// otherwise names the first stray flag, the command, and its help page.
    #[test]
    fn check_flags_names_the_first_stray_flag() {
        let show = FIXTURE.lookup("show").unwrap();
        let put = FIXTURE.lookup("put").unwrap();
        let globals = [("-d, --db <path>", "Where")];
        let none: [&str; 0] = [];

        assert_eq!(FIXTURE.check_flags(show, &globals, &none), Ok(()));
        assert_eq!(
            FIXTURE.check_flags(show, &globals, &["--all", "-d", "--db"]),
            Ok(())
        );
        assert_eq!(
            FIXTURE.check_flags(put, &globals, &["--db", "--all", "--x"]),
            Err("`put` doesn't take --all (see `demo put --help`)".to_string())
        );
        // A global's placeholder isn't a flag of its own.
        assert!(FIXTURE.check_flags(put, &globals, &["<path>"]).is_err());
    }

    /// Wrapping breaks at the help width and indents continuation lines.
    #[test]
    fn wrapping_indents_and_fits() {
        let text = "one two three four five six seven eight nine ten eleven twelve \
                    thirteen fourteen fifteen sixteen";
        let out = wrapped(text, 8);
        for line in out.lines() {
            assert!(line.starts_with("        "), "not indented: {line:?}");
            let width = line.chars().count();
            assert!(width <= WIDTH, "too wide ({width}): {line:?}");
        }
        // Em dashes are one column each, not three: a line full of them must
        // still fill out to the wrap width rather than breaking early.
        let dashes = "— ".repeat(40);
        let dashed = wrapped(dashes.trim(), 4);
        let first = dashed.lines().next().unwrap();
        assert!(
            first.chars().count() > WIDTH - 2,
            "wrapped short on em dashes: {first:?}"
        );

        // No word is lost or duplicated.
        assert_eq!(
            out.split_whitespace().collect::<Vec<_>>(),
            text.split_whitespace().collect::<Vec<_>>()
        );
    }

    /// An option block lines descriptions up under the same column, and a flag
    /// too wide for it drops its description to the next line instead.
    #[test]
    fn option_block_alignment() {
        let out = options_block(&[("--col <c>", "a column"), ("-n, --count <k>", "a cap")]);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        for (line, (flag, text)) in lines
            .iter()
            .zip([("--col <c>", "a column"), ("-n, --count <k>", "a cap")])
        {
            assert!(line.starts_with(&format!("    {flag}")), "{line:?}");
            assert_eq!(
                &line[NAME_COL + 4..],
                text,
                "description off column: {line:?}"
            );
        }

        // A flag wider than the column keeps the block aligned by dropping its
        // description to the next line.
        let wide = options_block(&[("--a-very-long-flag-name <value>", "a description")]);
        let lines: Vec<&str> = wide.lines().collect();
        assert_eq!(lines[0], "    --a-very-long-flag-name <value>");
        assert_eq!(lines[1], format!("{:NAME_COL$}    a description", ""));
    }
}
