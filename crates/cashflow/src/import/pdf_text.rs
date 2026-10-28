//! A PDF's text as lines of positioned cells, for importers that read a
//! printed table such as a bank statement.
//!
//! A statement's text has no column separators: the withdrawal and the
//! deposit column hold the same kind of number, and which one a figure is
//! in is the only thing that tells money out from money in. So this keeps
//! where each run of text sits on the page. Every glyph [`pdf_extract`]
//! draws is placed by its text matrix; glyphs on one baseline make a
//! [`Line`], and a line splits into a [`Cell`] wherever the gap between two
//! glyphs is wider than a letter, which is how a table's columns come
//! apart. A narrower gap is a space between words: PDFs often position
//! words rather than drawing a space glyph, and space glyphs that are
//! drawn are dropped and measured the same way, so both read alike.
//!
//! Text drawn at an angle, such as the vertical form codes down a
//! statement's margin, is left out: it belongs to no line.

use pdf_extract::{MediaBox, OutputDev, OutputError, Transform};

/// A run of text on one line with no gap wider than a letter in it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Cell {
    /// Where its first glyph starts, in points from the page's left edge.
    pub left: f64,
    /// Where its last glyph ends.
    pub right: f64,
    pub text: String,
}

/// The cells on one baseline of one page, left to right.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Line {
    /// The page, from 1.
    pub page: u32,
    /// The baseline, in points from the page's bottom edge.
    pub y: f64,
    pub cells: Vec<Cell>,
}

impl Line {
    /// The cells' text joined by single spaces.
    pub fn text(&self) -> String {
        let texts: Vec<&str> = self.cells.iter().map(|c| c.text.as_str()).collect();
        texts.join(" ")
    }
}

/// How far apart, in points, two glyphs' baselines may be and still be on
/// one line.
const SAME_LINE: f64 = 1.5;

/// A gap wider than this many font sizes starts a new cell.
const CELL_GAP: f64 = 1.0;

/// A gap wider than this many font sizes, and no wider than [`CELL_GAP`],
/// is a space between two words.
const WORD_GAP: f64 = 0.12;

/// The text of the PDF in `bytes`, as its lines from the first page's top
/// to the last page's bottom.
pub(super) fn lines(bytes: &[u8]) -> Result<Vec<Line>, String> {
    let mut doc = pdf_extract::Document::load_mem(bytes).map_err(|e| format!("not a PDF: {e}"))?;
    if doc.is_encrypted() {
        // Statements are encrypted with an empty password, if at all.
        doc.decrypt("")
            .map_err(|e| format!("an encrypted PDF that won't open: {e}"))?;
    }
    let mut glyphs = Glyphs::default();
    pdf_extract::output_doc(&doc, &mut glyphs).map_err(|e| format!("reading its text: {e:?}"))?;
    Ok(glyphs.into_lines())
}

/// One glyph as drawn.
struct Glyph {
    page: u32,
    x: f64,
    y: f64,
    /// Its advance width on the page.
    width: f64,
    /// Its font size on the page.
    size: f64,
    text: String,
}

/// The upright glyphs of a document, in drawing order.
#[derive(Default)]
struct Glyphs {
    page: u32,
    glyphs: Vec<Glyph>,
}

impl OutputDev for Glyphs {
    fn begin_page(
        &mut self,
        page_num: u32,
        _media_box: &MediaBox,
        _art_box: Option<(f64, f64, f64, f64)>,
    ) -> Result<(), OutputError> {
        self.page = page_num;
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    /// `trm` places the glyph and scales it, but leaves the font size out:
    /// `width` is in thousandths of `font_size`.
    fn output_character(
        &mut self,
        trm: &Transform,
        width: f64,
        _spacing: f64,
        font_size: f64,
        char: &str,
    ) -> Result<(), OutputError> {
        let upright = trm.m12.abs() < 1e-6 && trm.m21.abs() < 1e-6 && trm.m11 > 0.0;
        if !upright {
            return Ok(());
        }
        self.glyphs.push(Glyph {
            page: self.page,
            x: trm.m31,
            y: trm.m32,
            width: width * font_size * trm.m11,
            size: font_size * trm.m22.abs(),
            text: char.to_owned(),
        });
        Ok(())
    }

    fn begin_word(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn end_word(&mut self) -> Result<(), OutputError> {
        Ok(())
    }

    fn end_line(&mut self) -> Result<(), OutputError> {
        Ok(())
    }
}

impl Glyphs {
    /// The glyphs gathered into lines and cells, as [`lines`] says.
    fn into_lines(mut self) -> Vec<Line> {
        self.glyphs.sort_by(|a, b| {
            a.page
                .cmp(&b.page)
                .then(b.y.total_cmp(&a.y))
                .then(a.x.total_cmp(&b.x))
        });
        let mut lines = Vec::new();
        let mut rest = self.glyphs.as_slice();
        while let Some(first) = rest.first() {
            let len = rest
                .iter()
                .position(|g| g.page != first.page || first.y - g.y > SAME_LINE)
                .unwrap_or(rest.len());
            let (line, after) = rest.split_at(len);
            rest = after;
            if let Some(line) = line_of(line) {
                lines.push(line);
            }
        }
        lines
    }
}

/// The glyphs of one baseline as a line of cells, or `None` when they are
/// all blank.
fn line_of(glyphs: &[Glyph]) -> Option<Line> {
    let mut sorted: Vec<&Glyph> = glyphs.iter().collect();
    sorted.sort_by(|a, b| a.x.total_cmp(&b.x));
    let mut cells: Vec<Cell> = Vec::new();
    for glyph in sorted {
        if glyph.text.trim().is_empty() {
            continue;
        }
        let gap = cells.last().map(|cell| glyph.x - cell.right);
        match (cells.last_mut(), gap) {
            (Some(cell), Some(gap)) if gap <= glyph.size * CELL_GAP => {
                if gap > glyph.size * WORD_GAP {
                    cell.text.push(' ');
                }
                cell.text.push_str(&glyph.text);
                cell.right = cell.right.max(glyph.x + glyph.width);
            }
            _ => cells.push(Cell {
                left: glyph.x,
                right: glyph.x + glyph.width,
                text: glyph.text.clone(),
            }),
        }
    }
    let first = glyphs.first()?;
    if cells.is_empty() {
        return None;
    }
    Some(Line {
        page: first.page,
        y: first.y,
        cells,
    })
}

/// Building small PDFs for tests: text placed at given points in
/// Helvetica, whose widths every PDF reader knows without the font being
/// embedded.
#[cfg(test)]
pub(super) mod synthetic {
    use pdf_extract::content::{Content, Operation};
    use pdf_extract::{Dictionary, Document, Object, Stream};

    /// Text to draw: `text` at (`x`, `y`) in points, 8 points tall.
    pub struct Placed<'a> {
        pub x: f64,
        pub y: f64,
        pub text: &'a str,
    }

    /// A PDF of `pages`, each the text placed on it.
    pub fn pdf(pages: &[Vec<Placed>]) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let mut font = Dictionary::new();
        font.set("Type", "Font");
        font.set("Subtype", "Type1");
        font.set("BaseFont", "Helvetica");
        let font_id = doc.add_object(font);
        let mut fonts = Dictionary::new();
        fonts.set("F1", font_id);
        let mut resources = Dictionary::new();
        resources.set("Font", fonts);
        let resources_id = doc.add_object(resources);

        let mut kids = Vec::new();
        for placed in pages {
            let mut operations = Vec::new();
            for p in placed {
                operations.extend([
                    Operation::new("BT", vec![]),
                    Operation::new("Tf", vec!["F1".into(), 8.into()]),
                    Operation::new(
                        "Td",
                        vec![Object::Real(p.x as f32), Object::Real(p.y as f32)],
                    ),
                    Operation::new("Tj", vec![Object::string_literal(p.text)]),
                    Operation::new("ET", vec![]),
                ]);
            }
            let content = Content { operations }.encode().unwrap();
            let content_id = doc.add_object(Stream::new(Dictionary::new(), content));
            let mut page = Dictionary::new();
            page.set("Type", "Page");
            page.set("Parent", pages_id);
            page.set("Contents", content_id);
            kids.push(Object::from(doc.add_object(page)));
        }
        let mut tree = Dictionary::new();
        tree.set("Type", "Pages");
        tree.set("Count", kids.len() as i64);
        tree.set("Kids", kids);
        tree.set("Resources", resources_id);
        tree.set("MediaBox", vec![0.into(), 0.into(), 612.into(), 792.into()]);
        doc.objects.insert(pages_id, Object::Dictionary(tree));
        let mut catalog = Dictionary::new();
        catalog.set("Type", "Catalog");
        catalog.set("Pages", pages_id);
        let catalog_id = doc.add_object(catalog);
        doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::synthetic::{Placed, pdf};
    use super::*;

    fn at(x: f64, y: f64, text: &str) -> Placed<'_> {
        Placed { x, y, text }
    }

    #[test]
    fn glyphs_become_lines_of_cells() {
        let bytes = pdf(&[
            vec![
                at(90.0, 700.0, "Payroll Deposit"),
                at(45.0, 700.0, "6 Jul"),
                at(430.0, 700.5, "3,000.00"),
                at(90.0, 690.0, "next  line"),
            ],
            vec![at(17.0, 600.0, "page two")],
        ]);
        let lines = lines(&bytes).unwrap();
        let texts: Vec<Vec<&str>> = lines
            .iter()
            .map(|l| l.cells.iter().map(|c| c.text.as_str()).collect())
            .collect();
        assert_eq!(
            texts,
            [
                vec!["6 Jul", "Payroll Deposit", "3,000.00"],
                vec!["next line"],
                vec!["page two"],
            ]
        );
        assert_eq!(lines[0].page, 1);
        assert_eq!(lines[2].page, 2);
        let amount = &lines[0].cells[2];
        assert_eq!(amount.left, 430.0);
        // Eight Helvetica glyphs at 8 points: digits are 556/1000 em,
        // the comma and point 278.
        let width = (6.0 * 0.556 + 2.0 * 0.278) * 8.0;
        assert!((amount.right - (430.0 + width)).abs() < 0.01, "{amount:?}");
        assert_eq!(lines[0].text(), "6 Jul Payroll Deposit 3,000.00");
    }

    #[test]
    fn not_a_pdf() {
        assert!(
            lines(b"Date,Amount\n")
                .unwrap_err()
                .starts_with("not a PDF")
        );
    }
}
