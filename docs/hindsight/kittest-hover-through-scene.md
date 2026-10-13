# Point kittest at a widget inside an egui::Scene

kittest's `hover` and `click` aim at a Scene widget's untransformed rect,
so the pointer lands elsewhere and the test silently sees no hover.

- headway:dave/eye-legend-stay — ~6 min: the scene hover test saw no card; debug prints showed the pointer at the node's scene-space center, fixed by mapping through `ctx.memory(|m| m.to_global)` in a test-only `hover_node` (ui/scene.rs tests)
- headway:dave/render-resemble-artwork — wrote a second mapping helper (`node_on_screen` + `click_at` with raw PointerButton events) to click a satellite, since `node.click()` aims at the scene-space rect too
