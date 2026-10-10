# A ci.py step timeout that a cold workspace build fits inside

`ci.py` gives each step 600s, which a cold workspace build spends compiling, so
linux-test says TIMEOUT having run no test, and its steps get run by hand.

- headway:notedeck/ginger-home-mirror — a 10-min ci.py timeout on a cold workspace build (recorded in one-chrome-frame-for-tests.md)
- headway:dave/tape-swim-army — 107 min session, ~45 of it here: linux-test hit TIMEOUT twice while still compiling, then I ran notedeck_dave's step from build-and-test.yml by hand, and again after a rebase
