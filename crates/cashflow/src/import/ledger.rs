//! The `ledger` importer: a ledger-cli file, already in ledgerdb's subset,
//! taken as one batch.
//!
//! This is the stub the importer registry ships with. Every transaction must
//! already carry a `; id:` and the file must parse cleanly; synthesizing
//! legacy ids for files without them, and reporting parse errors per line
//! instead of refusing, is headway:cashflow/travel-size-carpet.

use std::path::Path;

use ledgerdb::{Journal, Parsed};

use super::{Batch, ImportError, ImportInput, Importer};

/// Reads one ledger-cli file whose transactions all carry `; id:`.
pub struct LedgerImporter;

impl Importer for LedgerImporter {
    fn name(&self) -> &'static str {
        "ledger"
    }

    fn layer(&self) -> &'static str {
        "ledger"
    }

    fn import(&self, input: &ImportInput) -> Result<Batch, ImportError> {
        let [path] = input.paths.as_slice() else {
            return Err(ImportError::Usage(
                "`import ledger` takes exactly one <file>".to_owned(),
            ));
        };
        import_file(path)
    }
}

fn import_file(path: &Path) -> Result<Batch, ImportError> {
    let text = std::fs::read_to_string(path).map_err(|source| ImportError::Read {
        path: path.to_owned(),
        source,
    })?;
    let Parsed { journal, errors } = Journal::parse(&text);
    if !errors.is_empty() {
        return Err(ImportError::Parse {
            path: path.to_owned(),
            errors,
        });
    }
    if let Some(txn) = journal.txns.iter().find(|txn| txn.id.is_none()) {
        return Err(ImportError::MissingId {
            path: path.to_owned(),
            date: txn.date,
            payee: journal.strings.get(txn.payee).to_owned(),
        });
    }
    Ok(Batch::new(journal))
}
