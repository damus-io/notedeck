fmt:
	cargo fmt --all -- --config format_code_in_doc_comments=true

check: fmt check-stable check-msrv

check-fmt:
	cargo fmt --all -- --config format_code_in_doc_comments=true --check

check-stable:
	@bash contrib/scripts/check.sh

check-msrv:
	@bash contrib/scripts/check.sh msrv

clean:
	cargo clean

loc:
	@echo "--- Counting lines of .rs files (LOC):" && find src/ -type f -name "*.rs" -exec cat {} \; | wc -l