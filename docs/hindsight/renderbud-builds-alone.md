# renderbud builds and tests on its own

renderbud takes wgpu without `wgsl`, so `cargo test -p renderbud` fails on
`ShaderSource::Wgsl` unless a dependent crate in the build unifies it in.

- headway:notedeck/tool-parade-multiply — ~2 min: `cargo test -p renderbud` failed with five E0599s on code nobody touched; reran as `-p renderbud -p notedeck_nostrverse` to get the feature
- headway:cashflow/firm-tuition-better — ~1 min: `cargo check -p renderbud --tests` failed on `ShaderSource::Wgsl` after adding `upload_mesh`; checked it through `-p notedeck_cashflow` instead
