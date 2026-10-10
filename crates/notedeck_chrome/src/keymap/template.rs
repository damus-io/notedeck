//! `{name}` placeholders in a macro step's args, filled in as the step runs.
//!
//! A placeholder is `{` a name, then any `.field` (or `.0` for an array
//! index), then `}`, inside a string anywhere in the args: `"{card.issue}"`,
//! `"x-{prev.ref}"`. The names a step can use are [`CURRENT`] (the current
//! ref of the app in front), [`PREV`] (the previous step's result) and any
//! earlier step's `as`. Anything else in braces that isn't a placeholder's
//! shape, such as `{a b}` or `{}`, is left as written.
//!
//! A string that is exactly one placeholder takes the value itself, so a
//! number or an object keeps its type; a placeholder inside longer text is
//! written into it, a string as is and anything else as JSON.

use std::collections::BTreeMap;

use serde_json::Value;

use super::CURRENT_NAME;

/// The name of the previous step's result.
pub const PREV: &str = "prev";

/// The names a step can use without an earlier `as` binding them.
pub const RESERVED: [&str; 2] = [CURRENT_NAME, PREV];

/// One `{name.path}` in a string.
#[derive(Debug, PartialEq, Eq)]
pub struct Placeholder<'a> {
    /// The name, e.g. `card`.
    pub name: &'a str,
    /// The `.`-separated path after it, e.g. `issue`; empty for none.
    pub path: &'a str,
}

/// What a running macro has to fill its next step's placeholders from.
#[derive(Clone, Copy)]
pub struct Vars<'a> {
    /// The current ref of the app in front, as a string, for
    /// [`CURRENT`](super::CURRENT).
    pub current: Option<&'a Value>,
    /// The previous step's result, for [`PREV`].
    pub prev: Option<&'a Value>,
    /// The results of the steps with an `as`, by that name.
    pub bound: &'a BTreeMap<String, Value>,
}

/// The placeholders in `text`, in order.
pub fn placeholders(text: &str) -> impl Iterator<Item = Placeholder<'_>> {
    let mut rest = text;
    std::iter::from_fn(move || loop {
        let open = rest.find('{')?;
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            rest = "";
            return None;
        };
        let inner = &after[..close];
        match parse_placeholder(inner) {
            Some(found) => {
                rest = &after[close + 1..];
                return Some(found);
            }
            // Not a placeholder: look again from just past this `{`.
            None => rest = after,
        }
    })
}

/// `inner`, the text between a `{` and its `}`, as a placeholder: a name of
/// letters, digits, `_` and `-` starting with a letter or `_`, then any
/// `.segment`s of the same characters.
fn parse_placeholder(inner: &str) -> Option<Placeholder<'_>> {
    let (name, path) = match inner.split_once('.') {
        Some(split) => split,
        None => (inner, ""),
    };
    let word = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    };
    let starts_ok = name
        .chars()
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_');
    if !word(name) || !starts_ok {
        return None;
    }
    if !path.is_empty() && !path.split('.').all(word) {
        return None;
    }
    Some(Placeholder { name, path })
}

/// Every placeholder in every string of `args`, however deep, passed to `f`.
pub fn each_placeholder<'a>(args: &'a Value, f: &mut impl FnMut(Placeholder<'a>)) {
    match args {
        Value::String(text) => placeholders(text).for_each(f),
        Value::Array(items) => items.iter().for_each(|item| each_placeholder(item, f)),
        Value::Object(fields) => fields.values().for_each(|value| each_placeholder(value, f)),
        _ => {}
    }
}

/// `args` with every placeholder filled from `vars`. Fails, naming the
/// placeholder, when its name has no value or its path leads nowhere.
pub fn fill(args: &Value, vars: Vars<'_>) -> Result<Value, String> {
    match args {
        Value::String(text) => fill_string(text, vars),
        Value::Array(items) => items
            .iter()
            .map(|item| fill(item, vars))
            .collect::<Result<_, _>>()
            .map(Value::Array),
        Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| fill(value, vars).map(|v| (key.clone(), v)))
            .collect::<Result<_, _>>()
            .map(Value::Object),
        other => Ok(other.clone()),
    }
}

/// `text` with its placeholders filled: the value itself when `text` is
/// exactly one placeholder, else the text with each one written in.
fn fill_string(text: &str, vars: Vars<'_>) -> Result<Value, String> {
    if let Some(whole) = text
        .strip_prefix('{')
        .and_then(|t| t.strip_suffix('}'))
        .and_then(parse_placeholder)
    {
        return lookup(&whole, vars).cloned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            break;
        };
        let Some(found) = parse_placeholder(&after[..close]) else {
            out.push_str(&rest[..=open]);
            rest = after;
            continue;
        };
        out.push_str(&rest[..open]);
        match lookup(&found, vars)? {
            Value::String(s) => out.push_str(s),
            other => out.push_str(&other.to_string()),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Ok(Value::String(out))
}

/// The value `found` names in `vars`.
fn lookup<'v>(found: &Placeholder<'_>, vars: Vars<'v>) -> Result<&'v Value, String> {
    let shown = || match found.path {
        "" => format!("{{{}}}", found.name),
        path => format!("{{{}.{path}}}", found.name),
    };
    let mut value = match found.name {
        CURRENT_NAME => vars
            .current
            .ok_or_else(|| format!("{}: the app in front has no current ref", shown()))?,
        PREV => vars
            .prev
            .ok_or_else(|| format!("{}: there is no previous step", shown()))?,
        name => vars
            .bound
            .get(name)
            .ok_or_else(|| format!("{}: no earlier step is bound as '{name}'", shown()))?,
    };
    for segment in found.path.split('.').filter(|s| !s.is_empty()) {
        let next = match value {
            Value::Object(fields) => fields.get(segment),
            Value::Array(items) => segment.parse::<usize>().ok().and_then(|i| items.get(i)),
            _ => None,
        };
        value = next.ok_or_else(|| format!("{}: no '{segment}' in {value}", shown()))?;
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Each placeholder in `text`, as `name` or `name.path`.
    fn found(text: &str) -> Vec<String> {
        placeholders(text)
            .map(|p| match p.path {
                "" => p.name.to_owned(),
                path => format!("{}.{path}", p.name),
            })
            .collect()
    }

    #[test]
    fn placeholders_are_found_and_other_braces_left_alone() {
        assert_eq!(found("{card.issue}"), ["card.issue"]);
        assert_eq!(found("x-{prev.ref}-{a}"), ["prev.ref", "a"]);
        assert_eq!(found("{items.0.id}"), ["items.0.id"]);
        assert!(found("{} {a b} {.x} {1a} { a } plain").is_empty());
        assert_eq!(found("{{x}}"), ["x"], "the inner one");
        assert!(found("{unclosed").is_empty());
    }

    /// A whole-string placeholder keeps the value's type; one inside text is
    /// written in, a string as is, anything else as JSON.
    #[test]
    fn fill_keeps_types_whole_and_writes_into_text() {
        let current = json!("headway:notedeck/some-card-here");
        let prev = json!({"n": 2, "ref": "fake:1", "tags": ["a", "b"]});
        let mut bound = BTreeMap::new();
        bound.insert("card".to_owned(), json!({"issue": "headway:b/x-y-z"}));
        let vars = Vars {
            current: Some(&current),
            prev: Some(&prev),
            bound: &bound,
        };
        let args = json!({
            "card": "{card.issue}",
            "n": "{prev.n}",
            "all": "{prev}",
            "text": "x-{prev.ref}, n={prev.n}, tag {prev.tags.1}",
            "here": "{current}",
            "nested": [{"deep": "{card.issue}"}],
            "plain": "{not a placeholder}",
            "num": 7,
        });
        assert_eq!(
            fill(&args, vars).expect("fills"),
            json!({
                "card": "headway:b/x-y-z",
                "n": 2,
                "all": {"n": 2, "ref": "fake:1", "tags": ["a", "b"]},
                "text": "x-fake:1, n=2, tag b",
                "here": "headway:notedeck/some-card-here",
                "nested": [{"deep": "headway:b/x-y-z"}],
                "plain": "{not a placeholder}",
                "num": 7,
            })
        );
    }

    /// A name with no value, or a path leading nowhere, fails naming the
    /// placeholder.
    #[test]
    fn fill_fails_on_a_missing_name_or_path() {
        let prev = json!({"ref": "fake:1"});
        let bound = BTreeMap::new();
        let vars = Vars {
            current: None,
            prev: Some(&prev),
            bound: &bound,
        };
        let err = |args: Value| fill(&args, vars).expect_err("fails");
        assert!(err(json!("{prev.nope}")).contains("{prev.nope}"));
        assert!(err(json!("{card}")).contains("no earlier step is bound as 'card'"));
        assert!(err(json!("x {current}")).contains("no current ref"));
        let none = Vars { prev: None, ..vars };
        assert!(fill(&json!("{prev}"), none)
            .expect_err("no prev")
            .contains("no previous step"));
    }
}
