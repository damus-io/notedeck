# A git helper that feeds stdin

`headway::git`'s `git`/`git_bytes` take only args, so each stdin-fed command
(`lfs smudge`, `cat-file --batch`, `check-attr --stdin`) builds its own Command.

- headway:notedeck/base-hurdle-camp — ~3 min: a piped Command for `git lfs smudge` in headway/src/git/lfs.rs, and a writer-thread one for `cat-file --batch` and `check-attr --stdin` in bin_testing's lfs_snapshots test
