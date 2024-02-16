
all:
	cargo check

tags: fake
	find . -type d -name target -prune -o -type f -name '*.rs' -print | xargs ctags

.PHONY: fake
