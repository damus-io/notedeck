# Allocation-free painter helpers

Polygons and dashes build a `Vec`, and a cached `Arc<Mesh>` copies as it fades,
so views hand-roll chords, can't fill an arrowhead, and animate rects and circles.

- headway:dave/camp-marine-express — ~5 min: wrote `dashed_line` and a quadratic `Curve` beside `dashed_circle` in ui/scene.rs, and drew the handoff arrowhead as an open chevron because a filled triangle needs `Shape::convex_polygon`'s Vec
- headway:dave/text-link-oxygen — ~3 min: the card said draw worktree edges with `Shape::dashed_line`, which builds a Vec of shapes each frame; wrote `dashed_rect` over `dashed_line` in ui/scene.rs instead
- headway:cashflow/crush-snake-top — ~6 min: the HUD design pass needed gradients, a diamond and a padlock; drew the gradients as 16 stepped rects (`fade_rule`) and bands, the diamond as a rotated `RectShape`, and the padlock shackle as a half-rounded rect stroke in hud/paint.rs, because `Mesh` and `convex_polygon` each carry a Vec
- headway:cashflow/write-blame-penalty — ~4 min: checked whether `Shape::Mesh` was really out for the level-up burst (it holds an `Arc<Mesh>`, but `multiply_opacity` and `transform` `make_mut`-copy it each frame of a fade), then drew the glow as a blurred `RectShape` and the sparks as circles instead of a radial-gradient mesh
