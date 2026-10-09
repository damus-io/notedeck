//! The which-key strip's content, read off the keymap: what the keys in the
//! active scopes do, or, while a chord is pending, what can finish it.
//!
//! A binding is listed only when pressing its keys would run it: its tool is
//! available, no binding of the same keys in a more specific scope (or earlier
//! in its own) takes the press first, and no key on the way there fires
//! something else. That is [`find`] asked about the binding's own keys, so the
//! strip and the engine can't disagree.

use super::{find, BindAction, Binding, Found, KeySeq, Keymap, Scopes};

/// The most keycaps one [`Hint`] holds. Bindings sharing a label beyond this
/// start another hint.
pub const MAX_HINT_KEYS: usize = 4;

/// One entry of the which-key strip: keycaps that share a job (`j k`), and the
/// job. Held inline, so listing a strip doesn't allocate.
#[derive(Clone, Copy, Debug)]
pub struct Hint<'a> {
    /// The keycaps; only the first `len` are meaningful.
    keycaps: [&'a str; MAX_HINT_KEYS],
    /// How many of `keycaps` are in use.
    len: usize,
    /// What the keys do.
    pub label: &'a str,
    /// The bindings' which-key group.
    pub group: Option<&'a str>,
}

impl<'a> Hint<'a> {
    /// The keycaps, in file order: what is left to type of each binding.
    pub fn keycaps(&self) -> &[&'a str] {
        &self.keycaps[..self.len]
    }

    /// The keycaps by value, for a drawer that walks them twice.
    pub fn keycap_iter(self) -> impl Iterator<Item = &'a str> + Clone {
        self.keycaps.into_iter().take(self.len)
    }
}

impl Keymap {
    /// The which-key strip for `scopes`. With a chord `pending`, only the
    /// bindings that finish it, each keycap just the keys left to type (`g`
    /// after a `g` of `g g`); without one, every binding the keys would run.
    ///
    /// Listed by `group`, groups in the order the most specific scope first
    /// names them, a group's bindings from every scope together; adjacent
    /// bindings of one group with the same label merge into one hint. Lazy,
    /// allocation-free, and cheap to clone, so a caller can look ahead (is it
    /// empty?) without collecting: `available` is asked about each tool as it
    /// comes.
    pub fn hints<'a>(
        &'a self,
        scopes: Scopes<'a>,
        pending: Option<KeySeq>,
        available: impl Fn(&str) -> bool + Clone + 'a,
    ) -> impl Iterator<Item = Hint<'a>> + Clone + 'a {
        let typed = pending.map_or(0, |seq| seq.as_slice().len());
        let mut shown = self
            .listed(scopes)
            .filter(move |binding| self.reachable(binding, scopes, pending, &available))
            .map(move |binding| (binding, self.hint_label(binding)))
            .peekable();

        std::iter::from_fn(move || {
            let (first, label) = shown.next()?;
            let mut hint = Hint {
                keycaps: [""; MAX_HINT_KEYS],
                len: 1,
                label,
                group: first.group.as_deref(),
            };
            hint.keycaps[0] = first.keycap(typed);
            while hint.len < MAX_HINT_KEYS {
                let same_job = |(next, next_label): &(&Binding, &str)| {
                    *next_label == label && next.group == first.group
                };
                let Some((next, _)) = shown.next_if(same_job) else {
                    break;
                };
                hint.keycaps[hint.len] = next.keycap(typed);
                hint.len += 1;
            }
            Some(hint)
        })
    }

    /// Every binding of `scopes`, grouped: a group the first time any scope
    /// names it, with its bindings from that scope and every less specific one.
    fn listed<'a>(&'a self, scopes: Scopes<'a>) -> impl Iterator<Item = &'a Binding> + Clone + 'a {
        (0..scopes.len()).flat_map(move |at| {
            let scope = scopes.get(at).and_then(|name| self.scope(name));
            scope
                .into_iter()
                .flat_map(|scope| scope.groups.iter())
                .filter(move |run| !self.group_named_before(scopes, at, run.name.as_deref()))
                .flat_map(move |run| self.group_from(scopes, at, run.name.as_deref()))
        })
    }

    /// Whether a scope more specific than `scopes[at]` has the group `name`.
    fn group_named_before(&self, scopes: Scopes<'_>, at: usize, name: Option<&str>) -> bool {
        (0..at).any(|before| {
            scopes
                .get(before)
                .and_then(|scope| self.scope(scope))
                .is_some_and(|scope| scope.group(name).is_some())
        })
    }

    /// The bindings in group `name` of `scopes[at]` and every scope after it.
    fn group_from<'a>(
        &'a self,
        scopes: Scopes<'a>,
        at: usize,
        name: Option<&'a str>,
    ) -> impl Iterator<Item = &'a Binding> + Clone + 'a {
        (at..scopes.len())
            .filter_map(move |i| scopes.get(i).and_then(|scope| self.scope(scope)))
            .filter_map(move |scope| scope.group(name).map(|run| (scope, run)))
            .flat_map(|(scope, run)| run.members.iter().map(move |&i| &scope.bindings[i]))
    }

    /// Whether typing `binding`'s keys, after `pending`, would run it: each
    /// key on the way is a prefix, and the last fires this binding rather
    /// than another.
    fn reachable(
        &self,
        binding: &Binding,
        scopes: Scopes<'_>,
        pending: Option<KeySeq>,
        available: &impl Fn(&str) -> bool,
    ) -> bool {
        let keys = binding.keys.as_slice();
        if let Some(pending) = pending {
            let typed = pending.as_slice();
            if keys.len() <= typed.len() || !keys.starts_with(typed) {
                return false;
            }
        }
        let on_the_way = (1..keys.len())
            .all(|len| matches!(find(self, scopes, &keys[..len], available), Found::Prefix));
        on_the_way
            && matches!(
                find(self, scopes, keys, available),
                Found::Fire(fired) if std::ptr::eq(fired, binding)
            )
    }

    /// What the strip calls `binding`: its label, else its macro's, else the
    /// tool or macro name.
    fn hint_label<'a>(&'a self, binding: &'a Binding) -> &'a str {
        if let Some(label) = &binding.label {
            return label;
        }
        match &binding.action {
            BindAction::Tool(invocation) => &invocation.name,
            BindAction::Macro(name) => self
                .macro_named(name)
                .and_then(|m| m.label.as_deref())
                .unwrap_or(name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::parse;
    use super::*;

    /// Two app scopes and the global one, with a group spanning scopes, a
    /// shared label, a shadowed key and a chord.
    const KEYMAP: &str = r#"
[[bind]]
scope = "app.pane"
keys = "j"
tool = "scroll_down"
label = "scroll"
group = "move"

[[bind]]
scope = "app.pane"
keys = "k"
tool = "scroll_up"
label = "scroll"
group = "move"

[[bind]]
scope = "app"
keys = "j"
tool = "cursor_down"
label = "down"
group = "move"

[[bind]]
scope = "app"
keys = "o"
tool = "open"
label = "open"
group = "card"

[[bind]]
scope = "app"
keys = "g g"
tool = "first"
label = "first"
group = "move"

[[bind]]
scope = "app"
keys = "g t"
tool = "tab"
group = "move"

[[bind]]
scope = "global"
keys = "?"
tool = "help"
label = "keys"
"#;

    const SCOPES: &[&str] = &["app.pane", "app"];

    /// The strip as `(keycaps joined by spaces, label)`.
    fn strip(keymap: &Keymap, pending: Option<&str>, off: &[&str]) -> Vec<(String, String)> {
        let pending = pending.map(|keys| KeySeq::parse(keys).expect("pending"));
        keymap
            .hints(Scopes::new(SCOPES), pending, |tool| !off.contains(&tool))
            .map(|hint| (hint.keycaps().join(" "), hint.label.to_owned()))
            .collect()
    }

    fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
        expected
            .iter()
            .map(|(keys, label)| (keys.to_string(), label.to_string()))
            .collect()
    }

    #[test]
    fn the_full_strip_groups_merges_and_drops_shadowed_keys() {
        let keymap = parse(KEYMAP).expect("keymap");
        assert_eq!(
            strip(&keymap, None, &[]),
            pairs(&[
                // `move` from app.pane, then app's `move` bindings with it;
                // app's `j` is shadowed by app.pane's.
                ("j k", "scroll"),
                ("gg", "first"),
                ("gt", "tab"),
                ("o", "open"),
                ("?", "keys"),
            ])
        );
    }

    #[test]
    fn an_unavailable_tool_is_left_out_and_unshadows_its_keys() {
        let keymap = parse(KEYMAP).expect("keymap");
        assert_eq!(
            strip(&keymap, None, &["scroll_down", "open"]),
            pairs(&[
                ("k", "scroll"),
                ("j", "down"),
                ("gg", "first"),
                ("gt", "tab"),
                ("?", "keys"),
            ])
        );
    }

    #[test]
    fn a_pending_chord_shows_only_what_finishes_it() {
        let keymap = parse(KEYMAP).expect("keymap");
        assert_eq!(
            strip(&keymap, Some("g"), &[]),
            pairs(&[("g", "first"), ("t", "tab")])
        );
    }

    #[test]
    fn a_key_that_fires_on_the_way_hides_the_chord_behind_it() {
        // `g` in the more specific scope fires before `g g` could.
        let src = format!(
            "{KEYMAP}\n[[bind]]\nscope = \"app.pane\"\nkeys = \"g\"\ntool = \"go\"\ngroup = \"move\"\n"
        );
        let keymap = parse(&src).expect("keymap");
        let shown = strip(&keymap, None, &[]);
        assert!(shown.contains(&("g".into(), "go".into())), "{shown:?}");
        assert!(
            !shown.iter().any(|(_, label)| label == "first"),
            "{shown:?}"
        );
    }

    /// A binding naming `{current}` is listed only while the app has a
    /// current ref, as it only runs then.
    #[test]
    fn a_current_binding_is_listed_only_with_a_current_ref() {
        let src = format!(
            "{KEYMAP}\n[[bind]]\nscope = \"app\"\nkeys = \"a\"\ntool = \"archive\"\n\
             args = {{ card = \"{{current}}\" }}\nlabel = \"archive\"\n"
        );
        let keymap = parse(&src).expect("keymap");
        let listed = |current| {
            keymap
                .hints(Scopes::new(SCOPES).with_current(current), None, |_| true)
                .any(|hint| hint.label == "archive")
        };
        assert!(!listed(None), "no current ref, not listed");
        assert!(listed(Some("app:card")));
    }
}
