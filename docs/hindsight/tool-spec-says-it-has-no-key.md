# A UI tool's spec says it has no key

`ui_tools.rs` says "No key does this" of `dave_spawn_session` in a doc comment;
the keymap test can't read that, so each key-less tool is hand-listed there.

- headway:notedeck/option-unit-journey — CI red on autowork until a hand list in dave_keymap.rs named it; a `.read_only()`-style `.keyless()` marker on `ToolSpec` would have let the test filter on the spec and skip the list
