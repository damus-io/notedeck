# Draw already-parsed markdown with its refs, without the caller juggling a txn

Ref-aware markdown re-parses every frame and needs the caller's transaction, so
a pane that caches its parse hand-rolls NoteContext and txn around the renderer.

- headway:personal/cook-town-slow — wrote markdown_ui in notedeck_headway/src/ui/review_sources.rs (open a txn only when the caller holds none) to draw cached explainer and code-walk ParsedMarkdown with headway: chips
- headway:personal/transfer-pave-all — figured_ui in review_sources.rs opens the txn once and builds a NoteContext per Markdown run between figures, since the renderer takes both from the caller
- headway:notedeck/ball-lawn-zoo — figured_ui became one render_parsed_markdown call with a BlockHook, but page_body_ui in review_sources.rs still opens a txn when the caller holds none and builds the NoteContext for it
