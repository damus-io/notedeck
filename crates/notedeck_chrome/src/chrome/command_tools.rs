//! The Cmd-K bar's tools: every tool that can run now as a row, and the
//! argument steps a tool takes before it runs.
//!
//! [`Chrome::list_tools`] fills the bar from [`Chrome::runnable_tools`] as it
//! opens: the tools chosen lately first, under "Recent", then the active
//! app's, the chrome's and the other running apps', each under its
//! [category](ToolSpec::category) and with the keys that run it. A row is
//! matched on its title, then on the tool's name and description.
//!
//! Choosing a row asks for the arguments a person has to pick before the
//! tool can run ([`Chrome::ask_next_arg`]), one level of the bar each: an
//! enum lists its values ("Group list by" → Status, Priority, …) and a
//! required string or number takes the query as its text. An argument that
//! names what the app's view is on is never asked: it is filled from the
//! app's [current ref](notedeck::App::current_ref) as the call runs, as a
//! key binding's `{current}` is. Any other optional argument is left out, so
//! the tool takes its default.

use std::borrow::Cow;

use notedeck::{
    tr, App, AppContext, CommandAction, CommandItem, ToolArg, ToolArgType, ToolInvocation,
    ToolSpec, SAVE_KEYMAP_TOOL,
};
use serde_json::Value;

use super::command_bar::{with_arg, TextArg};
use super::runnable::RunnableTool;
use super::Chrome;
use crate::keymap::{Scopes, CURRENT};

/// The chrome's tools the bar doesn't offer: Save keymap takes the whole
/// keymap as TOML, which is the keymap editor's to send, not a line typed
/// into the query.
const UNLISTED: &[&str] = &[SAVE_KEYMAP_TOOL];

impl Chrome {
    /// Fill the open command bar with every tool that can run now (see the
    /// module docs). Allocates the rows: for the bar opening, not per frame.
    pub(super) fn list_tools(&mut self, ctx: &mut AppContext) {
        let recent_section = tr!(
            ctx.i18n,
            "Recent",
            "Command bar section listing the commands run from it lately"
        );
        let other_section = tr!(
            ctx.i18n,
            "Other",
            "Command bar section for commands that name no app or part of the shell"
        );
        let mut tools = self.runnable_tools(&ctx.registries.tools);
        // A query's answer is for an agent or a macro; the bar would throw
        // it away.
        tools.retain(|tool| !tool.spec.is_read_only() && !UNLISTED.contains(&tool.spec.name()));

        let recent = self.command_bar.recent().filter_map(|name| {
            tools
                .iter()
                .find(|tool| tool.spec.name() == name)
                .map(|tool| tool_row(tool, Cow::Owned(recent_section.clone())))
        });
        let rest = tools
            .iter()
            .filter(|tool| {
                !self
                    .command_bar
                    .recent()
                    .any(|name| name == tool.spec.name())
            })
            .map(|tool| tool_row(tool, section_of(&tool.spec, &other_section)));
        let items = recent.chain(rest).collect();
        self.command_bar.set_items(items);
    }

    /// Step the bar into the next argument `invocation`'s tool asks for, if
    /// it asks for one it hasn't been given (see the module docs). Returns
    /// whether it did; false means the call is ready to run.
    pub(super) fn ask_next_arg(
        &mut self,
        ctx: &mut AppContext,
        invocation: &ToolInvocation,
    ) -> bool {
        let default_label = tr!(
            ctx.i18n,
            "default",
            "Command bar: marks the value a command takes when none is picked"
        );
        let Some(spec) = find_spec(ctx, &invocation.name) else {
            return false;
        };
        let Some(arg) = spec
            .arguments()
            .iter()
            .find(|arg| asks_for(arg, &invocation.args))
        else {
            return false;
        };
        let prompt = spec.display_title().to_owned();
        let hint = arg.description().to_owned();
        let values = match arg.typ() {
            ToolArgType::Enum(values) => values.iter().map(|&v| Value::from(v)).collect(),
            ToolArgType::Boolean => vec![Value::from(true), Value::from(false)],
            ToolArgType::String | ToolArgType::Number => {
                let number = matches!(arg.typ(), ToolArgType::Number);
                let arg = TextArg {
                    invocation: invocation.clone(),
                    arg: arg.name(),
                    number,
                };
                self.command_bar.ask_text(prompt, hint, arg);
                return true;
            }
        };
        let items = values
            .into_iter()
            .map(|value| self.value_row(spec, arg, invocation, value, &default_label))
            .collect();
        self.command_bar.ask_row(prompt, hint, items);
        true
    }

    /// The row that picks `value` for `arg` of `invocation`, with the keys
    /// bound to that call in the active app's scopes, marked when it is the
    /// argument's default.
    fn value_row(
        &self,
        spec: &ToolSpec,
        arg: &ToolArg,
        invocation: &ToolInvocation,
        value: Value,
        default_label: &str,
    ) -> CommandItem {
        let title = value_title(&value);
        let is_default = arg.default_value() == Some(&value);
        let call = with_arg(invocation, arg.name(), value);
        let keys = self.keys_for_call(spec, &call);
        // Unnamed, so the values are listed under the prompt alone.
        let mut row = CommandItem::new("", title, CommandAction::RunTool(call));
        if is_default {
            row = row.subtitled(default_label);
        }
        match keys {
            Some(keys) => row.with_keycap(keys),
            None => row,
        }
    }

    /// The keycap of the keys bound to `call` in the active app's scopes. A
    /// binding names the arguments filled from the current ref as
    /// [`CURRENT`] (`{ card = "{current}", to = "left" }`), so those are
    /// looked up that way first.
    fn keys_for_call(&self, spec: &ToolSpec, call: &ToolInvocation) -> Option<String> {
        let active = self.apps.get(self.active as usize);
        let scopes = Scopes::new(active.map_or(&[][..], |app| app.key_scopes()))
            .with_current(active.and_then(|app| app.current_ref()));
        let keymap = self.keymap();
        let with_current = spec
            .arguments()
            .iter()
            .filter(|arg| arg.is_from_current())
            .fold(call.clone(), |call, arg| {
                with_arg(&call, arg.name(), Value::from(CURRENT))
            });
        keymap
            .keys_for_call(&call.name, &with_current.args, scopes)
            .or_else(|| keymap.keys_for_call(&call.name, &call.args, scopes))
            .map(|keys| keys.keycap(0).to_owned())
    }

    /// `invocation` with each argument its tool fills from the current ref
    /// and wasn't given set to the active app's current ref, as a binding's
    /// [`CURRENT`] would be. Unchanged when the app has none.
    pub(super) fn fill_current_args(
        &self,
        ctx: &AppContext,
        invocation: ToolInvocation,
    ) -> ToolInvocation {
        let Some(current) = self
            .apps
            .get(self.active as usize)
            .and_then(|app| app.current_ref())
        else {
            return invocation;
        };
        let Some(spec) = find_spec(ctx, &invocation.name) else {
            return invocation;
        };
        let mut call = invocation;
        for arg in spec.arguments() {
            if arg.is_from_current() && !has_arg(&call.args, arg.name()) {
                call = with_arg(&call, arg.name(), Value::from(current));
            }
        }
        call
    }
}

/// The bar's row for `tool`, under `section`: its title, its keys, matched
/// on its name and description too, running it with no arguments yet.
fn tool_row(tool: &RunnableTool, section: Cow<'static, str>) -> CommandItem {
    let spec = &tool.spec;
    let row = CommandItem::new(
        section,
        spec.display_title(),
        CommandAction::RunTool(ToolInvocation::new(spec.name(), Value::Null)),
    )
    .with_keywords(format!("{} {}", spec.name(), spec.description()));
    match &tool.keys {
        Some(keys) => row.with_keycap(keys.keycap(0)),
        None => row,
    }
}

/// The section `spec` is listed under: its category, else `other`.
fn section_of(spec: &ToolSpec, other: &str) -> Cow<'static, str> {
    match spec.category() {
        Some(category) => Cow::Borrowed(category),
        None => Cow::Owned(other.to_owned()),
    }
}

/// The registered spec of the tool `name`.
fn find_spec<'a>(ctx: &'a AppContext, name: &str) -> Option<&'a ToolSpec> {
    ctx.registries
        .tools
        .specs()
        .find(|spec| spec.name() == name)
}

/// Whether `args` sets `name`.
fn has_arg(args: &Value, name: &str) -> bool {
    args.get(name).is_some()
}

/// Whether the bar asks for `arg` before its tool runs, given `args` so
/// far: an enum, or a required boolean, string or number, unless it is set
/// already or filled from the current ref.
fn asks_for(arg: &ToolArg, args: &Value) -> bool {
    if arg.is_from_current() || has_arg(args, arg.name()) {
        return false;
    }
    match arg.typ() {
        ToolArgType::Enum(_) => true,
        ToolArgType::Boolean | ToolArgType::String | ToolArgType::Number => arg.is_required(),
    }
}

/// How a picked value reads in the bar: `group_by` as "Group by", `true` as
/// "True".
fn value_title(value: &Value) -> String {
    let raw = match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let spaced = raw.replace(['_', '-'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => spaced,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn values_read_as_sentences() {
        assert_eq!(value_title(&json!("status")), "Status");
        assert_eq!(value_title(&json!("work_order")), "Work order");
        assert_eq!(value_title(&json!(true)), "True");
        assert_eq!(value_title(&json!("")), "");
    }

    #[test]
    fn enums_and_required_args_are_asked_for() {
        let pick = ToolArg::new("by", ToolArgType::Enum(vec!["a", "b"]), "");
        let text = ToolArg::new("title", ToolArgType::String, "").required(true);
        let optional = ToolArg::new("view", ToolArgType::String, "");
        let flag = ToolArg::new("on", ToolArgType::Boolean, "");
        let card = ToolArg::new("card", ToolArgType::String, "")
            .required(true)
            .from_current();
        assert!(asks_for(&pick, &Value::Null));
        assert!(!asks_for(&pick, &json!({ "by": "a" })), "set already");
        assert!(asks_for(&text, &json!({})));
        assert!(!asks_for(&optional, &Value::Null), "takes its default");
        assert!(!asks_for(&flag, &Value::Null), "takes its default");
        assert!(!asks_for(&card, &Value::Null), "filled from current");
    }
}
