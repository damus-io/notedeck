# A new app registers with the chrome in one place

A new app is a `NotedeckApp` variant plus a copied cfg-gated arm in ~16 matches
(app.rs, sidebar.rs, header.rs); a missed arm fails only under that feature.

- headway:cashflow/answer-vibrant-crush — ~5 min: 12 forwarding arms in `notedeck_chrome/src/app.rs` copied from Horizon's by a regex script, then the label, two icon matches and `add_app` by hand; the template commit (e41e1385bff1) predates half of them
- headway:notedeck/danger-hub-print — ~3 min: the `--serve` roster (Dave without its host, Notebook, Headway) is a second hand-kept list in `Chrome::new_with_render_state`; a new app with data tools has to be added there too, or nd never sees them when no GUI runs. A per-app "serves data" property on `NotedeckApp` would let the roster filter instead
