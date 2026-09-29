//! `tokenize_code` runs inside per-frame ui functions (every diff row, every
//! markdown code block), so tokenizing a line must not allocate (CLAUDE.md
//! rule 18). This binary owns the counting allocator and pins that at zero:
//! the lazy [`CodeTokens`](notedeck_ui::markdown::CodeTokens) iterator and the
//! case-insensitive language lookup both borrow instead of allocating.

use notedeck_testing::alloc::{measure, CountingAllocator};
use notedeck_ui::markdown::{tokenize_code, SandToken};

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Tokenize and walk every token of `code`, returning how many heap
/// allocations that made on this thread.
fn allocs_to_tokenize(code: &str, language: &str) -> u64 {
    let (count, counts) = measure(|| {
        tokenize_code(code, language)
            .map(|(token, text)| (token != SandToken::Whitespace) as usize + text.len())
            .sum::<usize>()
    });
    assert!(count > 0, "tokenizer yielded nothing for {language:?}");
    counts.thread.allocs + counts.thread.reallocs
}

#[test]
fn tokenizing_a_line_does_not_allocate() {
    let line = r#"    pub fn main() -> u32 { let s = "hi"; 42 } // done"#;
    for language in ["rust", "RUST", "Rs", "c++", "python", "sh", "toml"] {
        assert_eq!(
            allocs_to_tokenize(line, language),
            0,
            "language {language:?}"
        );
    }
}

#[test]
fn unknown_language_does_not_allocate() {
    assert_eq!(allocs_to_tokenize("plain text", "brainfuck"), 0);
}
