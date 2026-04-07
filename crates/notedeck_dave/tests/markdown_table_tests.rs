use egui_kittest::Harness;
use md_stream::StreamParser;
use notedeck_dave::ui::markdown_ui::render_assistant_message;

fn table_harness(markdown: &str) -> Harness<'static> {
    let mut parser = StreamParser::default();
    parser.push(markdown);
    let elements = parser.parsed().to_vec();
    let partial = parser.partial().cloned();
    let buffer = parser.buffer().to_string();

    Harness::builder()
        .with_size(egui::Vec2::new(500.0, 300.0))
        .renderer(notedeck::software_renderer())
        .build_ui(move |ui| {
            render_assistant_message(&elements, partial.as_ref(), &buffer, ui);
        })
}

#[test]
#[ignore] // requires lavapipe — run via scripts/snapshot-test
fn snapshot_table_header_background() {
    let md = "\
| Name | Status | Priority |
|------|--------|----------|
| Auth bug | Open | High |
| UI tweak | Closed | Low |
| Perf fix | Open | Medium |
";
    let mut harness = table_harness(md);
    harness.run();
    harness.snapshot("table_header_background");
}
