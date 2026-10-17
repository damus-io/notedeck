//! Reading ledger-cli text into a [`Journal`].
//!
//! The supported subset is what real personal journals use: transactions
//! with a date, an optional `*`/`!` mark, an optional `(code)` and a payee;
//! postings separated from their amounts by a tab or two spaces; at most one
//! elided amount per transaction; `@`/`@@` costs; `P` price directives; and
//! `;` comments, with `; key: value` read as metadata. Anything else is
//! reported and skipped.
//!
//! A bad line never aborts the file. Each problem becomes a [`ParseError`]
//! carrying its line and column, the transaction it was in is left out of
//! the journal whole, and reading carries on at the next transaction.
//!
//! Reading is two passes over the text, both zero-copy. A commodity's
//! precision must be known before its first amount becomes an integer (see
//! [`Commodities`]), so the first pass only learns precision and style from
//! every posting amount, and the second builds the journal.

use crate::amount::take_commodity;
use crate::journal::{Comment, Cost, CostKind, Decimal, FileComment, Journal, Price};
use crate::{
    AccountError, AccountId, Amount, AmountError, AmountText, Commodities, CommodityId, Date,
    DateError, Meta, Posting, PostingFlags, Span, Txn, TxnFlag, TxnId,
};

/// The result of reading a journal: everything that parsed, and what did not.
#[derive(Debug)]
pub struct Parsed {
    /// The transactions, prices and comments that parsed.
    pub journal: Journal,
    /// Every problem found, in file order.
    pub errors: Vec<ParseError>,
}

/// One problem in a journal, and where it is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{line}:{col}: {kind}")]
pub struct ParseError {
    /// The line, starting at 1.
    pub line: u32,
    /// The byte column in the line, starting at 1.
    pub col: u32,
    /// What is wrong.
    pub kind: ParseErrorKind,
}

/// What is wrong with a line of a journal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseErrorKind {
    /// A transaction's date is malformed.
    #[error("bad date: {0}")]
    Date(DateError),
    /// An amount, cost or price is malformed or does not fit.
    #[error("bad amount: {0}")]
    Amount(AmountError),
    /// An account name is malformed.
    #[error("bad account: {0}")]
    Account(AccountError),
    /// An indented line with no transaction header above it.
    #[error("posting outside a transaction")]
    OrphanPosting,
    /// A line that starts with something other than a date, `P`, a comment
    /// character or whitespace, such as an `account` or `include` directive.
    #[error("unsupported directive")]
    UnsupportedDirective,
    /// A transaction code's `(` has no `)`.
    #[error("unclosed `(` in the transaction code")]
    UnclosedCode,
    /// A virtual account's `(` or `[` has no matching close.
    #[error("unclosed virtual account")]
    UnclosedVirtual,
    /// An `@` or `@@` cost with no amount before it.
    #[error("a cost needs an amount before it")]
    CostWithoutAmount,
    /// A second posting with its amount left out; only one can be inferred.
    #[error("more than one posting without an amount")]
    SecondElided,
    /// A `(virtual)` posting with its amount left out: it is outside the
    /// balance, so there is nothing to infer it from.
    #[error("a virtual posting needs an amount")]
    ElidedVirtual,
    /// A transaction header with no postings under it.
    #[error("transaction has no postings")]
    NoPostings,
    /// A transaction whose postings do not sum to zero.
    #[error("transaction does not balance: off by {residual}")]
    Unbalanced {
        /// The first commodity's amount left over, as it would be written.
        residual: String,
    },
    /// A `P` directive missing its date, commodity or amount.
    #[error("price directive needs a date, a commodity and an amount")]
    BadPrice,
}

impl Journal {
    /// Read ledger-cli text, collecting what parses into a journal and every
    /// problem into the errors.
    ///
    /// The subset: transactions with a date, an optional `*`/`!` mark, an
    /// optional `(code)` and a payee; postings separated from their amounts
    /// by a tab or two spaces; at most one elided amount per transaction
    /// (several commodities left over expand it into one posting each, as
    /// ledger-cli does); `@`/`@@` costs, kept and used to balance but with no
    /// lot accounting; `P` price directives; and `;` comments, read as
    /// metadata when they are `key: value`. A comment on its own line belongs
    /// to the posting above it, or to the transaction when no posting is
    /// above it yet, and the first transaction-level `id` sets [`Txn::id`].
    /// A transaction with any error, including not balancing, is left out
    /// whole.
    #[profiling::function]
    pub fn parse(text: &str) -> Parsed {
        let mut journal = Journal::new();
        observe_amounts(text, &mut journal.commodities);
        let mut parser = Parser {
            journal,
            errors: Vec::new(),
            pending: None,
            skipping: false,
            real: Vec::new(),
            bracketed: Vec::new(),
        };
        for (index, raw) in text.split('\n').enumerate() {
            let line = Line {
                text: raw.strip_suffix('\r').unwrap_or(raw),
                number: u32::try_from(index + 1).unwrap_or(u32::MAX),
            };
            parser.line(line);
        }
        parser.finish_txn();
        Parsed {
            journal: parser.journal,
            errors: parser.errors,
        }
    }
}

/// One line of the input, without its line ending.
#[derive(Copy, Clone)]
struct Line<'a> {
    text: &'a str,
    number: u32,
}

/// What a line is, from its first character.
#[derive(Copy, Clone, PartialEq, Eq)]
enum LineKind {
    /// Empty or only whitespace: ends a transaction.
    Blank,
    /// Starts with whitespace: a posting or a comment in a transaction.
    Indented,
    /// Starts with a comment character.
    Comment,
    /// Starts with a digit: a transaction header.
    Header,
    /// `P` and whitespace: a price directive.
    Price,
    /// Anything else.
    Directive,
}

fn classify(text: &str) -> LineKind {
    let Some(&first) = text.as_bytes().first() else {
        return LineKind::Blank;
    };
    match first {
        b' ' | b'\t' if text.trim_start().is_empty() => LineKind::Blank,
        b' ' | b'\t' => LineKind::Indented,
        b';' | b'#' | b'%' | b'|' | b'*' => LineKind::Comment,
        b'0'..=b'9' => LineKind::Header,
        b'P' if text[1..].starts_with([' ', '\t']) => LineKind::Price,
        _ => LineKind::Directive,
    }
}

/// The first pass: learn every commodity's precision and style from the
/// posting amounts of every transaction block, before any amount is
/// converted.
///
/// It takes amounts from every posting-shaped line under a header, so a
/// transaction the second pass rejects still contributes its precision.
#[profiling::function]
fn observe_amounts(text: &str, commodities: &mut Commodities) {
    let mut in_txn = false;
    for raw in text.split('\n') {
        match classify(raw) {
            LineKind::Header => in_txn = true,
            LineKind::Indented if in_txn => {
                let body = raw.trim_start();
                if body.starts_with(';') {
                    continue;
                }
                let Ok(parts) = split_posting(body) else {
                    continue;
                };
                let Some(amount) = parts.amount else {
                    continue;
                };
                if let Ok(amount) = AmountText::parse(amount) {
                    // Errors are reported by the second pass.
                    let _ = commodities.observe(&amount);
                }
            }
            LineKind::Indented => {}
            _ => in_txn = false,
        }
    }
}

/// A posting line split into its parts, borrowing the line.
struct PostingParts<'a> {
    /// The `*`/`!` mark and the virtual bits.
    flags: PostingFlags,
    /// The account name, without virtual brackets.
    account: &'a str,
    /// The amount, if written.
    amount: Option<&'a str>,
    /// The `@` or `@@` cost, if written.
    cost: Option<CostText<'a>>,
    /// The text after the comment's `;`, if any.
    comment: Option<&'a str>,
}

/// A posting's cost as written.
struct CostText<'a> {
    kind: CostKind,
    text: &'a str,
}

/// An error and the part of the line it is about.
struct Located<'a> {
    at: &'a str,
    kind: ParseErrorKind,
}

/// Split a posting line, with its indentation already removed.
fn split_posting(body: &str) -> Result<PostingParts<'_>, Located<'_>> {
    let mut flags = PostingFlags::NONE;
    let mut s = body;
    if let Some(rest) = s.strip_prefix('*') {
        flags = flags.with(PostingFlags::CLEARED);
        s = rest.trim_start();
    } else if let Some(rest) = s.strip_prefix('!') {
        flags = flags.with(PostingFlags::PENDING);
        s = rest.trim_start();
    }

    let end = account_end(s);
    let mut account = &s[..end];
    if let Some(inner) = account.strip_prefix('(') {
        account = inner.strip_suffix(')').ok_or(Located {
            at: account,
            kind: ParseErrorKind::UnclosedVirtual,
        })?;
        flags = flags.with(PostingFlags::VIRTUAL);
    } else if let Some(inner) = account.strip_prefix('[') {
        account = inner.strip_suffix(']').ok_or(Located {
            at: account,
            kind: ParseErrorKind::UnclosedVirtual,
        })?;
        flags = flags.with(PostingFlags::BALANCED_VIRTUAL);
    }

    let rest = &s[end..];
    let (amounts, comment) = match find_unquoted(rest, b';', true) {
        Some(at) => (&rest[..at], Some(&rest[at + 1..])),
        None => (rest, None),
    };
    let amounts = amounts.trim();

    let mut amount = (!amounts.is_empty()).then_some(amounts);
    let mut cost = None;
    if let Some(at) = find_unquoted(amounts, b'@', false) {
        let (kind, after) = match amounts[at + 1..].strip_prefix('@') {
            Some(after) => (CostKind::Total, after),
            None => (CostKind::PerUnit, &amounts[at + 1..]),
        };
        let before = amounts[..at].trim();
        if before.is_empty() {
            return Err(Located {
                at: &amounts[at..],
                kind: ParseErrorKind::CostWithoutAmount,
            });
        }
        amount = Some(before);
        cost = Some(CostText {
            kind,
            text: after.trim(),
        });
    }

    Ok(PostingParts {
        flags,
        account,
        amount,
        cost,
        comment,
    })
}

/// Where a posting's account name ends: at a tab, two spaces, a space before
/// a `;`, or the end of the line. Single spaces belong to the name.
fn account_end(s: &str) -> usize {
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        match b {
            b'\t' => return i,
            b' ' if matches!(bytes.get(i + 1), Some(b' ' | b'\t' | b';')) => return i,
            _ => {}
        }
    }
    s.len()
}

/// The first `needle` outside double quotes; with `after_space`, only one
/// that follows a space or a tab.
fn find_unquoted(s: &str, needle: u8, after_space: bool) -> Option<usize> {
    let mut quoted = false;
    let mut prev_space = false;
    for (i, b) in s.bytes().enumerate() {
        if b == b'"' {
            quoted = !quoted;
        } else if b == needle && !quoted && (prev_space || !after_space) {
            return Some(i);
        }
        prev_space = b == b' ' || b == b'\t';
    }
    None
}

/// Where a header's comment starts in `line`, searching from `from`: the
/// first `;` after a tab or two spaces. A `;` after one space is part of
/// the payee.
fn header_comment_start(line: &str, from: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    (from..bytes.len()).find(|&i| {
        bytes[i] == b';'
            && i >= 1
            && (bytes[i - 1] == b'\t' || (i >= 2 && bytes[i - 1] == b' ' && bytes[i - 2] == b' '))
    })
}

/// A `; key: value` comment, split.
struct MetaText<'a> {
    key: &'a str,
    value: &'a str,
}

/// Read a comment's text as metadata: its first word ends in `:` and is not
/// a `:tag:` list.
fn split_meta(text: &str) -> Option<MetaText<'_>> {
    let word_end = text.find([' ', '\t']).unwrap_or(text.len());
    let key = text[..word_end].strip_suffix(':')?;
    if key.is_empty() || key.starts_with(':') {
        return None;
    }
    Some(MetaText {
        key,
        value: text[word_end..].trim(),
    })
}

/// The byte offset of `part` in `line`; `part` must be a slice of `line`.
fn offset_in(line: &str, part: &str) -> usize {
    (part.as_ptr() as usize).saturating_sub(line.as_ptr() as usize)
}

/// A commodity's running total in a transaction being balanced.
struct Residual {
    commodity: CommodityId,
    sum: i128,
}

/// Add `qty` of `commodity` to a running balance.
fn add_residual(residuals: &mut Vec<Residual>, commodity: CommodityId, qty: i128) {
    match residuals.iter_mut().find(|r| r.commodity == commodity) {
        Some(r) => r.sum += qty,
        None => residuals.push(Residual {
            commodity,
            sum: qty,
        }),
    }
}

/// The posting whose amount was left out, to be filled in at the end.
#[derive(Copy, Clone)]
struct ElidedSlot {
    /// Its offset among the transaction's postings, counting itself.
    offset: u32,
    account: AccountId,
    flags: PostingFlags,
}

/// The transaction being read.
struct Pending {
    /// The header's line, where balance errors are reported.
    line: u32,
    /// The header, with every span's start set and its length still zero.
    txn: Txn,
    /// Where this transaction's costs start in the journal's costs.
    costs_start: usize,
    /// Postings read so far, counting the elided one.
    postings: u32,
    elided: Option<ElidedSlot>,
}

/// The second pass's state.
struct Parser {
    journal: Journal,
    errors: Vec<ParseError>,
    pending: Option<Pending>,
    /// Skip indented lines until the next top-level line, after a rejected
    /// transaction or an unsupported directive.
    skipping: bool,
    /// The pending transaction's balance over real postings.
    real: Vec<Residual>,
    /// The pending transaction's balance over `[bracketed]` postings.
    bracketed: Vec<Residual>,
}

impl Parser {
    fn line(&mut self, line: Line<'_>) {
        let kind = classify(line.text);
        if kind == LineKind::Indented {
            if self.skipping {
                return;
            }
            if self.pending.is_none() {
                self.error(line, 0, ParseErrorKind::OrphanPosting);
                self.skipping = true;
                return;
            }
            if let Err(err) = self.txn_line(line) {
                let col = offset_in(line.text, err.at);
                self.error(line, col, err.kind);
                self.discard();
                self.skipping = true;
            }
            return;
        }

        self.finish_txn();
        self.skipping = false;
        match kind {
            LineKind::Blank | LineKind::Indented => {}
            LineKind::Comment => {
                let text = self.journal.strings.intern(line.text[1..].trim());
                let txns_before = self.journal.txns.len() as u32;
                self.journal
                    .file_comments
                    .push(FileComment { text, txns_before });
            }
            LineKind::Header => {
                if let Err(err) = self.header(line) {
                    let col = offset_in(line.text, err.at);
                    self.error(line, col, err.kind);
                    self.skipping = true;
                }
            }
            LineKind::Price => {
                if let Err(err) = self.price(line) {
                    let col = offset_in(line.text, err.at);
                    self.error(line, col, err.kind);
                }
            }
            LineKind::Directive => {
                self.error(line, 0, ParseErrorKind::UnsupportedDirective);
                self.skipping = true;
            }
        }
    }

    fn error(&mut self, line: Line<'_>, offset: usize, kind: ParseErrorKind) {
        self.errors.push(ParseError {
            line: line.number,
            col: u32::try_from(offset + 1).unwrap_or(u32::MAX),
            kind,
        });
    }

    /// Start a transaction from its header line.
    fn header<'a>(&mut self, line: Line<'a>) -> Result<(), Located<'a>> {
        let text = line.text;
        let date_end = text.find([' ', '\t']).unwrap_or(text.len());
        let date = Date::parse(&text[..date_end]).map_err(|err| Located {
            at: text,
            kind: ParseErrorKind::Date(err),
        })?;

        let mut rest = text[date_end..].trim_start();
        let mut flag = TxnFlag::Uncleared;
        if let Some(after) = rest.strip_prefix('*') {
            flag = TxnFlag::Cleared;
            rest = after.trim_start();
        } else if let Some(after) = rest.strip_prefix('!') {
            flag = TxnFlag::Pending;
            rest = after.trim_start();
        }

        let mut code = None;
        if let Some(after) = rest.strip_prefix('(') {
            let close = after.find(')').ok_or(Located {
                at: rest,
                kind: ParseErrorKind::UnclosedCode,
            })?;
            code = Some(self.journal.strings.intern(&after[..close]));
            rest = after[close + 1..].trim_start();
        }

        let payee_start = offset_in(text, rest);
        let comment = header_comment_start(text, payee_start);
        let payee = &text[payee_start..comment.unwrap_or(text.len())];
        let payee = self.journal.strings.intern(payee.trim_end());

        let journal = &self.journal;
        self.pending = Some(Pending {
            line: line.number,
            txn: Txn {
                date,
                flag,
                code,
                payee,
                postings: span_at(journal.postings.len()),
                meta: span_at(journal.meta.len()),
                comments: span_at(journal.comments.len()),
                id: None,
            },
            costs_start: journal.costs.len(),
            postings: 0,
            elided: None,
        });
        if let Some(at) = comment {
            self.note(&text[at + 1..], None);
        }
        Ok(())
    }

    /// An indented line inside a transaction: a comment or a posting.
    fn txn_line<'a>(&mut self, line: Line<'a>) -> Result<(), Located<'a>> {
        let body = line.text.trim_start();
        if let Some(comment) = body.strip_prefix(';') {
            let posting = self
                .pending
                .as_ref()
                .and_then(|p| p.postings.checked_sub(1));
            self.note(comment, posting);
            return Ok(());
        }
        self.posting(body)
    }

    /// One posting line, its indentation removed.
    fn posting<'a>(&mut self, body: &'a str) -> Result<(), Located<'a>> {
        let parts = split_posting(body)?;
        let Self {
            journal,
            pending,
            real,
            bracketed,
            ..
        } = self;
        let pending = pending
            .as_mut()
            .expect("a posting is read inside a transaction");
        let account = journal
            .accounts
            .intern(parts.account)
            .map_err(|err| Located {
                at: parts.account,
                kind: ParseErrorKind::Account(err),
            })?;
        let offset = pending.postings;

        let Some(amount_text) = parts.amount else {
            if pending.elided.is_some() {
                return Err(Located {
                    at: body,
                    kind: ParseErrorKind::SecondElided,
                });
            }
            if parts.flags.contains(PostingFlags::VIRTUAL) {
                return Err(Located {
                    at: body,
                    kind: ParseErrorKind::ElidedVirtual,
                });
            }
            pending.elided = Some(ElidedSlot {
                offset,
                account,
                flags: parts.flags.with(PostingFlags::ELIDED),
            });
            pending.postings += 1;
            if let Some(comment) = parts.comment {
                self.note(comment, Some(offset));
            }
            return Ok(());
        };

        let amount = convert_amount(&mut journal.commodities, amount_text)?;
        journal.postings.push(Posting {
            date: pending.txn.date,
            account,
            commodity: amount.commodity,
            flags: parts.flags,
            txn: TxnId(journal.txns.len() as u32),
            qty: amount.qty,
        });
        pending.postings += 1;

        let weight = match &parts.cost {
            None => Weight {
                commodity: amount.commodity,
                qty: i128::from(amount.qty),
            },
            Some(cost) => {
                let cost = read_cost(&mut journal.commodities, cost, offset)?;
                let weight = cost_weight(&journal.commodities, amount, &cost).ok_or(Located {
                    at: parts.cost.as_ref().map_or(body, |c| c.text),
                    kind: ParseErrorKind::Amount(AmountError::Overflow),
                })?;
                journal.costs.push(cost);
                weight
            }
        };
        if parts.flags.contains(PostingFlags::BALANCED_VIRTUAL) {
            add_residual(bracketed, weight.commodity, weight.qty);
        } else if !parts.flags.contains(PostingFlags::VIRTUAL) {
            add_residual(real, weight.commodity, weight.qty);
        }

        if let Some(comment) = parts.comment {
            self.note(comment, Some(offset));
        }
        Ok(())
    }

    /// A `;` comment in the pending transaction: metadata if it reads as
    /// `key: value`, a plain comment otherwise.
    fn note(&mut self, text: &str, posting: Option<u32>) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        let text = text.trim();
        let strings = &mut self.journal.strings;
        let Some(meta) = split_meta(text) else {
            let text = strings.intern(text);
            self.journal.comments.push(Comment { text, posting });
            return;
        };
        let key = strings.intern(meta.key);
        let value = strings.intern(meta.value);
        if posting.is_none() && pending.txn.id.is_none() && meta.key == "id" {
            pending.txn.id = Some(value);
        }
        self.journal.meta.push(Meta {
            key,
            value,
            posting,
        });
    }

    /// A `P DATE COMMODITY AMOUNT` line.
    fn price<'a>(&mut self, line: Line<'a>) -> Result<(), Located<'a>> {
        let bad = |at| Located {
            at,
            kind: ParseErrorKind::BadPrice,
        };
        let text = line.text;
        let mut rest = text[1..].trim_start();
        let date_end = rest.find([' ', '\t']).ok_or(bad(rest))?;
        let date = Date::parse(&rest[..date_end]).map_err(|err| Located {
            at: rest,
            kind: ParseErrorKind::Date(err),
        })?;
        rest = rest[date_end..].trim_start();
        let commodity_at = rest;
        let name = take_commodity(&mut rest).map_err(|_| bad(commodity_at))?;
        // A trailing comment on a price is not kept.
        let value_text = match find_unquoted(rest, b';', true) {
            Some(at) => &rest[..at],
            None => rest,
        };
        let value_text = value_text.trim();
        if value_text.is_empty() {
            return Err(bad(rest));
        }
        let amount_err = |err| Located {
            at: value_text,
            kind: ParseErrorKind::Amount(err),
        };

        let commodities = &mut self.journal.commodities;
        let commodity = commodities.intern(name).map_err(amount_err)?;
        let value = AmountText::parse(value_text).map_err(amount_err)?;
        let quote = commodities.observe_style(&value).map_err(amount_err)?;
        let value = decimal(&value).map_err(amount_err)?;
        self.journal.prices.push(Price {
            date,
            commodity,
            quote,
            value,
        });
        Ok(())
    }

    /// Close the pending transaction: fill in its elided amount, check it
    /// balances, and add it to the journal, or report why not and drop it.
    fn finish_txn(&mut self) {
        let Some(pending) = self.pending.as_ref() else {
            return;
        };
        let line = pending.line;
        if let Err(kind) = self.close() {
            self.errors.push(ParseError { line, col: 1, kind });
            self.discard();
        }
        self.pending = None;
        self.real.clear();
        self.bracketed.clear();
    }

    fn close(&mut self) -> Result<(), ParseErrorKind> {
        let Self {
            journal,
            pending,
            real,
            bracketed,
            ..
        } = self;
        let pending = pending.as_mut().expect("closing a pending transaction");
        if pending.postings == 0 {
            return Err(ParseErrorKind::NoPostings);
        }
        let start = pending.txn.postings.start as usize;

        let mut expanded = 1;
        if let Some(slot) = pending.elided {
            let (fill, other) = if slot.flags.contains(PostingFlags::BALANCED_VIRTUAL) {
                (&*bracketed, &*real)
            } else {
                (&*real, &*bracketed)
            };
            check_balanced(&journal.commodities, other)?;
            for r in fill {
                if i64::try_from(-r.sum).is_err() {
                    return Err(ParseErrorKind::Amount(AmountError::Overflow));
                }
            }
            let at = start + slot.offset as usize;
            let date = pending.txn.date;
            let txn = TxnId(journal.txns.len() as u32);
            let elided = |commodity, qty| Posting {
                date,
                account: slot.account,
                commodity,
                flags: slot.flags,
                txn,
                qty,
            };
            let nonzero = fill.iter().filter(|r| r.sum != 0).count();
            if nonzero == 0 {
                // ledger-cli gives the posting a zero amount.
                let commodity = match journal.postings[start..].first() {
                    Some(posting) => posting.commodity,
                    None => journal
                        .commodities
                        .intern("")
                        .map_err(ParseErrorKind::Amount)?,
                };
                journal.postings.insert(at, elided(commodity, 0));
            } else {
                let fills = fill
                    .iter()
                    .filter(|r| r.sum != 0)
                    .map(|r| elided(r.commodity, (-r.sum) as i64));
                journal.postings.splice(at..at, fills);
                expanded = nonzero as u32;
            }
            shift_after(journal, pending, slot.offset, expanded - 1);
        } else {
            check_balanced(&journal.commodities, real)?;
            check_balanced(&journal.commodities, bracketed)?;
        }

        for cost in &mut journal.costs[pending.costs_start..] {
            cost.posting += start as u32;
        }
        let txn = &mut pending.txn;
        txn.postings.len = (journal.postings.len() - start) as u32;
        txn.meta.len = journal.meta.len() as u32 - txn.meta.start;
        txn.comments.len = journal.comments.len() as u32 - txn.comments.start;
        journal.txns.push(*txn);
        Ok(())
    }

    /// Drop everything the pending transaction added to the journal.
    fn discard(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let txn = &pending.txn;
        let journal = &mut self.journal;
        journal.postings.truncate(txn.postings.start as usize);
        journal.meta.truncate(txn.meta.start as usize);
        journal.comments.truncate(txn.comments.start as usize);
        journal.costs.truncate(pending.costs_start);
        self.real.clear();
        self.bracketed.clear();
    }
}

/// An empty span starting at `start`.
fn span_at(start: usize) -> Span {
    Span {
        start: start as u32,
        len: 0,
    }
}

/// After the elided posting at `slot` expanded into `extra + 1` postings,
/// move every later posting's metadata, comments and costs down by `extra`.
fn shift_after(journal: &mut Journal, pending: &Pending, slot: u32, extra: u32) {
    if extra == 0 {
        return;
    }
    let bump = |posting: &mut Option<u32>| {
        if let Some(p) = posting.as_mut().filter(|p| **p > slot) {
            *p += extra;
        }
    };
    for meta in &mut journal.meta[pending.txn.meta.start as usize..] {
        bump(&mut meta.posting);
    }
    for comment in &mut journal.comments[pending.txn.comments.start as usize..] {
        bump(&mut comment.posting);
    }
    for cost in &mut journal.costs[pending.costs_start..] {
        if cost.posting > slot {
            cost.posting += extra;
        }
    }
}

/// Fail with the first commodity left over, if the balance is not zero.
fn check_balanced(commodities: &Commodities, residuals: &[Residual]) -> Result<(), ParseErrorKind> {
    let Some(left) = residuals.iter().find(|r| r.sum != 0) else {
        return Ok(());
    };
    let residual = match i64::try_from(left.sum) {
        Ok(qty) => Amount {
            qty,
            commodity: left.commodity,
        }
        .display(commodities)
        .to_string(),
        Err(_) => format!("{} {}", left.sum, commodities.name(left.commodity)),
    };
    Err(ParseErrorKind::Unbalanced { residual })
}

/// Convert a posting amount at its commodity's learned precision.
fn convert_amount<'a>(commodities: &mut Commodities, text: &'a str) -> Result<Amount, Located<'a>> {
    let err = |err| Located {
        at: text,
        kind: ParseErrorKind::Amount(err),
    };
    let parsed = AmountText::parse(text).map_err(err)?;
    // Already observed by the first pass; this surfaces its errors.
    commodities.observe(&parsed).map_err(err)?;
    commodities.amount(&parsed).map_err(err)
}

/// Read a cost, keeping it at its own scale.
fn read_cost<'a>(
    commodities: &mut Commodities,
    cost: &CostText<'a>,
    offset: u32,
) -> Result<Cost, Located<'a>> {
    let err = |err| Located {
        at: cost.text,
        kind: ParseErrorKind::Amount(err),
    };
    let text = AmountText::parse(cost.text).map_err(err)?;
    let commodity = commodities.observe_style(&text).map_err(err)?;
    Ok(Cost {
        // Local to the transaction until it closes.
        posting: offset,
        kind: cost.kind,
        commodity,
        value: decimal(&text).map_err(err)?,
    })
}

/// An amount as an exact decimal at the scale it was written with.
fn decimal(text: &AmountText<'_>) -> Result<Decimal, AmountError> {
    let scale = text.decimals();
    Ok(Decimal {
        mantissa: text.qty_at(scale)?,
        scale,
    })
}

/// What a posting contributes to its transaction's balance.
struct Weight {
    commodity: CommodityId,
    qty: i128,
}

/// A posting's weight in its cost's commodity, rounded half away from zero
/// to that commodity's precision, as ledger-cli balances at display
/// precision. `None` on overflow.
fn cost_weight(commodities: &Commodities, amount: Amount, cost: &Cost) -> Option<Weight> {
    let unit_scale = i32::from(commodities.precision(amount.commodity));
    let target = i32::from(commodities.precision(cost.commodity));
    let price = i128::from(cost.value.mantissa);
    let price_scale = i32::from(cost.value.scale);
    let qty = i128::from(amount.qty);
    let (value, scale) = match cost.kind {
        CostKind::PerUnit => (qty.checked_mul(price)?, unit_scale + price_scale),
        CostKind::Total => (price.abs() * qty.signum(), price_scale),
    };
    let qty = rescale(value, target - scale)?;
    Some(Weight {
        commodity: cost.commodity,
        qty,
    })
}

/// `value * 10^exp`, rounding half away from zero when `exp` is negative.
fn rescale(value: i128, exp: i32) -> Option<i128> {
    if exp >= 0 {
        return value.checked_mul(10i128.checked_pow(exp as u32)?);
    }
    let divisor = 10i128.checked_pow(exp.unsigned_abs())?;
    let quotient = value / divisor;
    let remainder = value % divisor;
    if remainder.abs() * 2 >= divisor {
        Some(quotient + value.signum())
    } else {
        Some(quotient)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rescale_rounds_half_away_from_zero() {
        assert_eq!(rescale(12345, -2), Some(123));
        assert_eq!(rescale(12350, -2), Some(124));
        assert_eq!(rescale(-12350, -2), Some(-124));
        assert_eq!(rescale(-12349, -2), Some(-123));
        assert_eq!(rescale(7, 3), Some(7000));
    }

    #[test]
    fn meta_needs_a_key_word_ending_in_colon() {
        let meta = split_meta("id: amex:123").unwrap();
        assert_eq!((meta.key, meta.value), ("id", "amex:123"));
        assert!(split_meta("1 BTC = 30000 CAD").is_none());
        assert!(split_meta("http://example.com").is_none());
        assert!(split_meta(":tag:other:").is_none());
        let bare = split_meta("flagged:").unwrap();
        assert_eq!((bare.key, bare.value), ("flagged", ""));
    }

    #[test]
    fn account_ends_at_a_tab_or_two_spaces() {
        assert_eq!(account_end("assets:bank\t5 CAD"), "assets:bank".len());
        assert_eq!(account_end("assets:my bank  5 CAD"), "assets:my bank".len());
        assert_eq!(account_end("assets:bank ; note"), "assets:bank".len());
        assert_eq!(account_end("assets:bank"), "assets:bank".len());
    }
}
