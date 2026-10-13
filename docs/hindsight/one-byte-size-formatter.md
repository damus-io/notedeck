# One byte-size formatter for every crate

Columns, chrome and Dave each format "4.2 KB" privately, unreachable from the
others, so each new size display writes another copy.

- headway:dave/raise-cube-used — ~2 min: found columns' settings::format_size and chrome's debug::format_bytes unimportable and wrote a third in ui/inspector/files.rs
- headway:dave/thank-judge-flee — ~3 min: the dashboard's storage card needed byte sizes; chrome's format_bytes is behind its `memory` feature, so storage.rs wrote a fourth
