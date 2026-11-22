# One encrypted-imeta type below notedeck

headway can't depend on notedeck, so it copies `EncryptedImeta`'s imeta tag code.
Two copies can drift, and only a notedeck_headway test would notice.

- headway:ux-feedback/shadow-camera-vendor — wrote a second imeta parser/writer in `crates/headway/src/event/attachment.rs` plus `tests/attachment_compat.rs` to pin it against notedeck's; a notedeck-free crate (or `nostrdb_net`) holding the tag shape would have been one import
- headway:ux-feedback/torch-legend-session — wrote `attach::imeta` and `attach::attachment` in `notedeck_headway/src/ui/attach.rs` to convert `Attachment`↔`EncryptedImeta` field by field (nonce enum, `Dims`↔`PixelDimensions`), and the snapshot test hand-maps an `EncryptedBlob` into an `Attachment` a third time
- headway:ux-feedback/guess-kiwi-cause — `nd headway attach` hands the attachment to the comment tool as text, so `Attachment::imeta_text`/`from_imeta_text` (`crates/headway/src/event/attachment.rs`) are a third spelling of the same tag; a shared type could have owned its text form once
