//! A group of parameters or of return values is a struct with named fields,
//! never a tuple.
//!
//! A tuple parameter lowers the count `too_many_arguments` sees without making
//! the function take any less, and a tuple's parts have no names where it is
//! built or read. This parses every source file in the workspace and fails on
//! a `fn` whose parameter is a tuple behind any number of references, a `fn`
//! whose return type holds a tuple anywhere (`Option<(A, B)>`, `impl
//! Iterator<Item = (A, B)>`), and a `type` alias holding one, which would hide
//! it from the other two. Closures are not signatures and are not read.
//!
//! The crates in [`NOT_YET`] still hold tuples and are skipped until their
//! cards under headway:notedeck/foil-genre-inner convert them. Every other
//! crate, a new one included, is checked. Run it with
//! `cargo test -p bin_testing --test signatures`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use syn::visit::Visit;

/// Crates, by directory under `crates/`, that still hold a tuple in a
/// signature. Converting one removes it here; a listed crate that is already
/// clean fails [`not_yet_lists_only_crates_that_hold_a_tuple`], so the list
/// only shrinks. When it is empty, delete it.
const NOT_YET: &[&str] = &[
    "agentium-core",
    "agentium_cli",
    "headway",
    "headway_cli",
    "notedeck_dashboard",
    "notedeck_dave",
    "notedeck_headway",
    "notedeck_messages",
    "notedeck_nostrverse",
    "notedeck_release",
    "protoverse",
];

/// The workspace root, so offenders print as `crates/<crate>/src/…`.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the workspace root exists")
}

/// Every crate directory under `crates/`: one holding a `Cargo.toml`.
fn crates(root: &Path) -> Vec<PathBuf> {
    let dir = root.join("crates");
    let mut out: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("the workspace has a crates directory")
        .map(|entry| entry.expect("a readable entry").path())
        .filter(|path| path.join("Cargo.toml").is_file())
        .collect();
    out.sort();
    out
}

/// Every `.rs` under a crate's `src`, `tests`, `examples` and `benches`.
fn sources(krate: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.expect("a readable entry").path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    for sub in ["src", "tests", "examples", "benches"] {
        walk(&krate.join(sub), &mut out);
    }
    out.sort();
    out
}

/// The crate's directory name, as [`NOT_YET`] spells it.
fn crate_name(krate: &Path) -> &str {
    krate
        .file_name()
        .and_then(|n| n.to_str())
        .expect("a crate directory has a UTF-8 name")
}

/// Every tuple signature in `krate`'s sources, one line each.
fn crate_hits(root: &Path, krate: &Path) -> Vec<String> {
    let mut hits = Vec::new();
    for path in sources(krate) {
        let name = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();
        let text = std::fs::read_to_string(&path).expect("a readable source file");
        hits.extend(tuple_signatures(&name, &text));
    }
    hits
}

/// The tuple a parameter's type is, seen through references and parentheses.
fn tuple_of(mut ty: &syn::Type) -> Option<&syn::TypeTuple> {
    loop {
        match ty {
            syn::Type::Reference(r) => ty = &r.elem,
            syn::Type::Paren(p) => ty = &p.elem,
            syn::Type::Group(g) => ty = &g.elem,
            syn::Type::Tuple(t) if t.elems.len() > 1 => return Some(t),
            _ => return None,
        }
    }
}

/// The first tuple anywhere in a return type: bare, behind a reference, or as a
/// type argument such as `Option<(A, B)>` or `impl Iterator<Item = (A, B)>`.
fn tuple_within(ty: &syn::Type) -> Option<&syn::TypeTuple> {
    struct First<'ast>(Option<&'ast syn::TypeTuple>);
    impl<'ast> Visit<'ast> for First<'ast> {
        fn visit_type_tuple(&mut self, t: &'ast syn::TypeTuple) {
            if self.0.is_none() && t.elems.len() > 1 {
                self.0 = Some(t);
            }
            syn::visit::visit_type_tuple(self, t);
        }
    }
    let mut first = First(None);
    first.visit_type(ty);
    first.0
}

/// `file:line fn name(parameter)` for each tuple parameter in `text`,
/// `file:line fn name -> ..` for each return type holding a tuple, and
/// `file:line type Name = ..` for each alias holding one, which would hide it.
fn tuple_signatures(file: &str, text: &str) -> Vec<String> {
    struct Finder<'a> {
        file: &'a str,
        found: Vec<String>,
    }
    impl<'ast> Visit<'ast> for Finder<'_> {
        fn visit_signature(&mut self, sig: &'ast syn::Signature) {
            for input in &sig.inputs {
                let syn::FnArg::Typed(arg) = input else {
                    continue;
                };
                if let Some(tuple) = tuple_of(&arg.ty) {
                    let line = tuple.paren_token.span.open().start().line;
                    let pat = quote_pat(&arg.pat);
                    self.found
                        .push(format!("{}:{line} fn {}({pat})", self.file, sig.ident));
                }
            }
            if let syn::ReturnType::Type(_, ty) = &sig.output {
                if let Some(tuple) = tuple_within(ty) {
                    let line = tuple.paren_token.span.open().start().line;
                    self.found
                        .push(format!("{}:{line} fn {} -> ..", self.file, sig.ident));
                }
            }
            syn::visit::visit_signature(self, sig);
        }
        fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
            self.alias(&item.ident, &item.ty);
            syn::visit::visit_item_type(self, item);
        }
        fn visit_impl_item_type(&mut self, item: &'ast syn::ImplItemType) {
            self.alias(&item.ident, &item.ty);
            syn::visit::visit_impl_item_type(self, item);
        }
    }
    impl Finder<'_> {
        fn alias(&mut self, name: &syn::Ident, ty: &syn::Type) {
            if let Some(tuple) = tuple_within(ty) {
                let line = tuple.paren_token.span.open().start().line;
                self.found
                    .push(format!("{}:{line} type {name} = ..", self.file));
            }
        }
    }
    let ast = syn::parse_file(text).unwrap_or_else(|e| panic!("{file} does not parse: {e}"));
    let mut finder = Finder {
        file,
        found: Vec::new(),
    };
    finder.visit_file(&ast);
    finder.found
}

/// A parameter's pattern as written, near enough to find it by.
fn quote_pat(pat: &syn::Pat) -> String {
    match pat {
        syn::Pat::Ident(i) => i.ident.to_string(),
        syn::Pat::Tuple(t) => {
            let parts: Vec<_> = t.elems.iter().map(quote_pat).collect();
            format!("({})", parts.join(", "))
        }
        syn::Pat::Wild(_) => "_".into(),
        _ => "..".into(),
    }
}

#[test]
fn no_signature_holds_a_tuple() {
    let root = root();
    let gated: Vec<PathBuf> = crates(&root)
        .into_iter()
        .filter(|krate| !NOT_YET.contains(&crate_name(krate)))
        .collect();
    let files: usize = gated.iter().map(|krate| sources(krate).len()).sum();
    assert!(
        files > 20,
        "the scanner found {files} files in the gated crates, not a workspace"
    );

    let mut offenders = String::new();
    for krate in &gated {
        for hit in crate_hits(&root, krate) {
            let _ = writeln!(offenders, "  {hit}");
        }
    }

    assert!(
        offenders.is_empty(),
        "a signature holding a tuple:\n{offenders}\n\
         Group the parameters, or what comes back, in a struct whose fields name them, and \
         name the struct for the thing its users share (a diff's two sides, a mesh's \
         geometry), not `FooArgs`. Reuse it where the same group recurs. If a function still \
         takes more than clippy's seven once grouped, it is doing too much: split it. A tuple \
         here hides the count from `too_many_arguments` and the names from every caller."
    );
}

#[test]
fn not_yet_lists_only_crates_that_hold_a_tuple() {
    let root = root();
    let krates = crates(&root);
    let mut stale = String::new();
    for name in NOT_YET {
        let Some(krate) = krates.iter().find(|k| crate_name(k) == *name) else {
            let _ = writeln!(stale, "  {name}: no such crate under crates/");
            continue;
        };
        if crate_hits(&root, krate).is_empty() {
            let _ = writeln!(stale, "  {name}: holds no tuple now, so check it");
        }
    }
    assert!(
        stale.is_empty(),
        "NOT_YET in this file names crates it should not:\n{stale}\
         Remove them from the list, so the gate covers them."
    );
}

#[test]
fn the_scanner_sees_a_planted_tuple() {
    // A scanner that matched nothing would pass in silence forever.
    let planted = "
        fn a(x: u32, (y, z): (u32, u32)) {}
        struct S;
        impl S { fn b(&self, pair: &(u8, u8)) {} }
        trait T { fn c(_: (i8, i8)); }
        fn clean(x: (u32,), f: impl Fn((u8, u8)), v: &[(u8, u8)]) -> Option<(u32,)> { None }
        fn closure() { let _ = |(a, b): (u8, u8)| -> (u8, u8) { (a, b) }; }
        fn d() -> (u8, u8) { (0, 0) }
        fn e() -> Result<Vec<(u8, u8)>, ()> { Ok(vec![]) }
        impl S { fn f(&self) -> impl Iterator<Item = (u8, &u8)> { [].into_iter() } }
        type G = [(u8, u8); 2];
        fn unit() -> Result<(), ()> { Ok(()) }
    ";
    let hits = tuple_signatures("planted.rs", planted);
    let want = [
        "planted.rs:2 fn a((y, z))",
        "planted.rs:4 fn b(pair)",
        "planted.rs:5 fn c(_)",
        "planted.rs:8 fn d -> ..",
        "planted.rs:9 fn e -> ..",
        "planted.rs:10 fn f -> ..",
        "planted.rs:11 type G = ..",
    ];
    assert_eq!(
        hits, want,
        "the scanner stopped finding tuples in signatures, or started finding things that are not"
    );
}
