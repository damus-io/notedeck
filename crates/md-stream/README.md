# md-stream

Incremental zero-copy markdown parser for streaming LLM output.

Designed for chat interfaces where markdown arrives token-by-token and
needs to be rendered progressively. Zero dependencies.

## Design

All parsed output uses `Span { start, end }` byte indices into the
parser's internal buffer rather than owned `String`s. This means:

- **Zero heap allocations** in the parsing hot path
- Spans are `Copy` (16 bytes vs String's 24 + heap)
- Buffer is append-only — pushed content never moves, spans stay valid
- Resolve spans to `&str` via `span.resolve(parser.buffer())`

## Usage

```rust
use md_stream::{StreamParser, MdElement, Span};

let mut parser = StreamParser::new();

// Push tokens as they arrive from the LLM
parser.push("# Hello ");
parser.push("World\n\n");
parser.push("Some **bold** text\n\n");

// Read completed elements
for element in parser.parsed() {
    match element {
        MdElement::Heading { level, content } => {
            println!("H{}: {}", level, content.resolve(parser.buffer()));
        }
        MdElement::Paragraph(inlines) => {
            // render inline elements...
        }
        _ => {}
    }
}

// Call finalize when the stream ends to flush any partial state
parser.finalize();
```

## Supported elements

- Headings (`# ` through `###### `)
- Paragraphs with inline formatting:
  - **Bold** (`**text**`)
  - *Italic* (`*text*`)
  - ***Bold italic*** (`***text***`)
  - ~~Strikethrough~~ (`~~text~~`)
  - `Inline code` (`` `code` ``)
  - [Links]() (`[text](url)`)
  - Images (`![alt](url)`)
- Fenced code blocks (`` ``` `` and `~~~`) with language tags
- Tables (`| header | header |` with separator row)
- Thematic breaks (`---`, `***`, `___`)

## Streaming behavior

The parser handles partial input gracefully:

- **Partial state**: Access `parser.partial()` to speculatively render
  in-progress elements (e.g. a code block still receiving content)
- **Ambiguous prefixes**: Single `` ` `` is deferred until the parser
  can confirm it's not the start of `` ``` ``
- **Split boundaries**: Double newlines split across `push()` calls
  are detected correctly
