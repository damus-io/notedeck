//! Writing a [`Journal`] back to ledger-cli text, in one canonical form.
//!
//! The writer is the inverse of [`Journal::parse`]: parsing what it writes
//! gives back the same transactions, postings, metadata, comments, costs and
//! prices. It is also a normal form. Its output depends only on the
//! journal's contents, never on how the source text was laid out, so writing
//! a parsed copy of the output reproduces the output byte for byte. Journal
//! segments are named by the hash of these bytes, which is why the layout is
//! fixed rather than configurable:
//!
//! ```text
//! ; file comments that came before any transaction
//!
//! P 2024-01-01 BTC 61234.56789 CAD
//!
//! 2024-01-02 * (1042) Payee
//! \t; key: transaction metadata
//! \t; a transaction comment
//! \t* assets:bank\t-4.50 CAD
//! \t; key: posting metadata
//! \texpenses:food
//!
//! ; a file comment after the first transaction
//! ```
//!
//! Sections are separated by one blank line and the text ends with a
//! newline. Lines are indented with a tab and an account is separated from
//! its amount by a tab. Every note is written on its own line under what it
//! belongs to, metadata before plain comments, so a trailing comment in the
//! source moves to the next line. Amounts use their commodity's learned
//! style and precision, and costs and prices keep their own scale.

use std::borrow::Cow;
use std::fmt::{self, Write};

use crate::amount::needs_quotes;
use crate::{CostKind, Date, FileComment, Journal, PostingFlags, Price, Txn, TxnFlag, TxnId};

/// How to write a journal.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct WriteOptions {
    /// Write every posting's amount, including the ones that were left out
    /// and inferred when the journal was read. An elided posting that was
    /// filled with several commodities is then written as one posting per
    /// commodity. Off by default, so elided amounts stay elided.
    pub explicit_amounts: bool,
}

impl Journal {
    /// Write the whole journal as ledger-cli text: file comments, then
    /// prices, then the transactions with the file comments that came
    /// between them.
    ///
    /// **The output is canonical: identical journal contents produce
    /// identical bytes**, whatever the text they were parsed from looked
    /// like (see the module docs for the layout). Journal segments are
    /// content-addressed by these bytes, so changing the layout changes
    /// every segment's name; treat it as a format version.
    ///
    /// Amounts are written at their commodity's precision in this journal's
    /// [`crate::Commodities`] table. Text the format cannot hold is
    /// normalized: line breaks in strings become spaces, strings are
    /// trimmed, and the whitespace before a `;` in a payee shrinks to one
    /// space, since a tab or two spaces there would start a comment. A
    /// journal read by [`Journal::parse`] never needs any of this.
    #[profiling::function]
    pub fn write_ledger<W: Write>(&self, out: &mut W, options: WriteOptions) -> fmt::Result {
        let mut writer = Writer::new(self, out, options);
        let mut comments = self.file_comments.iter().peekable();
        writer.file_comments(&mut comments, 0)?;
        writer.prices()?;
        for index in 0..self.txns.len() {
            writer.txn(&self.txns[index])?;
            writer.file_comments(&mut comments, index as u32 + 1)?;
        }
        // Any placed past the last transaction.
        writer.file_comments(&mut comments, u32::MAX)
    }

    /// Write only the transactions `txns`, in the order given, each in the
    /// canonical form [`Self::write_ledger`] uses and separated by blank
    /// lines. File comments and prices are left out.
    pub fn write_txns<W: Write>(
        &self,
        txns: impl IntoIterator<Item = TxnId>,
        out: &mut W,
        options: WriteOptions,
    ) -> fmt::Result {
        let mut writer = Writer::new(self, out, options);
        for id in txns {
            writer.txn(self.txn(id))?;
        }
        Ok(())
    }

    /// Write only the `P` directives dated before `end` (every one when
    /// `None`), in the canonical form [`Self::write_ledger`] uses, with no
    /// blank line before or after. With [`Self::write_txns`] this exports a
    /// date range that still converts: a rate from before the range prices
    /// the postings in it.
    pub fn write_prices<W: Write>(&self, end: Option<Date>, out: &mut W) -> fmt::Result {
        let mut writer = Writer::new(self, out, WriteOptions::default());
        for price in &self.prices {
            if end.is_some_and(|end| price.date >= end) {
                continue;
            }
            writer.price(price)?;
        }
        Ok(())
    }

    /// The whole journal as canonical ledger-cli text; see
    /// [`Self::write_ledger`].
    pub fn to_ledger(&self, options: WriteOptions) -> String {
        let mut out = String::new();
        self.write_ledger(&mut out, options)
            .expect("writing to a String cannot fail");
        out
    }
}

/// Writes sections of one journal to `out`, a blank line between each.
struct Writer<'a, W> {
    journal: &'a Journal,
    out: &'a mut W,
    options: WriteOptions,
    /// Whether a section has been written, so the next needs a blank line.
    started: bool,
}

impl<'a, W: Write> Writer<'a, W> {
    fn new(journal: &'a Journal, out: &'a mut W, options: WriteOptions) -> Self {
        Self {
            journal,
            out,
            options,
            started: false,
        }
    }

    /// Begin a section: a blank line unless it is the first.
    fn section(&mut self) -> fmt::Result {
        if self.started {
            self.out.write_char('\n')?;
        }
        self.started = true;
        Ok(())
    }

    /// Write, as one section, the next file comments placed before
    /// transaction `txns_before`.
    fn file_comments<'c>(
        &mut self,
        comments: &mut std::iter::Peekable<impl Iterator<Item = &'c FileComment>>,
        txns_before: u32,
    ) -> fmt::Result {
        let mut first = true;
        while let Some(comment) = comments.next_if(|c| c.txns_before <= txns_before) {
            if first {
                self.section()?;
                first = false;
            }
            let text = one_line(self.journal.strings.get(comment.text));
            write_comment(self.out, "", &text)?;
        }
        Ok(())
    }

    /// Write every `P` directive as one section.
    fn prices(&mut self) -> fmt::Result {
        let journal = self.journal;
        if journal.prices.is_empty() {
            return Ok(());
        }
        self.section()?;
        for price in &journal.prices {
            self.price(price)?;
        }
        Ok(())
    }

    /// `P DATE COMMODITY PRICE`.
    fn price(&mut self, price: &Price) -> fmt::Result {
        let journal = self.journal;
        let name = journal.commodities.name(price.commodity);
        let quote = if needs_quotes(name) { "\"" } else { "" };
        writeln!(
            self.out,
            "P {} {quote}{name}{quote} {}",
            price.date,
            price.value.display(price.quote, &journal.commodities)
        )
    }

    /// Write one transaction as a section: its header, its own notes, then
    /// each posting with its notes.
    fn txn(&mut self, txn: &Txn) -> fmt::Result {
        self.section()?;
        self.header(txn)?;
        self.notes(txn, None)?;

        let journal = self.journal;
        let postings = journal.postings_of(txn);
        let mut previous_elided = false;
        for (offset, posting) in postings.iter().enumerate() {
            let elided = posting.flags.contains(PostingFlags::ELIDED);
            // An elided posting filled with several commodities was one line;
            // the first of its run stands for the rest.
            let continues_elided = elided && previous_elided;
            previous_elided = elided;
            if !continues_elided || self.options.explicit_amounts {
                let index = txn.postings.start + offset as u32;
                self.posting(index, !elided || self.options.explicit_amounts)?;
            }
            self.notes(txn, Some(offset as u32))?;
        }
        Ok(())
    }

    /// `DATE [*|!] [(CODE)] PAYEE`.
    fn header(&mut self, txn: &Txn) -> fmt::Result {
        let strings = &self.journal.strings;
        write!(self.out, "{}", txn.date)?;
        match txn.flag {
            TxnFlag::Uncleared => {}
            TxnFlag::Cleared => self.out.write_str(" *")?,
            TxnFlag::Pending => self.out.write_str(" !")?,
        }
        if let Some(code) = txn.code {
            write!(self.out, " ({})", one_line(strings.get(code)))?;
        }
        let payee = strings.get(txn.payee).trim();
        if !payee.is_empty() {
            self.out.write_char(' ')?;
            write_payee(self.out, payee)?;
        }
        self.out.write_char('\n')
    }

    /// One posting line: mark, account in its virtual brackets, and the
    /// amount and cost when `amount` is set.
    fn posting(&mut self, index: u32, amount: bool) -> fmt::Result {
        let journal = self.journal;
        let posting = journal.postings[index as usize];
        let flags = posting.flags;
        self.out.write_char('\t')?;
        if flags.contains(PostingFlags::CLEARED) {
            self.out.write_str("* ")?;
        } else if flags.contains(PostingFlags::PENDING) {
            self.out.write_str("! ")?;
        }
        let (open, close) = if flags.contains(PostingFlags::VIRTUAL) {
            ("(", ")")
        } else if flags.contains(PostingFlags::BALANCED_VIRTUAL) {
            ("[", "]")
        } else {
            ("", "")
        };
        let account = journal.accounts.full_name(posting.account);
        write!(self.out, "{open}{account}{close}")?;
        if amount {
            write!(
                self.out,
                "\t{}",
                posting.amount().display(&journal.commodities)
            )?;
            if let Some(cost) = journal.cost_of(index) {
                let at = match cost.kind {
                    CostKind::PerUnit => "@",
                    CostKind::Total => "@@",
                };
                write!(
                    self.out,
                    " {at} {}",
                    cost.value.display(cost.commodity, &journal.commodities)
                )?;
            }
        }
        self.out.write_char('\n')
    }

    /// The metadata, then the plain comments, attached to `posting` (an
    /// offset into the transaction's postings) or, for `None`, to the
    /// transaction itself, one per line.
    fn notes(&mut self, txn: &Txn, posting: Option<u32>) -> fmt::Result {
        let journal = self.journal;
        let strings = &journal.strings;
        for meta in journal.meta_of(txn).iter().filter(|m| m.posting == posting) {
            let key = one_line(strings.get(meta.key));
            let value = one_line(strings.get(meta.value));
            write!(self.out, "\t; {key}:")?;
            if !value.is_empty() {
                write!(self.out, " {value}")?;
            }
            self.out.write_char('\n')?;
        }
        let comments = journal.comments_of(txn).iter();
        for comment in comments.filter(|c| c.posting == posting) {
            write_comment(self.out, "\t", &one_line(strings.get(comment.text)))?;
        }
        Ok(())
    }
}

/// `INDENT; text`, or a bare `;` for an empty comment.
fn write_comment(out: &mut impl Write, indent: &str, text: &str) -> fmt::Result {
    if text.is_empty() {
        return writeln!(out, "{indent};");
    }
    writeln!(out, "{indent}; {text}")
}

/// `text` on one line and trimmed, as the parser will read it back.
fn one_line(text: &str) -> Cow<'_, str> {
    let text = text.trim();
    if !text.contains(['\n', '\r']) {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.replace(['\n', '\r'], " "))
}

/// Whitespace that can sit inside a payee: what the parser would split on,
/// and line breaks, which become spaces.
fn is_payee_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// Write a trimmed payee so it reads back whole: line breaks become spaces,
/// and the whitespace before a `;` shrinks to one space, because a tab or
/// two spaces there would start the header's comment.
fn write_payee(out: &mut impl Write, payee: &str) -> fmt::Result {
    let mut rest = payee;
    while let Some(start) = rest.find(is_payee_space) {
        out.write_str(&rest[..start])?;
        let run = &rest[start..];
        let end = run.find(|c| !is_payee_space(c)).unwrap_or(run.len());
        let next = &run[end..];
        if next.starts_with(';') {
            out.write_char(' ')?;
        } else {
            for c in run[..end].chars() {
                out.write_char(if c == '\t' || c == ' ' { c } else { ' ' })?;
            }
        }
        rest = next;
    }
    out.write_str(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payee(text: &str) -> String {
        let mut out = String::new();
        write_payee(&mut out, text).unwrap();
        out
    }

    #[test]
    fn payee_whitespace_before_a_semicolon_shrinks() {
        assert_eq!(payee("A;B ; c"), "A;B ; c");
        assert_eq!(payee("A  ; c"), "A ; c");
        assert_eq!(payee("A\t; c"), "A ; c");
        assert_eq!(payee("A  B\tC"), "A  B\tC");
        assert_eq!(payee("A\nB\r\n;C"), "A B ;C");
    }

    #[test]
    fn one_line_trims_and_joins() {
        assert_eq!(one_line("  x  "), "x");
        assert_eq!(one_line("a\nb\r\nc"), "a b  c");
    }
}
