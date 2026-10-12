//! The Outputs card's state: what this session produced.
//!
//! Two sources, read two ways:
//!
//! - **Commits** whose `Agentium:` trailer names the session. The trailer is
//!   the only record that ties a commit to a session, so the card asks git:
//!   `git log --all --grep='^Agentium: agentium:<word-id>$'` in the session's
//!   cwd, off the UI thread ([`CommitsState`]), again whenever the chat has
//!   grown since the last read, at most every few seconds.
//! - **Cards and pages** mentioned in the transcript: `headway:<board>/<word-id>`
//!   refs (by the headway crate's own inline grammar,
//!   [`headway::wordid::find_ref`]) and published pages — a `…/share/*.html`
//!   explainer or a claude.ai artifact. Scanned from the chat itself
//!   ([`mentions`]), so a restored or remote session lists them too, and only
//!   when the chat's text has grown ([`MentionsKey`]), at most once a second.

use super::LabelMemo;
use crate::messages::ExecutedTool;
use crate::tools::ToolResponses;
use crate::Message;
pub use agentium_core::inspector_snapshot::{Commit, CommitList};
use poll_promise::Promise;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Least time between two `git log` reads.
const COMMITS_REFRESH: Duration = Duration::from_secs(5);

/// Least time between two scans of the transcript while it grows.
const MENTIONS_RESCAN: Duration = Duration::from_secs(1);

/// The most commits the card lists.
pub const COMMITS_CAP: usize = 50;

/// The most cards, and the most pages, the card lists; the rest are counted.
pub const MENTIONS_CAP: usize = 30;

/// The `git log` that lists `session_ref`'s commits in the repo at `cwd`, on
/// every ref, newest first, a rebased copy collapsed into the newest commit
/// with its subject.
pub fn list_commits(cwd: &Path, session_ref: &str) -> CommitList {
    // git would fail to start in a missing directory, which reads as git
    // itself missing.
    if !cwd.is_dir() {
        return CommitList::NotARepo;
    }
    let mut cmd = std::process::Command::new("git");
    cmd.args(["-c", "color.ui=never", "log", "--all", "--date-order"])
        .arg(format!("--max-count={}", COMMITS_CAP * 2))
        .arg("--format=%H%x09%h%x09%s")
        .arg(format!("--grep=^Agentium: {}$", session_ref))
        .current_dir(cwd);
    crate::git_status::configure_git_command(&mut cmd);

    let output = match cmd.output() {
        Ok(output) => output,
        Err(err) => return CommitList::Failed(err.to_string()),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not a git repository") {
            return CommitList::NotARepo;
        }
        return CommitList::Failed(stderr.trim().to_owned());
    }
    CommitList::Commits(parse_log(&String::from_utf8_lossy(&output.stdout)))
}

/// `hash \t short \t subject` lines, newest first. A rebase leaves the old
/// copy reachable from wherever it was landed, so only the newest commit with
/// each subject is kept.
fn parse_log(stdout: &str) -> Vec<Commit> {
    let mut commits: Vec<Commit> = Vec::new();
    for line in stdout.lines() {
        let mut fields = line.splitn(3, '\t');
        let (Some(hash), Some(short), Some(subject)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if commits.iter().any(|c| c.subject == subject) {
            continue;
        }
        commits.push(Commit {
            hash: hash.to_owned(),
            short: short.to_owned(),
            subject: subject.to_owned(),
        });
        if commits.len() == COMMITS_CAP {
            break;
        }
    }
    commits
}

/// The repo and session a commit list is of, so a session that changes
/// either is listed afresh.
#[derive(Debug, PartialEq, Eq)]
struct ListedFor {
    cwd: PathBuf,
    session_ref: String,
}

/// The Outputs card's commits: the latest `git log` answer and the read in
/// flight. See the module docs.
#[derive(Default)]
pub struct CommitsState {
    listed_for: Option<ListedFor>,
    /// The latest answer; `None` before the first read lands.
    list: Option<CommitList>,
    /// Bumped each time a read lands, so strings built from it rebuild.
    generation: u64,
    reading: Option<Promise<CommitList>>,
    /// The chat's row count when the last read started.
    read_rows: Option<usize>,
    read_at: Option<Instant>,
}

impl std::fmt::Debug for CommitsState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommitsState")
            .field("listed_for", &self.listed_for)
            .field("list", &self.list)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl CommitsState {
    /// Land a finished read of `session_ref`'s commits in the repo at `cwd`,
    /// and start one off the UI thread when the chat has grown to
    /// `chat_rows` since the last began and that was a few seconds ago. A
    /// commit lands in the chat as a tool result, so a grown chat is when
    /// there may be a new one. [`Self::list`] has the answer.
    pub fn poll(&mut self, cwd: &Path, session_ref: &str, chat_rows: usize, now: Instant) {
        let same = self
            .listed_for
            .as_ref()
            .is_some_and(|l| l.cwd == cwd && l.session_ref == session_ref);
        if !same {
            self.listed_for = Some(ListedFor {
                cwd: cwd.to_path_buf(),
                session_ref: session_ref.to_owned(),
            });
            self.list = None;
            self.reading = None;
            self.read_rows = None;
            self.read_at = None;
            self.generation += 1;
        }

        if let Some(list) = self.reading.as_mut().and_then(|p| p.ready_mut()) {
            // Only an answer that differs counts as new, so an unchanged
            // log doesn't mark the session's snapshot stale.
            if self.list.as_ref() != Some(list) {
                self.list = Some(std::mem::replace(list, CommitList::Commits(Vec::new())));
                self.generation += 1;
            }
            self.reading = None;
        }
        if self.reading.is_none() && self.wants_read(chat_rows, now) {
            self.read_rows = Some(chat_rows);
            self.read_at = Some(now);
            // A missing directory answers at once, so a session whose cwd is
            // gone (or a test's) never shows a read in flight. One stat, at
            // most every few seconds.
            if !cwd.is_dir() {
                if self.list != Some(CommitList::NotARepo) {
                    self.list = Some(CommitList::NotARepo);
                    self.generation += 1;
                }
                return;
            }
            let cwd = cwd.to_path_buf();
            let session_ref = session_ref.to_owned();
            self.reading = Some(Promise::spawn_thread("dave-session-commits", move || {
                list_commits(&cwd, &session_ref)
            }));
        }
    }

    /// Show a host's answer, for a remote session: its repo can't be read
    /// from here, so [`Self::poll`] is never called for one.
    pub fn set_remote(&mut self, list: Option<CommitList>) {
        if self.list == list {
            return;
        }
        self.list = list;
        self.generation += 1;
    }

    /// The latest `git log` answer; `None` until the first read lands.
    pub fn list(&self) -> Option<&CommitList> {
        self.list.as_ref()
    }

    /// Never read, or the chat grew since the last read and that was long
    /// enough ago.
    fn wants_read(&self, chat_rows: usize, now: Instant) -> bool {
        let Some(at) = self.read_at else {
            return true;
        };
        self.read_rows != Some(chat_rows) && now.duration_since(at) >= COMMITS_REFRESH
    }

    /// Bumped each time a read lands.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// A read is waiting on the chat's growth or on git, so the card should
    /// draw again soon to pick it up.
    pub fn pending(&self, chat_rows: usize) -> bool {
        self.reading.is_some() || self.read_rows != Some(chat_rows)
    }

    /// How soon to draw again while [`Self::pending`].
    pub fn refresh_after() -> Duration {
        COMMITS_REFRESH
    }
}

/// The next published page in `text`: a `https://` URL to a `…/share/….html`
/// page (what `sharefile` publishes an explainer as) or a claude.ai artifact.
/// The URL ends at whitespace or a delimiter prose wraps it in, and trailing
/// sentence punctuation is not part of it.
pub fn find_page(text: &str) -> Option<Range<usize>> {
    const SCHEME: &str = "https://";
    let mut search = 0;
    while let Some(rel) = text[search..].find(SCHEME) {
        let start = search + rel;
        let rest = &text[start..];
        let len = rest
            .find(|c: char| {
                c.is_whitespace()
                    || matches!(
                        c,
                        '"' | '\''
                            | '<'
                            | '>'
                            | '('
                            | ')'
                            | '['
                            | ']'
                            | '{'
                            | '}'
                            | '`'
                            | '|'
                            | ','
                    )
            })
            .unwrap_or(rest.len());
        search = start + len;
        let url = rest[..len].trim_end_matches(['.', ':', ';', '!', '?']);
        if is_page(&url[SCHEME.len()..]) {
            return Some(start..start + url.len());
        }
    }
    None
}

/// A URL after its scheme names a published page: a shared `.html` file, or
/// a claude.ai artifact.
fn is_page(rest: &str) -> bool {
    let Some((host, path)) = rest.split_once('/') else {
        return false;
    };
    let shared_html =
        path.ends_with(".html") && (path.starts_with("share/") || path.contains("/share/"));
    let artifact = host == "claude.ai"
        && (path.starts_with("artifact/") || path.starts_with("code/artifact/"));
    shared_html || artifact
}

/// One card ref or page URL the transcript mentions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mention {
    /// The ref (`headway:dave/maple-river-canyon`) or URL, as written.
    pub text: String,
    /// The chat row it was last mentioned in.
    pub last_row: usize,
}

/// One kind of mention, each once, most recently mentioned first, at most
/// [`MENTIONS_CAP`] of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MentionGroup {
    pub list: Vec<Mention>,
    /// How many distinct ones there are, listed or not.
    pub total: usize,
}

/// Every card ref and page URL a transcript mentions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mentions {
    pub cards: MentionGroup,
    pub pages: MentionGroup,
}

/// Scan `chat` for card refs and page URLs. See [`Mentions`].
pub fn mentions(chat: &[Message]) -> Mentions {
    let mut cards: HashMap<&str, usize> = HashMap::new();
    let mut pages: HashMap<&str, usize> = HashMap::new();
    for (row, message) in chat.iter().enumerate() {
        for text in message_texts(message) {
            for found in matches(text, headway::wordid::find_ref) {
                cards.insert(found, row);
            }
            for found in matches(text, find_page) {
                pages.insert(found, row);
            }
        }
    }
    Mentions {
        cards: newest_first(cards),
        pages: newest_first(pages),
    }
}

/// Each match `find` makes in `text`, left to right.
fn matches(text: &str, find: fn(&str) -> Option<Range<usize>>) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        let range = find(rest)?;
        let found = &rest[range.clone()];
        rest = &rest[range.end..];
        Some(found)
    })
}

/// The mentions ordered by their last row, newest first (ties by text), cut
/// to [`MENTIONS_CAP`], and how many there were.
fn newest_first(by_text: HashMap<&str, usize>) -> MentionGroup {
    let total = by_text.len();
    let mut list: Vec<Mention> = by_text
        .into_iter()
        .map(|(text, last_row)| Mention {
            text: text.to_owned(),
            last_row,
        })
        .collect();
    list.sort_by(|a, b| b.last_row.cmp(&a.last_row).then(a.text.cmp(&b.text)));
    list.truncate(MENTIONS_CAP);
    MentionGroup { list, total }
}

/// The text a chat row holds that could mention a card or a page: what the
/// user and the assistant wrote, each tool's summary and output, and each
/// subagent's description, output and tools.
fn message_texts(message: &Message) -> impl Iterator<Item = &str> {
    let own: [Option<&str>; 2] = match message {
        Message::User(msg) => [Some(&msg.text), None],
        Message::Assistant(msg) => [Some(msg.text()), None],
        Message::ToolRunning(running) => [Some(&running.summary), None],
        Message::ToolResponse(response) => match response.responses() {
            ToolResponses::ExecutedTool(tool) => [Some(&tool.summary), tool.output.as_deref()],
            _ => [None, None],
        },
        Message::Subagent(info) => [Some(&info.description), Some(&info.output)],
        _ => [None, None],
    };
    let tools: &[ExecutedTool] = match message {
        Message::Subagent(info) => &info.tool_results,
        _ => &[],
    };
    own.into_iter().flatten().chain(
        tools
            .iter()
            .flat_map(|tool| std::iter::once(tool.summary.as_str()).chain(tool.output.as_deref())),
    )
}

/// What a scan of the transcript covered: its rows and the bytes of text
/// they hold. Either moves when a row lands, streams or resolves in place,
/// and counting them allocates nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MentionsKey {
    pub rows: usize,
    pub bytes: usize,
}

impl MentionsKey {
    /// `chat`'s key.
    pub fn of(chat: &[Message]) -> Self {
        Self {
            rows: chat.len(),
            bytes: chat
                .iter()
                .flat_map(message_texts)
                .map(str::len)
                .sum::<usize>(),
        }
    }
}

/// The transcript's mentions, rescanned when [`MentionsKey`] moves but no
/// more than once a second while the transcript streams.
#[derive(Debug, Default)]
pub struct MentionsMemo {
    scanned: Option<MentionsKey>,
    scanned_at: Option<Instant>,
    mentions: Mentions,
    /// Bumped each rescan, so strings built from the mentions rebuild.
    generation: u64,
}

impl MentionsMemo {
    /// Rescan `chat` if it has changed and the last scan is old enough.
    /// [`Self::mentions`] has the result.
    pub fn refresh(&mut self, chat: &[Message], now: Instant) {
        let key = MentionsKey::of(chat);
        let due = self
            .scanned_at
            .is_none_or(|at| now.duration_since(at) >= MENTIONS_RESCAN);
        if self.scanned != Some(key) && due {
            self.mentions = mentions(chat);
            self.scanned = Some(key);
            self.scanned_at = Some(now);
            self.generation += 1;
        }
    }

    /// The mentions as last scanned.
    pub fn mentions(&self) -> &Mentions {
        &self.mentions
    }

    /// The scan is behind the chat, waiting out [`MENTIONS_RESCAN`].
    pub fn stale(&self, chat: &[Message]) -> bool {
        self.scanned != Some(MentionsKey::of(chat))
    }

    /// Bumped each rescan.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// How soon a stale scan is due.
    pub fn rescan_after() -> Duration {
        MENTIONS_RESCAN
    }
}

/// The Outputs card's state: the session's commits, the transcript's
/// mentions, and the card's strings.
#[derive(Debug, Default)]
pub struct OutputsState {
    pub commits: CommitsState,
    pub mentions: MentionsMemo,
    /// The group headings, keyed by the commits' and mentions' generations.
    pub labels: LabelMemo<OutputsKey, OutputLabels>,
}

/// What the card's headings were built from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputsKey {
    pub commits: u64,
    pub mentions: u64,
}

/// The Outputs card's strings.
#[derive(Debug, Default)]
pub struct OutputLabels {
    /// "Commits · 3"
    pub commits: String,
    /// "Cards · 5"
    pub cards: String,
    /// "Pages · 1"
    pub pages: String,
    /// What an empty cards or pages group shows.
    pub no_cards: String,
    pub no_pages: String,
    /// "4 more", for a group past its cap.
    pub more_cards: Option<String>,
    pub more_pages: Option<String>,
}

/// Run git in `dir` for a test fixture repo, as a fixed author at a fixed
/// time, so a commit's hash is the same in every run.
#[cfg(test)]
pub(crate) fn test_git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .env("GIT_AUTHOR_DATE", "1893456000 +0000")
        .env("GIT_COMMITTER_DATE", "1893456000 +0000")
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{AssistantMessage, RunningTool, SubagentInfo, SubagentStatus};
    use crate::tools::ToolResponse;

    fn tool(summary: &str, output: Option<&str>) -> ExecutedTool {
        ExecutedTool {
            tool_name: "Bash".into(),
            summary: summary.into(),
            output: output.map(str::to_owned),
            parent_task_id: None,
            file_update: None,
            tool_use_id: None,
        }
    }

    fn user(text: &str) -> Message {
        Message::User(crate::messages::UserMessage::new(text, Vec::new()))
    }

    fn assistant(text: &str) -> Message {
        let mut msg = AssistantMessage::new();
        msg.push_token(text);
        Message::Assistant(msg)
    }

    /// Refs and pages come from every kind of row: the user's text, the
    /// assistant's, a tool's command and output, a running tool, and a
    /// subagent's tools. Each is listed once, most recently mentioned first.
    #[test]
    fn mentions_come_from_every_row_kind_newest_first() {
        let chat = vec![
            user("work headway:dave/shrimp-guide-poet please"),
            Message::ToolRunning(RunningTool {
                tool_use_id: "t1".into(),
                tool_name: "Bash".into(),
                summary: "headway move headway:dave/one-two-three --col done".into(),
            }),
            Message::ToolResponse(ToolResponse::executed_tool(tool(
                "sharefile page.html",
                Some("https://media.example.com/episodes/misc/share/outputs-card.html\n"),
            ))),
            Message::Subagent(SubagentInfo {
                task_id: "s1".into(),
                description: "look around".into(),
                subagent_type: "Explore".into(),
                status: SubagentStatus::Completed,
                output: String::new(),
                max_output_size: 1000,
                tool_results: vec![tool("headway show headway:dave/four-five-six", None)],
                background: false,
                agent_id: None,
                transcript: Default::default(),
            }),
            assistant(
                "Done: headway:dave/shrimp-guide-poet, see \
                 (https://claude.ai/code/artifact/3f0e-uuid). Not a page: \
                 https://example.com/share/x.png or https://example.com/index.html.",
            ),
        ];

        let found = mentions(&chat);
        let cards: Vec<_> = found
            .cards
            .list
            .iter()
            .map(|m| (m.text.as_str(), m.last_row))
            .collect();
        assert_eq!(
            cards,
            [
                ("headway:dave/shrimp-guide-poet", 4),
                ("headway:dave/four-five-six", 3),
                ("headway:dave/one-two-three", 1),
            ]
        );
        let pages: Vec<_> = found.pages.list.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(
            pages,
            [
                "https://claude.ai/code/artifact/3f0e-uuid",
                "https://media.example.com/episodes/misc/share/outputs-card.html",
            ]
        );
        assert_eq!(found.cards.total, 3);
    }

    #[test]
    fn page_urls_end_where_prose_does() {
        let s = "published: https://h.example/share/a-b.html. Next";
        assert_eq!(
            &s[find_page(s).unwrap()],
            "https://h.example/share/a-b.html"
        );
        let s = "[page](https://h.example/x/share/c.html)";
        assert_eq!(
            &s[find_page(s).unwrap()],
            "https://h.example/x/share/c.html"
        );
        assert_eq!(find_page("https://h.example/shared/c.html"), None);
        assert_eq!(find_page("https://claude.ai/chat/abc"), None);
        assert_eq!(find_page("https://"), None);
    }

    /// The key moves when text streams into the last row, not only when a
    /// row lands, and the memo rescans no more than once a second.
    #[test]
    fn the_memo_rescans_on_new_text_at_most_once_a_second() {
        let mut chat = vec![user("headway:dave/one-two-three")];
        let mut memo = MentionsMemo::default();
        let t0 = Instant::now();
        memo.refresh(&chat, t0);
        assert_eq!(memo.mentions().cards.list.len(), 1);

        chat.push(assistant("and headway:dave/four-five-six"));
        memo.refresh(&chat, t0);
        assert_eq!(memo.mentions().cards.list.len(), 1, "too soon");
        assert!(memo.stale(&chat));
        memo.refresh(&chat, t0 + MENTIONS_RESCAN);
        assert_eq!(memo.mentions().cards.list.len(), 2);
        assert!(!memo.stale(&chat));

        let before = MentionsKey::of(&chat);
        if let Some(Message::Assistant(msg)) = chat.last_mut() {
            msg.push_token(" more");
        }
        assert_ne!(MentionsKey::of(&chat), before);
    }

    #[test]
    fn a_rebased_copy_collapses_into_the_newest() {
        let log = "aaa\taa\tdave: second\nbbb\tbb\tdave: first\nccc\tcc\tdave: second\n";
        let commits = parse_log(log);
        assert_eq!(
            commits.iter().map(|c| c.hash.as_str()).collect::<Vec<_>>(),
            ["aaa", "bbb"]
        );
        assert_eq!(commits[0].short, "aa");
    }

    /// In a repo with commits for two sessions, the lister finds only the
    /// ones whose trailer names this session, on any branch.
    #[test]
    fn lists_only_this_sessions_commits() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path();
        test_git(dir, &["init", "-q", "-b", "main"]);
        test_git(
            dir,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "mine on main\n\nChangelog-None:\nAgentium: agentium:maple-river-canyon",
            ],
        );
        test_git(
            dir,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "someone else's\n\nAgentium: agentium:maple-river-canyons",
            ],
        );
        test_git(dir, &["checkout", "-q", "-b", "side"]);
        test_git(
            dir,
            &[
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "mine on a branch\n\nAgentium: agentium:maple-river-canyon",
            ],
        );
        test_git(dir, &["checkout", "-q", "main"]);

        let CommitList::Commits(commits) = list_commits(dir, "agentium:maple-river-canyon") else {
            panic!("expected commits");
        };
        let subjects: Vec<_> = commits.iter().map(|c| c.subject.as_str()).collect();
        assert_eq!(subjects, ["mine on a branch", "mine on main"]);
        assert_eq!(commits[0].hash.len(), 40);

        let outside = tempfile::tempdir().unwrap();
        assert_eq!(
            list_commits(outside.path(), "agentium:maple-river-canyon"),
            CommitList::NotARepo
        );
    }

    /// Polling reads off-thread and reads again only once the chat grows
    /// and the refresh interval has passed.
    #[test]
    fn poll_rereads_when_the_chat_grows() {
        let outside = tempfile::tempdir().unwrap();
        let mut state = CommitsState::default();
        let t0 = Instant::now();
        state.poll(outside.path(), "agentium:a-b-c", 3, t0);
        assert!(state.list().is_none());
        let deadline = Instant::now() + Duration::from_secs(10);
        while state.list().is_none() {
            state.poll(outside.path(), "agentium:a-b-c", 3, t0);
            assert!(Instant::now() < deadline, "the read never landed");
            std::thread::yield_now();
        }
        assert!(!state.pending(3));
        assert!(!state.wants_read(3, t0 + COMMITS_REFRESH));
        assert!(state.pending(4));
        assert!(!state.wants_read(4, t0));
        assert!(state.wants_read(4, t0 + COMMITS_REFRESH));

        // A missing directory answers on the poll itself.
        let mut state = CommitsState::default();
        state.poll(&outside.path().join("gone"), "agentium:a-b-c", 0, t0);
        assert_eq!(state.list(), Some(&CommitList::NotARepo));
        assert!(!state.pending(0));
    }
}
