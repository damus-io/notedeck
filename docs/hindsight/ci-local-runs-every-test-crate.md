# ci-local runs every test crate past the first failure

linux-test is one `cargo test --workspace` without `--no-fail-fast`, so a flake
in one crate hides the rest and skips android and snapshot-test: a rerun by hand.

- headway:notedeck/next-various-calm — 36 min session, ~16 of it here: a 10-min linux-test stopped at headway's racy shared_fold_admits_owner_plaintext_but_not_a_strangers (passes alone), then a 6-min --no-fail-fast rerun plus android by hand, which found a second pre-existing failure (dave_keymap's dave.scene scope)
