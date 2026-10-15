//! A tool's arguments, read off the `input_schema` the running notedeck sent:
//! turning `key=value` pairs into the JSON object `call_tool` takes, and
//! listing the arguments when a call gets them wrong.
//!
//! The schema is whatever `agentium_core`'s `ToolSpec::json_schema` wrote — an object with
//! `properties` (each a `type`, a `description` and maybe an `enum`) and a
//! `required` list — but nothing here assumes more than JSON Schema does, so a
//! tool that hand-writes its schema reads the same.

use serde_json::{Map, Number, Value};

/// One argument a tool takes, borrowed from its schema.
pub struct Arg<'a> {
    pub name: &'a str,
    /// The JSON Schema `type`, when the schema gives one.
    pub kind: Option<&'a str>,
    pub required: bool,
    pub description: &'a str,
    /// The values an `enum` argument is limited to.
    pub choices: Option<&'a [Value]>,
}

/// Every argument `schema` declares, in the order its `properties` list them.
pub fn args(schema: &Value) -> Vec<Arg<'_>> {
    let required = schema["required"].as_array();
    let is_required = |name: &str| required.is_some_and(|r| r.iter().any(|v| v == name));
    let Some(properties) = schema["properties"].as_object() else {
        return Vec::new();
    };
    properties
        .iter()
        .map(|(name, prop)| Arg {
            name,
            kind: prop["type"].as_str(),
            required: is_required(name),
            description: prop["description"].as_str().unwrap_or(""),
            choices: prop["enum"].as_array().map(Vec::as_slice),
        })
        .collect()
}

/// The argument list as `nd tools <name>` prints it, and as an argument error
/// ends: one line per argument, its name, type, whether it's required, and
/// what it is.
pub fn arg_lines(schema: &Value) -> String {
    let args = args(schema);
    if args.is_empty() {
        return "    (takes no arguments)\n".to_owned();
    }
    let types: Vec<String> = args.iter().map(type_label).collect();
    let name_width = args.iter().map(|a| a.name.len()).max().unwrap_or(0) + 2;
    let type_width = types.iter().map(|t| t.chars().count()).max().unwrap_or(0) + 2;
    let mut out = String::new();
    for (arg, kind) in args.iter().zip(&types) {
        out.push_str(&format!(
            "    {:<name_width$}{kind:<type_width$}{}\n",
            arg.name, arg.description
        ));
    }
    out
}

/// What [`arg_lines`] prints for an argument's type: its JSON type, or its
/// choices for an `enum`, then `, required` when it is.
fn type_label(arg: &Arg) -> String {
    let mut label = match arg.choices {
        Some(choices) => {
            let choices: Vec<&str> = choices.iter().filter_map(Value::as_str).collect();
            choices.join("|")
        }
        None => arg.kind.unwrap_or("any").to_owned(),
    };
    if arg.required {
        label.push_str(", required");
    }
    label
}

/// The `args` object for calling tool `tool` with `schema`: `base` (from
/// `--json`, or none) with each `key=value` of `pairs` set on top, coerced to
/// the type the schema gives `key`.
///
/// Fails, naming the tool and listing its arguments, on a pair without `=`, a
/// key the schema doesn't have, a value that isn't its key's type, or a
/// required key that ends up unset. Checking here is what fetching the schema
/// first buys: a typo fails in the terminal with the right spelling beside it,
/// rather than as whatever the tool makes of a missing field.
pub fn build(
    tool: &str,
    schema: &Value,
    base: Option<Value>,
    pairs: &[String],
) -> Result<Value, String> {
    let fail = |problem: String| Err(format!("{problem}\n\n{tool} takes:\n{}", arg_lines(schema)));
    let args = args(schema);

    let mut object = match base {
        None => Map::new(),
        Some(Value::Object(object)) => object,
        Some(other) => return fail(format!("--json must be an object, not {other}")),
    };

    for pair in pairs {
        let Some((key, raw)) = pair.split_once('=') else {
            return fail(format!("expected key=value, got '{pair}'"));
        };
        let Some(arg) = args.iter().find(|a| a.name == key) else {
            return fail(format!("{tool} doesn't take '{key}'"));
        };
        match coerce(arg, raw) {
            Ok(value) => object.insert(key.to_owned(), value),
            Err(problem) => return fail(format!("{key}: {problem}")),
        };
    }

    // Keys from --json are held to the schema too, so the two spellings of an
    // argument fail the same way.
    if let Some(key) = object.keys().find(|k| !args.iter().any(|a| a.name == *k)) {
        return fail(format!("{tool} doesn't take '{key}'"));
    }
    if let Some(arg) = args
        .iter()
        .find(|a| a.required && !object.contains_key(a.name))
    {
        return fail(format!("{tool} needs '{}'", arg.name));
    }
    Ok(Value::Object(object))
}

/// `raw` as the JSON value `arg`'s type calls for.
fn coerce(arg: &Arg, raw: &str) -> Result<Value, String> {
    let value = match arg.kind {
        Some("string") => Value::String(raw.to_owned()),
        Some("integer") => raw
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| format!("'{raw}' is not an integer"))?,
        Some("number") => number(raw).ok_or_else(|| format!("'{raw}' is not a number"))?,
        Some("boolean") => match raw {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => return Err(format!("'{raw}' is not true or false")),
        },
        Some(kind @ ("object" | "array")) => {
            let value: Value =
                serde_json::from_str(raw).map_err(|e| format!("'{raw}' is not JSON: {e}"))?;
            let matches = match kind {
                "object" => value.is_object(),
                _ => value.is_array(),
            };
            if !matches {
                return Err(format!("'{raw}' is not a JSON {kind}"));
            }
            value
        }
        // An untyped argument takes whatever the text parses as, and a bare
        // word as the string it is.
        _ => serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_owned())),
    };

    let Some(choices) = arg.choices else {
        return Ok(value);
    };
    if choices.contains(&value) {
        return Ok(value);
    }
    let choices: Vec<String> = choices.iter().map(Value::to_string).collect();
    Err(format!("'{raw}' is not one of {}", choices.join(", ")))
}

/// A number, kept an integer when it is one so `limit=5` goes over as `5`
/// rather than `5.0`.
fn number(raw: &str) -> Option<Value> {
    if let Ok(int) = raw.parse::<i64>() {
        return Some(Value::from(int));
    }
    let float = raw.parse::<f64>().ok()?;
    Number::from_f64(float).map(Value::Number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A schema with one argument of every type `nd` coerces, shaped the way
    /// `ToolSpec::json_schema` writes it.
    fn fixture() -> Value {
        json!({
            "type": "object",
            "properties": {
                "reference": {"type": "string", "description": "What to open"},
                "limit": {"type": "integer", "description": "How many"},
                "weight": {"type": "number", "description": "How heavy"},
                "force": {"type": "boolean", "description": "Skip the check"},
                "filter": {"type": "object", "description": "A filter"},
                "ids": {"type": "array", "description": "Some ids"},
                "col": {"type": "string", "enum": ["todo", "done"], "description": "A column"},
                "anything": {"description": "Untyped"}
            },
            "required": ["reference"]
        })
    }

    fn call(pairs: &[&str]) -> Result<Value, String> {
        let pairs: Vec<String> = pairs.iter().map(|p| p.to_string()).collect();
        build("t", &fixture(), None, &pairs)
    }

    #[test]
    fn each_type_is_coerced_from_its_text() {
        let args = call(&[
            "reference=headway:notedeck/a-b-c",
            "limit=5",
            "weight=2.5",
            "force=true",
            r#"filter={"col":"todo"}"#,
            "ids=[1,2]",
            "col=done",
            "anything=7",
        ])
        .unwrap();
        assert_eq!(
            args,
            json!({
                "reference": "headway:notedeck/a-b-c",
                "limit": 5,
                "weight": 2.5,
                "force": true,
                "filter": {"col": "todo"},
                "ids": [1, 2],
                "col": "done",
                "anything": 7,
            })
        );
    }

    #[test]
    fn a_string_stays_a_string_even_when_it_looks_like_json() {
        let args = call(&["reference=42", "anything=bare word"]).unwrap();
        assert_eq!(args, json!({"reference": "42", "anything": "bare word"}));
    }

    #[test]
    fn a_whole_number_goes_over_as_an_integer() {
        assert_eq!(
            call(&["reference=x", "weight=3"]).unwrap()["weight"],
            json!(3)
        );
    }

    #[test]
    fn an_equals_sign_in_the_value_is_kept() {
        let args = call(&["reference=headway:x?msg=a=b"]).unwrap();
        assert_eq!(args["reference"], json!("headway:x?msg=a=b"));
    }

    #[test]
    fn a_bad_value_names_the_key_and_lists_the_args() {
        for (pair, problem) in [
            ("limit=five", "limit: 'five' is not an integer"),
            ("weight=heavy", "weight: 'heavy' is not a number"),
            ("force=yes", "force: 'yes' is not true or false"),
            ("filter=[1]", "filter: '[1]' is not a JSON object"),
            ("ids={", "ids: '{' is not JSON"),
            ("col=doing", r#"col: 'doing' is not one of "todo", "done""#),
        ] {
            let err = call(&["reference=x", pair]).unwrap_err();
            assert!(err.starts_with(problem), "{pair}: {err}");
            assert!(err.contains("t takes:"), "{err}");
        }
    }

    #[test]
    fn an_unknown_or_missing_key_fails_with_the_arg_list() {
        let err = call(&["reference=x", "refrence=y"]).unwrap_err();
        assert!(err.starts_with("t doesn't take 'refrence'"), "{err}");
        assert!(
            err.contains("    reference  string, required  What to open\n"),
            "{err}"
        );

        let err = call(&["limit=1"]).unwrap_err();
        assert!(err.starts_with("t needs 'reference'"), "{err}");

        let err = call(&["reference"]).unwrap_err();
        assert!(
            err.starts_with("expected key=value, got 'reference'"),
            "{err}"
        );
    }

    #[test]
    fn json_is_the_base_and_pairs_override_it() {
        let pairs = ["limit=2".to_owned()];
        let base = json!({"reference": "x", "limit": 1});
        let args = build("t", &fixture(), Some(base), &pairs).unwrap();
        assert_eq!(args, json!({"reference": "x", "limit": 2}));

        let err = build("t", &fixture(), Some(json!({"nope": 1})), &[]).unwrap_err();
        assert!(err.starts_with("t doesn't take 'nope'"), "{err}");

        let err = build("t", &fixture(), Some(json!([1])), &[]).unwrap_err();
        assert!(err.starts_with("--json must be an object"), "{err}");
    }

    #[test]
    fn a_tool_without_arguments_says_so() {
        let schema = json!({"type": "object", "properties": {}});
        assert_eq!(build("t", &schema, None, &[]).unwrap(), json!({}));
        let err = build("t", &schema, None, &["x=1".to_owned()]).unwrap_err();
        assert!(err.ends_with("(takes no arguments)\n"), "{err}");
    }
}
