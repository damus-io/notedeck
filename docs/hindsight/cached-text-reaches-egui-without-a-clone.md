# Text formatted once reaches egui without a clone

A view caches its figures' text, but `RichText::new` and `layout_no_wrap` take an
owned `String`, so each frame clones every cached figure on its way to egui.

- headway:cashflow/group-matter-salad — ~6 min: attributed the Overview's frame-alloc figure and found ~40 of its 468 allocations were Categories' shared `bar_row` cloning each row's label, amount and share into the painter; wrote them into the measured figure rather than caching galleys per row
- headway:headway/visit-become-begin — 0 min, but a compromise: the explainer's hunk block (`pre_ui` in `ui/review_sources.rs`) rebuilds its LayoutJob, copying every line of the hunk, each frame, as the shared Markdown code block does, rather than keeping one galley per figure and theme on the cached page
- headway:dave/life-host-material — 0 min, but a compromise: the session row's agent spec tag (`paint_spec_tag` in ui/session_list.rs) borrows the slug as `&str` all the way to the painter, then `layout_no_wrap` makes it clone the slug every frame
