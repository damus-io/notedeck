# Allocation-free painter helpers

egui's dashed lines, curves and polygons build a `Vec`, barred per frame by rule 18,
so views hand-roll chords and can't fill a shape like an arrowhead at all.

- headway:dave/camp-marine-express — ~5 min: wrote `dashed_line` and a quadratic `Curve` beside `dashed_circle` in ui/scene.rs, and drew the handoff arrowhead as an open chevron because a filled triangle needs `Shape::convex_polygon`'s Vec
