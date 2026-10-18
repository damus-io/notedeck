#!/usr/bin/env bash
#
# Upload the Git LFS objects (the snapshot pngs) a push references. Without
# this a push sends only the pointers, and everyone who fetches it gets a 404
# for each png. Installed by scripts/dev_setup.sh.

if ! command -v git-lfs >/dev/null 2>&1; then
    echo >&2 "error: notedeck keeps its snapshot pngs in Git LFS, but git-lfs is not on your PATH,"
    echo >&2 "  so this push would send pointers without their objects. Install git-lfs (see README)."
    exit 2
fi
exec git lfs pre-push "$@"
