# nostrdb tag strings live as long as the note they came from

`Tag::get_str` promises `&'n str`, but a built note's bytes read as zeros
after the tag loop, so every parser must copy inside it or test-only fails.

- headway:personal/juice-road-castle — ~10 min: `parse_review_source` kept the `name` borrow past the tag loop, and the roundtrip test read back `"\0\0\0\0"`. Probed the built note's JSON (fine) before finding the borrow
- headway:notedeck/same-surprise-bunker — ~5 min: rewrote `updater::nostr::parse_asset_event` to borrow its tags past the loop to save copies, then read this file and put the copies back; nothing in the type says the borrow is unsafe to keep
