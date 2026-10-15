//! How `nd tools` prints the tool list it fetched: grouped by category, one
//! line a tool, or one tool's whole page.

use crate::schema;
use cli_term::help::wrapped;
use notedeck_ipc::{ToolInfo, ToolKind};
use regex::Regex;

/// The heading tools without a category are listed under, last.
const UNCATEGORIZED: &str = "Other";

/// The tools `pattern` matches in their name, title or description; all of
/// them without one.
pub fn matching<'a>(tools: &'a [ToolInfo], pattern: Option<&Regex>) -> Vec<&'a ToolInfo> {
    tools
        .iter()
        .filter(|t| {
            pattern.is_none_or(|p| {
                p.is_match(&t.name) || p.is_match(&t.title) || p.is_match(&t.description)
            })
        })
        .collect()
}

/// The grouped list: a heading per category, alphabetical with
/// [`UNCATEGORIZED`] last, then each of its tools in the order notedeck sent
/// them — name, marks (see [`marks`]), and title with the description's first
/// sentence.
pub fn overview(tools: &[&ToolInfo]) -> String {
    let mut categories: Vec<&str> = tools.iter().map(|t| category(t)).collect();
    categories.sort_by_key(|c| (*c == UNCATEGORIZED, *c));
    categories.dedup();

    let width = tools.iter().map(|t| t.name.len()).max().unwrap_or(0) + 2;
    let mut out = String::new();
    for (i, heading) in categories.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(heading);
        out.push('\n');
        for tool in tools.iter().filter(|t| category(t) == *heading) {
            out.push_str(&format!(
                "    {:<width$}{:<9}{}: {}\n",
                tool.name,
                marks(tool),
                tool.title,
                first_sentence(&tool.description),
            ));
        }
    }
    out
}

/// One tool's page: name and title, its marks and category, the whole
/// description, and its arguments.
pub fn page(tool: &ToolInfo) -> String {
    let mut out = format!("{} — {}\n", tool.name, tool.title);
    out.push_str(&format!(
        "{}{}\n\n",
        marks(tool),
        tool.category
            .as_deref()
            .map(|c| format!(", in {c}"))
            .unwrap_or_default()
    ));
    out.push_str(&wrapped(&tool.description, 0));
    out.push_str("\nARGS:\n");
    out.push_str(&schema::arg_lines(&tool.input_schema));
    out
}

/// The heading `tool` is listed under.
fn category(tool: &ToolInfo) -> &str {
    tool.category.as_deref().unwrap_or(UNCATEGORIZED)
}

/// `data` or `ui`, then ` ro` for a read-only tool.
fn marks(tool: &ToolInfo) -> String {
    let kind = match tool.kind {
        ToolKind::Data => "data",
        ToolKind::Ui => "ui",
    };
    if tool.read_only {
        format!("{kind} ro")
    } else {
        kind.to_owned()
    }
}

/// The description up to its first sentence break or line end. A break is a
/// period, a space and a capital, so an "e.g. a" mid-sentence doesn't end it.
fn first_sentence(description: &str) -> &str {
    let line = description.lines().next().unwrap_or("");
    let end = line
        .match_indices(". ")
        .map(|(i, _)| i)
        .find(|&i| line[i + 2..].chars().next().is_some_and(char::is_uppercase));
    match end {
        Some(end) => &line[..=end],
        None => line,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(name: &str, category: Option<&str>, kind: ToolKind, read_only: bool) -> ToolInfo {
        ToolInfo {
            name: name.to_owned(),
            title: format!("{name} title"),
            description: format!("Does {name}. More detail."),
            category: category.map(str::to_owned),
            kind,
            read_only,
            input_schema: json!({"type": "object", "properties": {}}),
        }
    }

    #[test]
    fn the_overview_groups_by_category_with_uncategorized_last() {
        let tools = [
            tool("b_tool", Some("Headway"), ToolKind::Ui, false),
            tool("loose", None, ToolKind::Data, true),
            tool("a_tool", Some("Chrome"), ToolKind::Data, false),
        ];
        let all = matching(&tools, None);
        assert_eq!(
            overview(&all),
            "Chrome\n    a_tool  data     a_tool title: Does a_tool.\n\n\
             Headway\n    b_tool  ui       b_tool title: Does b_tool.\n\n\
             Other\n    loose   data ro  loose title: Does loose.\n"
        );
    }

    #[test]
    fn a_pattern_matches_name_title_or_description() {
        let tools = [
            tool("headway_open", None, ToolKind::Ui, false),
            tool("open", None, ToolKind::Ui, false),
        ];
        let pattern = cli_term::compile_pattern("Headway", cli_term::CaseMode::Smart).unwrap();
        let hits = matching(&tools, Some(&pattern));
        assert!(
            hits.is_empty(),
            "smart case: capital H matches case-sensitively"
        );
        let pattern = cli_term::compile_pattern("headway", cli_term::CaseMode::Smart).unwrap();
        let names: Vec<&str> = matching(&tools, Some(&pattern))
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(names, ["headway_open"]);
    }

    #[test]
    fn the_first_sentence_runs_past_an_abbreviation() {
        assert_eq!(
            first_sentence("Open the issue, e.g. a card. Then more."),
            "Open the issue, e.g. a card."
        );
        assert_eq!(first_sentence("No break here"), "No break here");
    }

    #[test]
    fn a_page_shows_marks_description_and_args() {
        let mut open = tool("open", Some("Chrome"), ToolKind::Ui, false);
        open.input_schema = json!({
            "type": "object",
            "properties": {"reference": {"type": "string", "description": "What to open"}},
            "required": ["reference"],
        });
        assert_eq!(
            page(&open),
            "open — open title\nui, in Chrome\n\n\
             Does open. More detail.\n\n\
             ARGS:\n    reference  string, required  What to open\n"
        );
    }
}
