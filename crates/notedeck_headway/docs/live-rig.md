# Live rig: a visual pass of the real app

The snapshot tests (`scripts/snapshot-test`, lavapipe) render Headway inside a
test harness, not inside the chrome. For the real app, use
`scripts/headway-live-rig`. It builds a release notedeck with Headway and Dave,
and runs it headless with a throwaway key. Then it seeds a demo board through
the app's embedded relay. `shot` films the real app offscreen, so none of this
needs a display, Xvfb or xdotool.

    scripts/headway-live-rig                 # build, launch, seed; prints the rig
    scripts/headway-live-rig shot            # PNG of the board
    scripts/headway-live-rig session --attach
    scripts/headway-live-rig down            # stop what `up` started

Every subcommand takes the rig dir; it defaults to the last `up`.
`scripts/headway-live-rig --help` lists the options (`--size`, `--no-build`,
`--bin`, `--commits`, `--no-seed`, `--dir`, `--xvfb`).

## What `up` does

1. Builds `notedeck_chrome --features headway,dave` in release mode and copies
   the binary into the rig dir. Sibling worktrees share `target/`, so a
   rebuild there could replace the binary under a running rig.
2. Makes a throwaway key with `nak`. notedeck takes it as hex, and the CLIs
   take it as an nsec. Both go in `<dir>/rig.env`.
3. Starts `notedeck --headless` with `--datapath <dir>/data -r
   ws://127.0.0.1:9` (so it reaches no real relay) and `--relay-bind
   127.0.0.1:<port>` on the first free port from 6690.
4. Seeds board `headway` with the headway CLI, over the rig's relay, using its
   own cache and data dir. It adds one Backlog, one Todo and one In Progress
   card, plus one In Review card per commit from `HEAD` back. Each In Review
   card carries a review record of that commit in this checkout.
   `$AGENTIUM_SESSION` and the `HEADWAY_*` key/board variables are unset for
   these calls, so the caller's own session and keys never reach the demo.

   A headless app runs every app's update, so Headway has usually made the
   board already; the CLI then only titles it "Live rig".

It prints the relay, the pid, and the teardown command. `down` kills that pid
and nothing else.

## Shooting it

`shot [dir] [out.png] [steps...]` stops the live app, so only one process
has its database open, and films a fresh one from the same data with
`notedeck --shot`. Then it starts the live app again. With no steps, it
opens Headway from the app drawer and shoots the board. Steps are notedeck's
shot flags, run in order (see `crates/notedeck_chrome/src/shot/script.rs`):

    --wait <secs>      run frames for that long (1.5, 1.5s, 500ms)
    --frames <n>       run that many frames
    --key <keys>       press keys, in the keymap's syntax: '<C-k>', 'g g', '<Enter>'
    --type <text>      type text into whatever has focus
    --click <x>,<y>    click there, in the PNG's pixels
    --hover <x>,<y>    move the pointer there
    --shot <out.png>   write the window as it is now (repeatable)

The review queue, for example:

    scripts/headway-live-rig shot <dir> review.png --wait 2 --key '<F11>' \
        --wait 1.5 --click 97,539 --wait 2 --key R --wait 1

F11 opens the app drawer, and Headway is its last item, at (97, 539). Wait
about 1.5s after F11 before clicking: while the drawer is still sliding in, a
click lands on the Columns sidebar under it. On the board, `R` opens the
review queue over the seeded In Review cards. Don't open Headway from the
Cmd-K bar yet: it panics (headway:headway/fetch-mind-series).

The same flags work on any notedeck, without the rig, for any app:

    notedeck --datapath <d> --wait 1 --key '<C-Space>' --wait 0.3 --shot out.png

The shot runs the same `Notedeck` and chrome as the window, through egui's
wgpu renderer with eframe's options, into an offscreen texture. Against the
same view under Xvfb it differs only in the caret's blink phase and in
one-bit dither noise on antialiased edges.

## Driving it by hand

`up --xvfb` runs the app in a window on a private Xvfb display instead, for
poking at it interactively. It starts Xvfb on the first free display from
`:90`, then moves the window to the origin, sizes it to the screen, and
focuses it with `xdotool windowfocus --sync`. There is no window manager, so
without this, keys are dropped.

    source <dir>/rig.env
    DISPLAY=$RIG_DISPLAY xdotool key --window $RIG_WINDOW F11

`shot` on such a rig screenshots the display with `import` and takes no steps.
Leader chords lapse after about 2s, and `import` can take a second, so take
screenshots at the end of a key sequence, not in the middle of one. Never use
`pkill -x Xvfb`: other sessions run their own displays.

## A word-id'd Dave session

A review record's `agentium:` ref resolves to a live chip only if the session
exists in the app's database. To get one:

1. Start a session in Dave, on an `--xvfb` rig. It asks for a working dir,
   then a backend.
2. Run `scripts/headway-live-rig session`. It reads Dave's kind-31988 state
   from `<dir>/data/db` (with the `ndb` CLI), and turns each d-tag into its
   ref with `agentium id <d-tag>`. It prints the sessions newest first.
3. With `--attach`, it also records the newest session on every seeded In
   Review card, at the same commit. Records are append-only, so each card
   then holds two records of that commit. The pane shows the newer one, with
   the session chip, and a picker row of both.

`agentium id <d-tag…>` works on its own too. It is offline: it needs no key,
cache or relay.
