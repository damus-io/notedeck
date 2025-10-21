check-fmt:
	cargo fmt --all -- --config format_code_in_doc_comments=true --check

fmt:
	cargo fmt --all -- --config format_code_in_doc_comments=true

deny:
	cargo deny --version || cargo install cargo-deny
	cargo deny check bans
	cargo deny check advisories
	cargo deny check sources

check: fmt deny
	cargo check
	cargo check --features tor
	cargo check --features socks
	cargo check --target wasm32-unknown-unknown
	cargo clippy -- -D warnings
	cargo clippy --features tor -- -D warnings
	cargo clippy --features socks -- -D warnings
	cargo clippy --target wasm32-unknown-unknown -- -D warnings
