//! The tools behind a spec's `Web` and `SubAgent` toolsets.
//!
//! These aren't app tools: no `nd` command runs them, so the host's tool
//! registry doesn't have them. They are declared here, once, and
//! [`Catalog::build`](super::Catalog::build) offers them to a spec that
//! names their toolset. The host runs them (Dave's native loop: a fetch over
//! HTTP, a child loop for the sub-agent).
//!
//! Every one is [`Capability::Egress`]: a search query, a URL and a question
//! to a researcher all carry text out of the agent's context. So a spec that
//! wants one to run unasked names it under `widen`, as the researcher does
//! for its own web tools, and the butler's questions to its researcher stay
//! gated.
//!
//! ## Why the sub-agent is always a researcher
//!
//! A sub-agent exists to keep egress away from private data, so the child
//! spec must hold none ([`AgentSpec::check_sub_agent`](super::AgentSpec::check_sub_agent)):
//! no Notedeck toolset, no Coding toolset, and no sub-agent of its own. What
//! is left is the Web toolset, so the call tool is named and described as
//! asking a web researcher.

use crate::tool::{Capability, ToolArg, ToolArgType, ToolSpec};

/// The Web toolset's search tool.
pub const WEB_SEARCH: &str = "web_search";

/// The Web toolset's fetch tool.
pub const WEB_FETCH: &str = "web_fetch";

/// The tool a `SubAgent` toolset offers: ask the child spec a question, get
/// its summary back.
pub const ASK_RESEARCHER: &str = "ask_researcher";

/// Search the web: a query in, the top results' titles, URLs and snippets
/// out.
pub fn web_search() -> ToolSpec {
    ToolSpec::new(
        WEB_SEARCH,
        "Search the web. Returns the top results, each with its title, URL and a snippet. \
         Fetch a result's URL with web_fetch to read the page.",
        vec![
            ToolArg::new("query", ToolArgType::String, "What to search for.").required(true),
            ToolArg::new(
                "count",
                ToolArgType::Integer,
                "How many results to return, 1 to 10 (default 8).",
            ),
        ],
    )
    .capability(Capability::Egress)
}

/// Fetch a page: a public http(s) URL in, its readable text out.
pub fn web_fetch() -> ToolSpec {
    ToolSpec::new(
        WEB_FETCH,
        "Fetch a public http or https URL and return its text, with an HTML page reduced \
         to its readable text. Long pages are cut short.",
        vec![ToolArg::new("url", ToolArgType::String, "The URL to fetch.").required(true)],
    )
    .capability(Capability::Egress)
}

/// Ask the spec's research sub-agent a question.
pub fn ask_researcher() -> ToolSpec {
    ToolSpec::new(
        ASK_RESEARCHER,
        "Ask a research sub-agent a question about the outside world: a market, a \
         competitor, a product, a price. It searches and reads the web in a separate \
         context that holds none of this conversation, and answers with a summary. Only \
         the question leaves this conversation, so put in it what the research needs and \
         nothing private.",
        vec![ToolArg::new(
            "question",
            ToolArgType::String,
            "The research question, self-contained: the sub-agent sees nothing else.",
        )
        .required(true)],
    )
    .capability(Capability::Egress)
}

/// The Web toolset's tools.
pub fn web_tools() -> [ToolSpec; 2] {
    [web_search(), web_fetch()]
}
