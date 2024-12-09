use enostr::{FullKeypair, Pubkey};
use nostrdb::{Config, Ndb, NoteBuilder};
use std::fs;
use std::path::Path;

// FIXME - make nostrdb::test_util::cleanup_db accessible instead
#[allow(dead_code)]
fn cleanup_db(path: &str) {
    let p = Path::new(path);
    let _ = fs::remove_file(p.join("data.mdb"));
    let _ = fs::remove_file(p.join("lock.mdb"));
}

// managed ndb handle that cleans up test data when dropped
pub struct ManagedNdb {
    pub path: String,
    pub ndb: Ndb,
}
impl ManagedNdb {
    pub fn setup(path: &str) -> (Self, Ndb) {
        cleanup_db(path); // ensure a clean slate before starting
        let ndb = Ndb::new(path, &Config::new())
            .unwrap_or_else(|err| panic!("Failed to create Ndb at {}: {}", path, err));
        (
            Self {
                path: path.to_string(),
                ndb: ndb.clone(),
            },
            ndb,
        )
    }
}
impl Drop for ManagedNdb {
    fn drop(&mut self) {
        cleanup_db(&self.path); // comment this out to leave the db for inspection
    }
}

// generate a testdbs_path for an async test automatically
#[macro_export]
macro_rules! testdbs_path_async {
    () => {{
        fn f() {}
        fn type_name_of<T>(_: T) -> &'static str {
            core::any::type_name::<T>()
        }
        let name = type_name_of(f);

        // Find and cut the rest of the path
        let test_name = match &name[..name.len() - 3].strip_suffix("::{{closure}}") {
            Some(stripped) => match &stripped.rfind(':') {
                Some(pos) => &stripped[pos + 1..stripped.len()],
                None => &stripped,
            },
            None => &name[..name.len() - 3],
        };

        format!("target/testdbs/{}", test_name)
    }};
}

// generate a deterministic keypair for testing
pub fn test_keypair(input: u64) -> FullKeypair {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(input.to_le_bytes());
    let hash = hasher.finalize();

    let secret_key = nostr::SecretKey::from_slice(&hash).expect("valid secret key");
    let (xopk, _) = secret_key.x_only_public_key(&nostr::SECP256K1);
    let pubkey = Pubkey::new(xopk.serialize());

    FullKeypair::new(pubkey, secret_key)
}

// generate a basic raw message from scratch
pub fn raw_msg(subid: &str, keys: &FullKeypair, kind: u32, content: &str) -> String {
    let note = NoteBuilder::new()
        .kind(kind)
        .content(content)
        .sign(&keys.secret_key.to_secret_bytes())
        .build()
        .expect("note");
    format!(r#"["EVENT", "{}", {}]"#, subid, note.json().expect("json"))
}
