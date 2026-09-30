# Live rig: a visual pass of the real app

The snapshot tests (`scripts/snapshot-test`, lavapipe) render Headway inside a
test harness, not inside the chrome. For the real window, use
`scripts/headway-live-rig`. It builds a release notedeck with Headway and Dave,
and runs it on a private Xvfb display with a throwaway key. Then it seeds a demo
board through the app's embedded relay.

    scripts/headway-live-rig                 # build, launch, seed; prints the rig
    scripts/headway-live-rig shot            # screenshot of the display
    scripts/headway-live-rig session --attach
    scripts/headway-live-rig down            # stop what `up` started

Every subcommand takes the rig dir; it defaults to the last `up`.
`scripts/headway-live-rig --help` lists the options (`--size`, `--no-build`,
`--bin`, `--commits`, `--no-seed`, `--dir`).

## What `up` does

1. Builds `notedeck_chrome --features headway,dave` in release mode and copies
   the binary into the rig dir. Sibling worktrees share `target/`, so a
   rebuild there could replace the binary under a running rig.
2. Makes a throwaway key with `nak`. notedeck takes it as hex, and the CLIs
   take it as an nsec. Both go in `<dir>/rig.env`.
3. Starts Xvfb on the first free display from `:90`. Then it starts notedeck
   with `--datapath <dir>/data -r ws://127.0.0.1:9` (so it reaches no real
   relay) and `--relay-bind 127.0.0.1:<port>` on the first free port from 6690.
4. Moves the window to the origin, sizes it to the screen, and focuses it with
   `xdotool windowfocus --sync`. There is no window manager, so without this,
   keys are dropped.
5. Seeds board `headway` with the headway CLI, over the rig's relay, using its
   own cache and data dir. It adds one Backlog, one Todo and one In Progress
   card, plus one In Review card per commit from `HEAD` back. Each In Review
   card carries a review record of that commit in this checkout.
   `$AGENTIUM_SESSION` and the `HEADWAY_*` key/board variables are unset for
   these calls, so the caller's own session and keys never reach the demo.

It prints the display, window id, pids, and the teardown command.
`down` kills those pids and nothing else. Never use `pkill -x Xvfb`: other
sessions run their own displays.

## Driving it

    source <dir>/rig.env
    DISPLAY=$RIG_DISPLAY xdotool key --window $RIG_WINDOW F11

F11 opens the app drawer. Headway is its last item, below Dave. Wait about
1.5s after F11 before clicking: while the drawer is still sliding in, a click
lands on the Columns sidebar under it. On the board, `R` opens the review
queue over the seeded In Review cards.

Leader chords lapse after about 2s, and `import` can take a second, so take
screenshots at the end of a key sequence, not in the middle of one.

## A word-id'd Dave session

A review record's `agentium:` ref resolves to a live chip only if the session
exists in the app's database. To get one:

1. Start a session in Dave. It asks for a working dir, then a backend.
2. Run `scripts/headway-live-rig session`. It reads Dave's kind-31988 state
   from `<dir>/data/db` (with the `ndb` CLI), and turns each d-tag into its
   ref with `agentium id <d-tag>`. It prints the sessions newest first.
3. With `--attach`, it also records the newest session on every seeded In
   Review card, at the same commit. Records are append-only, so each card
   then holds two records of that commit. The pane shows the newer one, with
   the session chip, and a picker row of both.

`agentium id <d-tag…>` works on its own too. It is offline: it needs no key,
cache or relay.
