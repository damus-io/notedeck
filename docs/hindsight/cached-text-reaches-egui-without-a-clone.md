# Text formatted once reaches egui without a clone

A view caches its figures' text, but `RichText::new` and `layout_no_wrap` take an
owned `String`, so each frame clones every cached figure on its way to egui.

- headway:cashflow/group-matter-salad — ~6 min: attributed the Overview's frame-alloc figure and found ~40 of its 468 allocations were Categories' shared `bar_row` cloning each row's label, amount and share into the painter; wrote them into the measured figure rather than caching galleys per row
