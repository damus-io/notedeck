//! The keymap read backwards: from a tool to the keys that fire it, so a
//! command bar can show each tool's keycap beside its title.
//!
//! A key fires the first ready binding of its keys in the first scope that
//! has one (see [`super::KeyEngine::resolve`]), so a binding only shows as a
//! tool's keys when nothing ahead of it takes those keys first: a binding of
//! the same keys, or of a prefix of them, in an earlier scope, or an earlier
//! binding of the same keys in its own scope. Whether a tool is *available*
//! changes frame to frame, and these lookups are made once per keymap or
//! scopes, so they assume every binding ahead is: a keycap they give is one
//! that fires the tool whenever what is ahead of it is ready too, and a
//! binding that is reached only by falling through an unavailable one shows
//! no keycap. A missing keycap costs less than a wrong one.

use notedeck::ToolInvocation;
use serde_json::Value;

use super::{BindAction, Binding, KeySeq, Keymap, Match, Scopes, CURRENT};

impl Keymap {
    /// The keys that run `tool` the way a command bar does when nothing has
    /// been chosen for it: a binding with no args, or whose args only name
    /// [`CURRENT`] (`args = { card = "{current}" }`), which the bar fills the
    /// same way. A binding naming [`CURRENT`] counts only while `scopes` has a
    /// current ref, as it only fires then. Within a scope the binding written
    /// first wins, so `j` beats a later `<Down>` for the same call.
    ///
    /// Bindings that choose an argument (`j` for `{ to = "down" }`) aren't the
    /// tool's keys but one call's: ask [`keys_for_call`](Self::keys_for_call).
    ///
    /// Walks the scopes' bindings without allocating; a caller listing every
    /// tool does it once per keymap or scopes, not per frame.
    pub fn keys_for(&self, tool: &str, scopes: Scopes<'_>) -> Option<KeySeq> {
        self.first_keys(scopes, |call| {
            call.name == tool && only_current_args(&call.args)
        })
    }

    /// The keys bound to call `tool` with exactly `args`, by the same rules as
    /// [`keys_for`](Self::keys_for): `j` for `headway_cursor_move` with
    /// `{ "to": "down" }`.
    pub fn keys_for_call(&self, tool: &str, args: &Value, scopes: Scopes<'_>) -> Option<KeySeq> {
        self.first_keys(scopes, |call| call.name == tool && call.args == *args)
    }

    /// The keys of the first binding, in scope order then file order, whose
    /// tool call is `wanted` and which the keys reach (see the module docs).
    fn first_keys(
        &self,
        scopes: Scopes<'_>,
        wanted: impl Fn(&ToolInvocation) -> bool,
    ) -> Option<KeySeq> {
        let wants = |binding: &Binding| match &binding.action {
            BindAction::Tool(call) => {
                wanted(call) && (!binding.uses_current || scopes.current().is_some())
            }
            BindAction::Macro(_) => false,
        };
        for (index, name) in scopes.iter().enumerate() {
            let Some(scope) = self.scope(name) else {
                continue;
            };
            let found = scope
                .bindings
                .iter()
                .filter(|&binding| wants(binding))
                .filter(|&binding| self.reaches(scopes, index, binding, &wants))
                .min_by_key(|binding| binding.order);
            if let Some(binding) = found {
                return Some(binding.keys);
            }
        }
        None
    }

    /// Whether pressing `binding`'s keys gets to it, it being in the scope at
    /// `index` of `scopes`: no earlier scope binds those keys or a prefix of
    /// them, nor starts a longer binding with them, and in its own scope the
    /// first binding of those keys is one `wants` takes.
    fn reaches(
        &self,
        scopes: Scopes<'_>,
        index: usize,
        binding: &Binding,
        wants: &impl Fn(&Binding) -> bool,
    ) -> bool {
        let keys = binding.keys.as_slice();
        for earlier in scopes.iter().take(index) {
            for len in 1..=keys.len() {
                match self.lookup(earlier, &keys[..len]) {
                    Match::Exact(_) => return false,
                    Match::Prefix if len == keys.len() => return false,
                    Match::Prefix | Match::None => {}
                }
            }
        }
        let Some(own) = scopes.get(index) else {
            return false;
        };
        match self.lookup(own, keys) {
            Match::Exact(bindings) => bindings.first().is_some_and(wants),
            Match::Prefix | Match::None => false,
        }
    }
}

/// Whether `args` choose nothing: none at all, or only fields naming
/// [`CURRENT`].
fn only_current_args(args: &Value) -> bool {
    match args {
        Value::Null => true,
        Value::Object(fields) => fields.values().all(|value| value.as_str() == Some(CURRENT)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{parse, parse_over};
    use super::*;
    use serde_json::json;

    const KEYMAP: &str = r#"
        [[bind]]
        scope = "app.list"
        keys = "o"
        tool = "open"

        [[bind]]
        scope = "app.list"
        keys = "j"
        tool = "cursor"
        args = { to = "down" }

        [[bind]]
        scope = "app.list"
        keys = "<Down>"
        tool = "cursor"
        args = { to = "down" }

        [[bind]]
        scope = "app.list"
        keys = "a"
        tool = "archive"
        args = { card = "{current}" }

        [[bind]]
        scope = "app.list"
        keys = "g g"
        tool = "first"

        [[bind]]
        scope = "app"
        keys = "r"
        tool = "refresh"

        [[bind]]
        scope = "app"
        keys = "o"
        tool = "other"

        [[bind]]
        scope = "app"
        keys = "g t"
        tool = "tab"

        [[bind]]
        scope = "global"
        keys = "?"
        tool = "hints"
    "#;

    const APP: &[&str] = &["app.list", "app"];

    fn keys(text: &str) -> Option<KeySeq> {
        Some(KeySeq::parse(text).expect("keys parse"))
    }

    #[test]
    fn a_tool_finds_its_keys_in_any_active_scope() {
        let keymap = parse(KEYMAP).expect("parses");
        let scopes = Scopes::new(APP);
        assert_eq!(keymap.keys_for("open", scopes), keys("o"));
        assert_eq!(keymap.keys_for("refresh", scopes), keys("r"));
        assert_eq!(keymap.keys_for("first", scopes), keys("g g"));
        assert_eq!(keymap.keys_for("hints", scopes), keys("?"));
        assert_eq!(keymap.keys_for("nope", scopes), None);
        // A scope that isn't active lends no keys.
        assert_eq!(keymap.keys_for("open", Scopes::new(&["app"])), None);
    }

    #[test]
    fn a_call_with_args_is_found_by_its_args_and_written_order_wins() {
        let keymap = parse(KEYMAP).expect("parses");
        let scopes = Scopes::new(APP);
        // Every binding of `cursor` chooses `to`, so the tool alone has none.
        assert_eq!(keymap.keys_for("cursor", scopes), None);
        // `j` is written before `<Down>`, though `<Down>` sorts first.
        let down = json!({ "to": "down" });
        assert_eq!(keymap.keys_for_call("cursor", &down, scopes), keys("j"));
        let up = json!({ "to": "up" });
        assert_eq!(keymap.keys_for_call("cursor", &up, scopes), None);
    }

    #[test]
    fn a_current_binding_counts_only_with_a_current_ref() {
        let keymap = parse(KEYMAP).expect("parses");
        let scopes = Scopes::new(APP);
        assert_eq!(keymap.keys_for("archive", scopes), None);
        let on_card = scopes.with_current(Some("headway:b/one-two-three"));
        assert_eq!(keymap.keys_for("archive", on_card), keys("a"));
    }

    #[test]
    fn keys_an_earlier_scope_takes_are_not_a_later_tools() {
        let keymap = parse(KEYMAP).expect("parses");
        // `o` in `app` is shadowed by `o` in `app.list`.
        assert_eq!(keymap.keys_for("other", Scopes::new(APP)), None);
        // Without `app.list`, it's reached.
        assert_eq!(keymap.keys_for("other", Scopes::new(&["app"])), keys("o"));
        // `g t` is reachable: `g` is only a prefix in `app.list`.
        assert_eq!(keymap.keys_for("tab", Scopes::new(APP)), keys("g t"));
    }

    #[test]
    fn a_prefix_bound_earlier_hides_the_longer_keys() {
        let keymap = parse(
            r#"
            [[bind]]
            scope = "app.list"
            keys = "g"
            tool = "go"

            [[bind]]
            scope = "app"
            keys = "g t"
            tool = "tab"

            [[bind]]
            scope = "app"
            keys = "z"
            tool = "fold"

            [[bind]]
            scope = "app.list"
            keys = "z a"
            tool = "fold_one"
            "#,
        )
        .expect("parses");
        let scopes = Scopes::new(APP);
        // `g` fires in `app.list` before `g t` can be typed.
        assert_eq!(keymap.keys_for("tab", scopes), None);
        // `z` starts `z a` in `app.list`, so it never reaches `app`'s `z`.
        assert_eq!(keymap.keys_for("fold", scopes), None);
        assert_eq!(keymap.keys_for("fold_one", scopes), keys("z a"));
    }

    #[test]
    fn same_keys_in_one_scope_go_to_the_first_written() {
        let keymap = parse(
            r#"
            [[bind]]
            scope = "app"
            keys = "x"
            tool = "first_choice"

            [[bind]]
            scope = "app"
            keys = "x"
            tool = "fallback"
            "#,
        )
        .expect("parses");
        let scopes = Scopes::new(&["app"]);
        assert_eq!(keymap.keys_for("first_choice", scopes), keys("x"));
        assert_eq!(keymap.keys_for("fallback", scopes), None);
    }

    #[test]
    fn a_user_override_wins() {
        let user = r#"
            [[bind]]
            scope = "app.list"
            keys = "o"
            tool = "refresh"

            [[bind]]
            scope = "app.list"
            keys = "<Enter>"
            tool = "open"

            [[bind]]
            scope = "global"
            keys = "?"
            unbind = true
        "#;
        let keymap = parse_over(KEYMAP, user, |_| true).expect("merges");
        let scopes = Scopes::new(APP);
        // The user took `o` for `refresh` and moved `open` to `<Enter>`.
        assert_eq!(keymap.keys_for("open", scopes), keys("<Enter>"));
        // `o` comes before `app`'s `r`, as `app.list` is searched first.
        assert_eq!(keymap.keys_for("refresh", scopes), keys("o"));
        // An unbound tool has no keys.
        assert_eq!(keymap.keys_for("hints", scopes), None);
    }
}
