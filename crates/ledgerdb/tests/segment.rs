//! Content-addressed journal segments and the store that files them.

use std::fs;
use std::path::Path;

use ledgerdb::{
    EncodeError, HeaderError, Journal, JournalStore, Parsed, Segment, SegmentHash, SegmentHeader,
    StoreError, TxnId,
};

/// Parse `text` and fail on any error.
fn parse_ok(text: &str) -> Journal {
    let Parsed { journal, errors } = Journal::parse(text);
    assert!(errors.is_empty(), "unexpected errors: {errors:#?}\n{text}");
    journal
}

fn header(source: &str, imported: u64) -> SegmentHeader {
    SegmentHeader {
        source: source.to_owned(),
        imported,
        layer: "cards".to_owned(),
    }
}

/// A batch of `n` synthetic transactions whose ids and amounts start at
/// `first`.
fn batch(first: u32, n: u32) -> Journal {
    let mut text = String::new();
    for i in first..first + n {
        text.push_str(&format!(
            "2024-01-{:02} * Shop {i}\n    ; id: test:{i}\n    ; raw-merchant: shop {i}\n    liabilities:card  -{i}.50 CAD\n    expenses:unknown\n\n",
            i % 28 + 1
        ));
    }
    parse_ok(&text)
}

fn store_in(dir: &Path) -> JournalStore {
    JournalStore::new(dir.join("journal"))
}

#[test]
fn encode_is_a_fixed_point_and_parses_back() {
    let journal = parse_ok(
        "; a note about the batch\n\nP 2024-01-01 BTC 61234.56789 CAD\n\n2024-01-02 * Payee\n  ; id: test:1\n  assets:bank  -4.5 CAD\n  expenses:food\n",
    );
    let header = header("bank", 1_760_000_000);
    let text = Segment::encode(&header, &journal).unwrap();
    assert_eq!(
        text,
        "; ledgerdb-segment: 1\n; source: bank\n; imported: 1760000000\n; layer: cards\n\n\
         ; a note about the batch\n\nP 2024-01-01 BTC 61234.56789 CAD\n\n\
         2024-01-02 * Payee\n\t; id: test:1\n\tassets:bank\t-4.5 CAD\n\texpenses:food\n"
    );

    let hash = SegmentHash::of(text.as_bytes());
    let segment = Segment::parse(hash, &text).unwrap();
    assert!(segment.errors.is_empty());
    assert_eq!(segment.header, header);
    assert_eq!(
        segment.journal.file_comments.len(),
        1,
        "header lines removed"
    );
    assert_eq!(segment.journal.txns.len(), 1);
    assert_eq!(
        Segment::encode(&segment.header, &segment.journal).unwrap(),
        text
    );
}

#[test]
fn encode_refuses_what_would_not_read_back() {
    let journal = parse_ok("2024-01-02 Payee\n  a  1 CAD\n  b\n");
    assert_eq!(
        Segment::encode(&header("bank", 1), &journal),
        Err(EncodeError::MissingId(TxnId(0)))
    );
    let mut bad = header("bank", 1);
    bad.layer = "two\nlines".to_owned();
    assert_eq!(
        Segment::encode(&bad, &batch(1, 1)),
        Err(EncodeError::Header(HeaderError::BadValue("layer")))
    );
}

#[test]
fn parse_rejects_a_missing_or_newer_header() {
    let hash = SegmentHash::of(b"");
    let err = Segment::parse(hash, "2024-01-02 Payee\n  a  1 CAD\n  b\n").unwrap_err();
    assert_eq!(err, HeaderError::Missing("ledgerdb-segment"));
    let newer = "; ledgerdb-segment: 2\n; source: a\n; imported: 1\n; layer: b\n";
    assert_eq!(
        Segment::parse(hash, newer).unwrap_err(),
        HeaderError::UnsupportedVersion("2".to_owned())
    );
    let swapped = "; ledgerdb-segment: 1\n; layer: b\n; source: a\n; imported: 1\n";
    assert_eq!(
        Segment::parse(hash, swapped).unwrap_err(),
        HeaderError::Missing("source")
    );
}

#[test]
fn write_then_read_round_trips_and_names_the_file_by_hash() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_in(tmp.path());
    assert!(store.list().unwrap().is_empty(), "a missing dir is empty");

    let journal = batch(1, 3);
    let header = header("card", 42);
    let hash = store.write_segment(&header, &journal).unwrap();

    let path = store.path_of(hash);
    let name = hash.to_string();
    assert_eq!(
        path,
        tmp.path()
            .join("journal")
            .join(&name[..2])
            .join(format!("{name}.ledger"))
    );
    let bytes = fs::read(&path).unwrap();
    assert_eq!(SegmentHash::of(&bytes), hash);
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        Segment::encode(&header, &journal).unwrap()
    );

    let segment = store.read(hash).unwrap();
    assert_eq!(segment.hash, hash);
    assert_eq!(segment.header, header);
    assert_eq!(segment.journal.txns.len(), 3);
    assert_eq!(store.list().unwrap(), vec![hash]);

    // Writing it again changes nothing, and the read copy hashes the same.
    assert_eq!(store.write_segment(&header, &journal).unwrap(), hash);
    assert_eq!(
        store
            .write_segment(&segment.header, &segment.journal)
            .unwrap(),
        hash
    );
    assert_eq!(store.list().unwrap(), vec![hash]);
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn a_tampered_file_is_rejected_by_name() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_in(tmp.path());
    let good = store
        .write_segment(&header("card", 1), &batch(1, 2))
        .unwrap();
    let edited = store
        .write_segment(&header("card", 2), &batch(5, 2))
        .unwrap();

    let path = store.path_of(edited);
    let text = fs::read_to_string(&path).unwrap().replace("5.50", "6.50");
    fs::write(&path, text).unwrap();

    let err = store.read(edited).unwrap_err();
    let StoreError::HashMismatch { path: named, .. } = &err else {
        panic!("expected a hash mismatch, got {err:?}");
    };
    assert_eq!(named, &path);
    assert!(err.to_string().contains(&edited.to_string()));

    let report = store.verify_all().unwrap();
    assert_eq!(report.ok, vec![good]);
    assert_eq!(report.bad.len(), 1);
}

#[test]
fn stray_files_are_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_in(tmp.path());
    let hash = store
        .write_segment(&header("card", 1), &batch(1, 1))
        .unwrap();
    let name = hash.to_string();
    let dir = store.dir();

    // A temp file, an rsync partial, a .git dir, a non-hash name, an
    // uppercase name, and a real name in the wrong shard.
    let shard = dir.join(&name[..2]);
    fs::write(shard.join(".tmpabc"), "x").unwrap();
    fs::write(shard.join(format!(".{name}.ledger.Xa1b2")), "x").unwrap();
    fs::create_dir_all(dir.join(".git/objects")).unwrap();
    fs::write(dir.join(".git/HEAD"), "ref").unwrap();
    fs::write(shard.join("notes.ledger"), "x").unwrap();
    fs::write(shard.join(format!("{}.ledger", name.to_uppercase())), "x").unwrap();
    let wrong = if &name[..2] == "00" { "01" } else { "00" };
    fs::create_dir_all(dir.join(wrong)).unwrap();
    fs::copy(
        store.path_of(hash),
        dir.join(wrong).join(format!("{name}.ledger")),
    )
    .unwrap();

    assert_eq!(store.list().unwrap(), vec![hash]);
    assert!(store.verify_all().unwrap().bad.is_empty());
}

/// Copy every file under `from` to the same place under `to`, as a plain
/// file sync would.
fn copy_tree(from: &Path, to: &Path) {
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            fs::create_dir_all(&target).unwrap();
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn two_stores_union_by_copying_files() {
    let tmp = tempfile::tempdir().unwrap();
    let a = JournalStore::new(tmp.path().join("a"));
    let b = JournalStore::new(tmp.path().join("b"));
    let shared = a.write_segment(&header("card", 1), &batch(1, 2)).unwrap();
    assert_eq!(
        b.write_segment(&header("card", 1), &batch(1, 2)).unwrap(),
        shared,
        "the same batch has the same name on both"
    );
    let only_a = a.write_segment(&header("card", 2), &batch(10, 2)).unwrap();
    let only_b = b.write_segment(&header("bank", 3), &batch(20, 2)).unwrap();

    copy_tree(b.dir(), a.dir());
    copy_tree(a.dir(), b.dir());

    let mut all = vec![shared, only_a, only_b];
    all.sort();
    assert_eq!(a.list().unwrap(), all);
    assert_eq!(b.list().unwrap(), all);
    let txns: usize = all
        .iter()
        .map(|&hash| a.read(hash).unwrap().journal.txns.len())
        .sum();
    assert_eq!(txns, 6);
    assert_eq!(a.verify_all().unwrap().ok, all);
}

#[test]
fn admit_from_staging_checks_every_file_and_leaves_staging_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_in(tmp.path());
    let remote = JournalStore::new(tmp.path().join("remote"));
    let present = store
        .write_segment(&header("card", 1), &batch(1, 1))
        .unwrap();
    assert_eq!(
        remote
            .write_segment(&header("card", 1), &batch(1, 1))
            .unwrap(),
        present
    );
    let sharded = remote
        .write_segment(&header("card", 2), &batch(2, 1))
        .unwrap();
    let tampered = remote
        .write_segment(&header("card", 3), &batch(3, 1))
        .unwrap();

    // Stage the remote's tree as rsync would, plus one flat file and one
    // file whose name is a real hash but whose body is not a segment.
    let staging = tmp.path().join("staging");
    fs::create_dir_all(&staging).unwrap();
    copy_tree(remote.dir(), &staging);
    let flat_text = Segment::encode(&header("bank", 4), &batch(4, 1)).unwrap();
    let flat = SegmentHash::of(flat_text.as_bytes());
    fs::write(staging.join(format!("{flat}.ledger")), &flat_text).unwrap();
    let junk = SegmentHash::of(b"not a segment");
    fs::write(staging.join(format!("{junk}.ledger")), "not a segment").unwrap();
    let tampered_path = {
        let name = tampered.to_string();
        staging.join(&name[..2]).join(format!("{name}.ledger"))
    };
    let edited = fs::read_to_string(&tampered_path)
        .unwrap()
        .replace("3.50", "9.50");
    fs::write(&tampered_path, &edited).unwrap();

    let report = store.admit_from(&staging).unwrap();
    let mut admitted = vec![sharded, flat];
    admitted.sort();
    assert_eq!(report.admitted, admitted);
    assert_eq!(report.present, vec![present]);
    assert_eq!(report.rejected.len(), 2, "{:#?}", report.rejected);
    assert!(report.rejected.iter().any(|err| matches!(
        err,
        StoreError::HashMismatch { path, .. } if path == &tampered_path
    )));
    assert!(report.rejected.iter().any(|err| matches!(
        err,
        StoreError::Header {
            source: HeaderError::Missing(_),
            ..
        }
    )));

    let mut listed = vec![present, sharded, flat];
    listed.sort();
    assert_eq!(store.list().unwrap(), listed);
    assert!(!store.contains(tampered));
    assert_eq!(fs::read_to_string(&tampered_path).unwrap(), edited);
    assert!(staging.join(format!("{flat}.ledger")).is_file());

    // Admitting again finds everything already there.
    let again = store.admit_from(&staging).unwrap();
    assert!(again.admitted.is_empty());
    assert_eq!(again.present, listed);
    assert!(
        store
            .admit_from(&tmp.path().join("missing"))
            .unwrap()
            .admitted
            .is_empty()
    );
}

#[test]
fn concurrent_duplicate_writes_are_safe() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_in(tmp.path());
    let journal = batch(1, 50);
    let header = header("card", 7);
    let expected = SegmentHash::of(Segment::encode(&header, &journal).unwrap().as_bytes());

    let hashes: Vec<SegmentHash> = std::thread::scope(|scope| {
        let writers: Vec<_> = (0..16)
            .map(|_| scope.spawn(|| store.write_segment(&header, &journal).unwrap()))
            .collect();
        writers.into_iter().map(|w| w.join().unwrap()).collect()
    });
    assert!(hashes.iter().all(|&hash| hash == expected));

    assert_eq!(store.list().unwrap(), vec![expected]);
    let shard = store.path_of(expected).parent().unwrap().to_owned();
    let names: Vec<_> = fs::read_dir(&shard)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "no temp files left behind: {names:?}");
    assert_eq!(store.read(expected).unwrap().journal.txns.len(), 50);
}
