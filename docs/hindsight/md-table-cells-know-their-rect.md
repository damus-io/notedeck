# A Markdown table cell knows its rect in the grid

A table is an `egui::Grid`, whose column widths and row heights are private,
so a cell fill (explain's `td.hot`) reads the cursor and remembers row heights.

- headway:headway/custom-supply-dog — ~10 min: `table_cell_ui` reads the column's right edge off `ui.cursor()` and keeps a filled row's height in egui temp memory to paint `{hot}` to its cell's edges
