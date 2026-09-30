//! Every non-ASCII character Headway writes into a string or char literal must
//! have a glyph in notedeck's loaded fonts.
//!
//! egui draws a missing glyph as an empty box ("tofu") and says nothing, so an
//! icon picked from a font nobody ships only shows up when someone looks at the
//! screen — "⧉" sat on the "Review diff" and "View dependency graph" actions
//! that way. This scans `src/` for the characters instead of rendering every
//! pane, so it covers labels no snapshot happens to reach.

use std::path::{Path, PathBuf};

/// One non-ASCII character found in a literal, and where.
struct Found {
    ch: char,
    file: PathBuf,
    line: usize,
}

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The non-ASCII characters inside `"…"` string literals and `'…'` char
/// literals on one source line. Line comments are skipped, which keeps doc
/// prose (which may name a missing glyph on purpose) out of the check. A
/// deliberately small lexer: it knows escapes, but not raw strings or string
/// literals that span lines, neither of which Headway uses for UI text.
fn literal_chars(line: &str, out: &mut Vec<char>) {
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    let mut in_str = false;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            match c {
                '\\' => i += 1,
                '"' => in_str = false,
                c if !c.is_ascii() => out.push(c),
                _ => {}
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            return;
        } else if c == '"' {
            in_str = true;
        } else if c == '\'' && chars.get(i + 2) == Some(&'\'') {
            // A one-character char literal; `'a` lifetimes never match.
            let lit = chars[i + 1];
            if !lit.is_ascii() {
                out.push(lit);
            }
            i += 2;
        }
        i += 1;
    }
}

#[test]
fn every_literal_glyph_is_in_the_loaded_fonts() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    files.sort();

    let mut found = Vec::new();
    let mut line_chars = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read source file");
        for (n, line) in text.lines().enumerate() {
            line_chars.clear();
            literal_chars(line, &mut line_chars);
            found.extend(line_chars.iter().map(|&ch| Found {
                ch,
                file: file.clone(),
                line: n + 1,
            }));
        }
    }
    assert!(
        found.iter().any(|f| f.ch == '←'),
        "the scan should find headway's own \"← Back\" glyph; is the lexer broken?"
    );

    // A bare `Context` has no font provider, so this checks notedeck's bundled
    // fonts only. The app also falls back to the OS's fonts on desktop, but a
    // glyph that only a system font has is a box on Android and on any machine
    // without that font, so it still fails here.
    let ctx = egui::Context::default();
    notedeck::fonts::setup_fonts(&ctx);
    // Fonts set with `set_fonts` take effect at the start of the next pass.
    ctx.run_pass(Default::default(), |_| {})
        .drop_without_applying_deltas();
    let font = egui::FontId::proportional(14.0);

    let missing: Vec<String> = found
        .iter()
        .filter(|f| !ctx.fonts_mut(|fonts| fonts.has_glyph(&font, f.ch)))
        .map(|f| {
            format!(
                "{} (U+{:04X}) at {}:{}",
                f.ch,
                f.ch as u32,
                f.file.strip_prefix(&src).unwrap_or(&f.file).display(),
                f.line
            )
        })
        .collect();
    assert!(
        missing.is_empty(),
        "these characters have no glyph in notedeck's fonts and will render as a box:\n  {}",
        missing.join("\n  ")
    );
}
