# Run the real app under a private Xvfb, with one command

Hand-testing in the real app means wiring Xvfb, a throwaway datapath and
lavapipe by hand; a slip lands on, or kills, another session's display.

- headway:notedeck/virtual-leg-drama — `Xvfb :87` was already another session's display, so mine exited silently, my notedeck ran on theirs, and cleanup by display name killed their Xvfb; a script that picks a free display and kills only its own pids would have prevented both
