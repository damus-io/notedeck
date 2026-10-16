# Draw already-parsed markdown with its refs, without the caller juggling a txn

Ref-aware markdown re-parses every frame and needs the caller's transaction, so
a pane that caches its parse hand-rolls NoteContext and txn around the renderer.

- headway:personal/cook-town-slow — wrote markdown_ui in notedeck_headway/src/ui/review_sources.rs (open a txn only when the caller holds none) to draw cached explainer and code-walk ParsedMarkdown with headway: chips
