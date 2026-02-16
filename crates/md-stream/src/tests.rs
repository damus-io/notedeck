//! Tests for streaming parser behavior.

use crate::partial::PartialKind;
use crate::{InlineElement, InlineStyle, MdElement, StreamParser};

#[test]
fn test_heading_complete() {
    let mut parser = StreamParser::new();
    parser.push("# Hello World\n");

    assert_eq!(parser.parsed().len(), 1);
    assert_eq!(
        parser.parsed()[0],
        MdElement::Heading {
            level: 1,
            content: "Hello World".to_string()
        }
    );
}

#[test]
fn test_heading_streaming() {
    let mut parser = StreamParser::new();

    // Stream in chunks
    parser.push("# Hel");
    assert_eq!(parser.parsed().len(), 0);
    assert!(parser.partial().is_some());

    parser.push("lo Wor");
    assert_eq!(parser.parsed().len(), 0);

    parser.push("ld\n");
    assert_eq!(parser.parsed().len(), 1);
    assert_eq!(
        parser.parsed()[0],
        MdElement::Heading {
            level: 1,
            content: "Hello World".to_string()
        }
    );
}

#[test]
fn test_code_block_complete() {
    let mut parser = StreamParser::new();
    parser.push("```rust\nfn main() {}\n```\n");

    assert_eq!(parser.parsed().len(), 1);
    match &parser.parsed()[0] {
        MdElement::CodeBlock(cb) => {
            assert_eq!(cb.language.as_deref(), Some("rust"));
            assert_eq!(cb.content, "fn main() {}\n");
        }
        _ => panic!("Expected code block"),
    }
}

#[test]
fn test_code_block_streaming() {
    let mut parser = StreamParser::new();

    parser.push("```py");
    assert!(parser.in_code_block() || parser.partial().is_some());

    parser.push("thon\n");
    assert!(parser.in_code_block());

    parser.push("print('hello')\n");
    assert!(parser.in_code_block());
    assert_eq!(parser.parsed().len(), 0);

    parser.push("```\n");
    assert!(!parser.in_code_block());
    assert_eq!(parser.parsed().len(), 1);
}

#[test]
fn test_multiple_elements() {
    let mut parser = StreamParser::new();
    parser.push("# Title\n\nSome paragraph text.\n\n## Subtitle\n");

    assert!(parser.parsed().len() >= 2);
}

#[test]
fn test_thematic_break() {
    let mut parser = StreamParser::new();
    parser.push("---\n");

    assert_eq!(parser.parsed().len(), 1);
    assert_eq!(parser.parsed()[0], MdElement::ThematicBreak);
}

#[test]
fn test_finalize_incomplete_code() {
    let mut parser = StreamParser::new();
    parser.push("```\nunclosed code");

    assert_eq!(parser.parsed().len(), 0);

    parser.finalize();

    assert_eq!(parser.parsed().len(), 1);
    match &parser.parsed()[0] {
        MdElement::CodeBlock(cb) => {
            assert!(cb.content.contains("unclosed code"));
        }
        _ => panic!("Expected code block"),
    }
}

#[test]
fn test_realistic_llm_stream() {
    let mut parser = StreamParser::new();

    // Simulate realistic LLM token chunks
    let chunks = [
        "Here's",
        " a ",
        "simple",
        " example:\n\n",
        "```",
        "rust",
        "\n",
        "fn ",
        "main() {\n",
        "    println!(\"Hello\");\n",
        "}",
        "\n```",
        "\n\nThat's",
        " it!",
    ];

    for chunk in chunks {
        parser.push(chunk);
    }

    parser.finalize();

    // Should have: paragraph, code block, paragraph
    assert!(
        parser.parsed().len() >= 2,
        "Got {} elements",
        parser.parsed().len()
    );
}

#[test]
fn test_heading_levels() {
    let mut parser = StreamParser::new();
    parser.push("# H1\n## H2\n### H3\n");

    let headings: Vec<_> = parser
        .parsed()
        .iter()
        .filter_map(|e| {
            if let MdElement::Heading { level, .. } = e {
                Some(*level)
            } else {
                None
            }
        })
        .collect();

    assert!(headings.contains(&1));
    assert!(headings.contains(&2));
    assert!(headings.contains(&3));
}

#[test]
fn test_empty_push() {
    let mut parser = StreamParser::new();
    parser.push("");
    parser.push("");
    parser.push("# Test\n");

    assert_eq!(parser.parsed().len(), 1);
}

#[test]
fn test_partial_content_visible() {
    let mut parser = StreamParser::new();
    parser.push("```\nsome code");

    // Should be able to see partial content for speculative rendering
    let partial = parser.partial_content();
    assert!(partial.is_some());
    assert!(partial.unwrap().contains("some code"));
}

// Inline formatting tests

#[test]
fn test_inline_bold() {
    let mut parser = StreamParser::new();
    parser.push("This has **bold** text.\n\n");

    assert_eq!(parser.parsed().len(), 1);
    if let MdElement::Paragraph(inlines) = &parser.parsed()[0] {
        assert!(
            inlines.iter().any(|e| matches!(
                e,
                InlineElement::Styled { style: InlineStyle::Bold, content } if content == "bold"
            )),
            "Expected bold element, got: {:?}",
            inlines
        );
    } else {
        panic!("Expected paragraph");
    }
}

#[test]
fn test_inline_italic() {
    let mut parser = StreamParser::new();
    parser.push("This has *italic* text.\n\n");

    assert_eq!(parser.parsed().len(), 1);
    if let MdElement::Paragraph(inlines) = &parser.parsed()[0] {
        assert!(
            inlines.iter().any(|e| matches!(
                e,
                InlineElement::Styled { style: InlineStyle::Italic, content } if content == "italic"
            )),
            "Expected italic element, got: {:?}",
            inlines
        );
    } else {
        panic!("Expected paragraph");
    }
}

#[test]
fn test_inline_code() {
    let mut parser = StreamParser::new();
    parser.push("Use `code` here.\n\n");

    assert_eq!(parser.parsed().len(), 1);
    if let MdElement::Paragraph(inlines) = &parser.parsed()[0] {
        assert!(
            inlines.iter().any(|e| matches!(
                e,
                InlineElement::Code(s) if s == "code"
            )),
            "Expected code element, got: {:?}",
            inlines
        );
    } else {
        panic!("Expected paragraph");
    }
}

#[test]
fn test_inline_link() {
    let mut parser = StreamParser::new();
    parser.push("Check [this link](https://example.com) out.\n\n");

    assert_eq!(parser.parsed().len(), 1);
    if let MdElement::Paragraph(inlines) = &parser.parsed()[0] {
        assert!(inlines.iter().any(|e| matches!(
            e,
            InlineElement::Link { text, url } if text == "this link" && url == "https://example.com"
        )), "Expected link element, got: {:?}", inlines);
    } else {
        panic!("Expected paragraph");
    }
}

#[test]
fn test_inline_image() {
    let mut parser = StreamParser::new();
    parser.push("See ![alt text](image.png) here.\n\n");

    assert_eq!(parser.parsed().len(), 1);
    if let MdElement::Paragraph(inlines) = &parser.parsed()[0] {
        assert!(
            inlines.iter().any(|e| matches!(
                e,
                InlineElement::Image { alt, url } if alt == "alt text" && url == "image.png"
            )),
            "Expected image element, got: {:?}",
            inlines
        );
    } else {
        panic!("Expected paragraph");
    }
}

#[test]
fn test_inline_strikethrough() {
    let mut parser = StreamParser::new();
    parser.push("This is ~~deleted~~ text.\n\n");

    assert_eq!(parser.parsed().len(), 1);
    if let MdElement::Paragraph(inlines) = &parser.parsed()[0] {
        assert!(inlines.iter().any(|e| matches!(
            e,
            InlineElement::Styled { style: InlineStyle::Strikethrough, content } if content == "deleted"
        )), "Expected strikethrough element, got: {:?}", inlines);
    } else {
        panic!("Expected paragraph");
    }
}

#[test]
fn test_inline_mixed_formatting() {
    let mut parser = StreamParser::new();
    parser.push("Some **bold**, *italic*, and `code` mixed.\n\n");

    assert_eq!(parser.parsed().len(), 1);
    if let MdElement::Paragraph(inlines) = &parser.parsed()[0] {
        let has_bold = inlines.iter().any(|e| {
            matches!(
                e,
                InlineElement::Styled {
                    style: InlineStyle::Bold,
                    ..
                }
            )
        });
        let has_italic = inlines.iter().any(|e| {
            matches!(
                e,
                InlineElement::Styled {
                    style: InlineStyle::Italic,
                    ..
                }
            )
        });
        let has_code = inlines.iter().any(|e| matches!(e, InlineElement::Code(_)));

        assert!(has_bold, "Missing bold");
        assert!(has_italic, "Missing italic");
        assert!(has_code, "Missing code");
    } else {
        panic!("Expected paragraph");
    }
}

#[test]
fn test_inline_finalize() {
    let mut parser = StreamParser::new();
    parser.push("Text with **bold** formatting");

    // Not complete yet (no paragraph break)
    assert_eq!(parser.parsed().len(), 0);

    parser.finalize();

    // Now should have parsed with inline formatting
    assert_eq!(parser.parsed().len(), 1);
    if let MdElement::Paragraph(inlines) = &parser.parsed()[0] {
        assert!(inlines.iter().any(|e| matches!(
            e,
            InlineElement::Styled { style: InlineStyle::Bold, content } if content == "bold"
        )));
    } else {
        panic!("Expected paragraph");
    }
}

// Paragraph partial kind tests

#[test]
fn test_paragraph_partial_kind() {
    let mut parser = StreamParser::new();
    parser.push("Some text without");

    // Should have a partial with Paragraph kind, not Heading with level 0
    let partial = parser.partial().expect("Should have partial");
    assert!(
        matches!(partial.kind, PartialKind::Paragraph),
        "Expected PartialKind::Paragraph, got {:?}",
        partial.kind
    );
}

#[test]
fn test_paragraph_streaming_with_newlines() {
    let mut parser = StreamParser::new();

    // Push text with single newline - should continue accumulating
    parser.push("First line\n");
    assert!(parser.partial().is_some());
    assert!(matches!(
        parser.partial().unwrap().kind,
        PartialKind::Paragraph
    ));

    parser.push("Second line");
    assert_eq!(parser.parsed().len(), 0); // Not complete yet

    // Finalize should emit the accumulated paragraph
    parser.finalize();
    assert_eq!(parser.parsed().len(), 1);
    assert!(matches!(parser.parsed()[0], MdElement::Paragraph(_)));
}

#[test]
fn test_paragraph_double_newline_boundary() {
    let mut parser = StreamParser::new();

    // Test when double newline arrives all at once
    parser.push("Complete paragraph\n\n");
    assert_eq!(parser.parsed().len(), 1);
    assert!(matches!(parser.parsed()[0], MdElement::Paragraph(_)));
}

#[test]
fn test_paragraph_finalize_emits_content() {
    let mut parser = StreamParser::new();
    parser.push("Incomplete paragraph without double newline");

    assert_eq!(parser.parsed().len(), 0);
    assert!(matches!(
        parser.partial().unwrap().kind,
        PartialKind::Paragraph
    ));

    parser.finalize();

    assert_eq!(parser.parsed().len(), 1);
    if let MdElement::Paragraph(inlines) = &parser.parsed()[0] {
        assert!(inlines.iter().any(|e| matches!(
            e,
            InlineElement::Text(s) if s.contains("Incomplete paragraph")
        )));
    } else {
        panic!("Expected paragraph");
    }
}

#[test]
fn test_inline_code_with_angle_brackets() {
    // Test parse_inline directly
    let input = "Generic Rust: `impl Iterator<Item = &str>` returns a `Result<(), anyhow::Error>`";
    let result = crate::parse_inline(input);
    eprintln!("parse_inline result: {:#?}", result);

    let code_elements: Vec<_> = result
        .iter()
        .filter(|e| matches!(e, InlineElement::Code(_)))
        .collect();
    assert_eq!(code_elements.len(), 2, "Expected 2 code spans, got: {:#?}", result);
}

#[test]
fn test_streaming_inline_code_with_angle_brackets() {
    // Test streaming parser with token-by-token delivery
    let mut parser = StreamParser::new();
    let input = "5. Generic Rust: `impl Iterator<Item = &str>` returns a `Result<(), anyhow::Error>`\n\n";

    // Simulate streaming token by token
    for ch in input.chars() {
        parser.push(&ch.to_string());
    }

    eprintln!("Parsed elements: {:#?}", parser.parsed());
    eprintln!("Partial: {:#?}", parser.partial());

    // Should have one paragraph with code spans
    assert!(!parser.parsed().is_empty(), "Should have parsed elements");

    if let MdElement::Paragraph(inlines) = &parser.parsed()[0] {
        let code_elements: Vec<_> = inlines
            .iter()
            .filter(|e| matches!(e, InlineElement::Code(_)))
            .collect();
        assert_eq!(code_elements.len(), 2, "Expected 2 code spans, got: {:#?}", inlines);
    } else {
        panic!("Expected paragraph, got: {:?}", parser.parsed()[0]);
    }
}

#[test]
fn test_streaming_multiple_code_spans_with_angle_brackets() {
    // From the screenshot: multiple code spans with nested angle brackets
    let mut parser = StreamParser::new();
    let input = "use `HashMap<K, V>` or `Vec<String>` or `Option<Box<dyn Error>>` in your types\n\n";

    for ch in input.chars() {
        parser.push(&ch.to_string());
    }

    assert!(!parser.parsed().is_empty(), "Should have parsed elements");

    if let MdElement::Paragraph(inlines) = &parser.parsed()[0] {
        let code_elements: Vec<_> = inlines
            .iter()
            .filter(|e| matches!(e, InlineElement::Code(_)))
            .collect();
        assert_eq!(code_elements.len(), 3, "Expected 3 code spans, got: {:#?}", inlines);
    } else {
        panic!("Expected paragraph, got: {:?}", parser.parsed()[0]);
    }
}

#[test]
fn test_code_block_after_paragraph_single_newline() {
    // Reproduces: paragraph text ending with ":\n" then "```\n" code block
    // This is the common pattern: "All events share these common tags:\n```\n..."
    let mut parser = StreamParser::new();
    let input = "All events share these common tags:\n```\n[\"d\", \"<session-id>\"]\n```\n";
    parser.push(input);

    eprintln!("Before finalize - parsed: {:#?}", parser.parsed());
    eprintln!("Before finalize - partial: {:#?}", parser.partial());

    parser.finalize();

    eprintln!("After finalize - parsed: {:#?}", parser.parsed());

    // Should have: paragraph + code block
    let has_paragraph = parser.parsed().iter().any(|e| matches!(e, MdElement::Paragraph(_)));
    let has_code_block = parser.parsed().iter().any(|e| matches!(e, MdElement::CodeBlock(_)));

    assert!(has_paragraph, "Missing paragraph element");
    assert!(has_code_block, "Missing code block element");
}

#[test]
fn test_code_block_after_paragraph_single_newline_streaming() {
    // Same test but streaming char-by-char (how LLM tokens arrive)
    let mut parser = StreamParser::new();
    let input = "All events share these common tags:\n```\n[\"d\", \"<session-id>\"]\n```\n";

    for ch in input.chars() {
        parser.push(&ch.to_string());
    }

    eprintln!("Before finalize - parsed: {:#?}", parser.parsed());
    eprintln!("Before finalize - partial: {:#?}", parser.partial());
    eprintln!("Before finalize - in_code_block: {}", parser.in_code_block());

    parser.finalize();

    eprintln!("After finalize - parsed: {:#?}", parser.parsed());

    let has_paragraph = parser.parsed().iter().any(|e| matches!(e, MdElement::Paragraph(_)));
    let has_code_block = parser.parsed().iter().any(|e| matches!(e, MdElement::CodeBlock(_)));

    assert!(has_paragraph, "Missing paragraph element");
    assert!(has_code_block, "Missing code block element");
}

#[test]
fn test_heading_partial_kind_distinct_from_paragraph() {
    let mut parser = StreamParser::new();
    parser.push("# Heading without newline");

    let partial = parser.partial().expect("Should have partial");
    assert!(
        matches!(partial.kind, PartialKind::Heading { level: 1 }),
        "Expected PartialKind::Heading {{ level: 1 }}, got {:?}",
        partial.kind
    );
}

// Table tests

#[test]
fn test_table_basic_batch() {
    let mut parser = StreamParser::new();
    parser.push("| Name | Age |\n|------|-----|\n| Alice | 30 |\n| Bob | 25 |\n\n");

    let tables: Vec<_> = parser.parsed().iter().filter(|e| matches!(e, MdElement::Table { .. })).collect();
    assert_eq!(tables.len(), 1, "Expected 1 table, got: {:#?}", parser.parsed());

    if let MdElement::Table { headers, rows } = &tables[0] {
        assert_eq!(headers, &["Name", "Age"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], &["Alice", "30"]);
        assert_eq!(rows[1], &["Bob", "25"]);
    }
}

#[test]
fn test_table_streaming_char_by_char() {
    let mut parser = StreamParser::new();
    let input = "| Name | Age |\n|------|-----|\n| Alice | 30 |\n| Bob | 25 |\n\n";

    for ch in input.chars() {
        parser.push(&ch.to_string());
    }

    let tables: Vec<_> = parser.parsed().iter().filter(|e| matches!(e, MdElement::Table { .. })).collect();
    assert_eq!(tables.len(), 1, "Expected 1 table, got: {:#?}", parser.parsed());

    if let MdElement::Table { headers, rows } = &tables[0] {
        assert_eq!(headers, &["Name", "Age"]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], &["Alice", "30"]);
        assert_eq!(rows[1], &["Bob", "25"]);
    }
}

#[test]
fn test_table_after_paragraph() {
    let mut parser = StreamParser::new();
    parser.push("Here is a comparison:\n| A | B |\n|---|---|\n| 1 | 2 |\n\n");

    let has_paragraph = parser.parsed().iter().any(|e| matches!(e, MdElement::Paragraph(_)));
    let has_table = parser.parsed().iter().any(|e| matches!(e, MdElement::Table { .. }));

    assert!(has_paragraph, "Missing paragraph, got: {:#?}", parser.parsed());
    assert!(has_table, "Missing table, got: {:#?}", parser.parsed());
}

#[test]
fn test_table_after_paragraph_streaming() {
    let mut parser = StreamParser::new();
    let input = "Here is a comparison:\n| A | B |\n|---|---|\n| 1 | 2 |\n\n";

    for ch in input.chars() {
        parser.push(&ch.to_string());
    }

    let has_paragraph = parser.parsed().iter().any(|e| matches!(e, MdElement::Paragraph(_)));
    let has_table = parser.parsed().iter().any(|e| matches!(e, MdElement::Table { .. }));

    assert!(has_paragraph, "Missing paragraph, got: {:#?}", parser.parsed());
    assert!(has_table, "Missing table, got: {:#?}", parser.parsed());
}

#[test]
fn test_table_then_paragraph() {
    let mut parser = StreamParser::new();
    parser.push("| X | Y |\n|---|---|\n| a | b |\n\nSome text after.\n\n");

    let has_table = parser.parsed().iter().any(|e| matches!(e, MdElement::Table { .. }));
    let has_paragraph = parser.parsed().iter().any(|e| matches!(e, MdElement::Paragraph(_)));

    assert!(has_table, "Missing table, got: {:#?}", parser.parsed());
    assert!(has_paragraph, "Missing paragraph, got: {:#?}", parser.parsed());
}

#[test]
fn test_table_no_separator_not_a_table() {
    let mut parser = StreamParser::new();
    // Two pipe rows but no separator — should not be a table
    parser.push("| foo | bar |\n| baz | qux |\n\n");

    let has_table = parser.parsed().iter().any(|e| matches!(e, MdElement::Table { .. }));
    assert!(!has_table, "Should NOT be a table without separator row, got: {:#?}", parser.parsed());
}

#[test]
fn test_table_uneven_columns() {
    let mut parser = StreamParser::new();
    parser.push("| A | B | C |\n|---|---|---|\n| 1 | 2 |\n| x | y | z |\n\n");

    let tables: Vec<_> = parser.parsed().iter().filter(|e| matches!(e, MdElement::Table { .. })).collect();
    assert_eq!(tables.len(), 1);

    if let MdElement::Table { headers, rows } = &tables[0] {
        assert_eq!(headers.len(), 3);
        assert_eq!(rows[0].len(), 2); // Fewer cells than headers
        assert_eq!(rows[1].len(), 3);
    }
}

#[test]
fn test_table_with_alignment() {
    // Separator with alignment colons should still be recognized
    let mut parser = StreamParser::new();
    parser.push("| Left | Center | Right |\n|:-----|:------:|------:|\n| a | b | c |\n\n");

    let tables: Vec<_> = parser.parsed().iter().filter(|e| matches!(e, MdElement::Table { .. })).collect();
    assert_eq!(tables.len(), 1, "Expected table with alignment separators, got: {:#?}", parser.parsed());

    if let MdElement::Table { headers, rows } = &tables[0] {
        assert_eq!(headers, &["Left", "Center", "Right"]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0], &["a", "b", "c"]);
    }
}

#[test]
fn test_table_finalize_incomplete() {
    // Table without trailing blank line — finalize should emit it
    let mut parser = StreamParser::new();
    parser.push("| H1 | H2 |\n|---|---|\n| v1 | v2 |");

    assert_eq!(parser.parsed().len(), 0, "Table shouldn't be complete yet");

    parser.finalize();

    let has_table = parser.parsed().iter().any(|e| matches!(e, MdElement::Table { .. }));
    assert!(has_table, "Finalize should emit the table, got: {:#?}", parser.parsed());
}

#[test]
fn test_table_single_column() {
    let mut parser = StreamParser::new();
    parser.push("| Item |\n|------|\n| Apple |\n| Banana |\n\n");

    let tables: Vec<_> = parser.parsed().iter().filter(|e| matches!(e, MdElement::Table { .. })).collect();
    assert_eq!(tables.len(), 1);

    if let MdElement::Table { headers, rows } = &tables[0] {
        assert_eq!(headers, &["Item"]);
        assert_eq!(rows.len(), 2);
    }
}

#[test]
fn test_table_empty_cells() {
    let mut parser = StreamParser::new();
    parser.push("| A | B |\n|---|---|\n|  | val |\n| val |  |\n\n");

    let tables: Vec<_> = parser.parsed().iter().filter(|e| matches!(e, MdElement::Table { .. })).collect();
    assert_eq!(tables.len(), 1);

    if let MdElement::Table { headers, rows } = &tables[0] {
        assert_eq!(headers, &["A", "B"]);
        assert_eq!(rows[0], &["", "val"]);
        assert_eq!(rows[1], &["val", ""]);
    }
}

#[test]
fn test_table_streaming_realistic_llm_chunks() {
    // Simulate LLM-style token delivery
    let mut parser = StreamParser::new();
    let chunks = [
        "Here's",
        " the comparison:\n",
        "| Feature",
        " | ",
        "Rust | ",
        "Go |\n",
        "|---",
        "---|",
        "------|------|\n",
        "| Speed",
        " | Fast",
        " | Fast |\n",
        "| Safety",
        " | Yes | No |\n",
        "\nThat's",
        " the table.",
    ];

    for chunk in chunks {
        parser.push(chunk);
    }
    parser.finalize();

    let has_paragraph = parser.parsed().iter().any(|e| matches!(e, MdElement::Paragraph(_)));
    let has_table = parser.parsed().iter().any(|e| matches!(e, MdElement::Table { .. }));

    assert!(has_paragraph, "Missing paragraph, got: {:#?}", parser.parsed());
    assert!(has_table, "Missing table, got: {:#?}", parser.parsed());

    if let Some(MdElement::Table { headers, rows }) = parser.parsed().iter().find(|e| matches!(e, MdElement::Table { .. })) {
        assert_eq!(headers.len(), 3, "Expected 3 headers, got: {:?}", headers);
        assert_eq!(rows.len(), 2, "Expected 2 rows, got: {:?}", rows);
    }
}

#[test]
fn test_table_partial_shows_during_streaming() {
    let mut parser = StreamParser::new();
    // Push header + separator, then start a data row
    parser.push("| A | B |\n|---|---|\n");

    // Should have a table partial with seen_separator=true
    let partial = parser.partial().expect("Should have partial");
    assert!(
        matches!(&partial.kind, PartialKind::Table { seen_separator: true, .. }),
        "Expected table partial with seen_separator=true, got: {:?}",
        partial.kind
    );
}
