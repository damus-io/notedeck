# The queue back-off test waits out its notice reliably

chrome_nav_loop_queue_back_off_keeps_its_notice times out in about half of runs on
the base commit too, so a red headway suite needs a base re-run to clear you.

- headway:personal/grab-south-total — ~5 min: four runs on the base commit to prove its 2-in-4 failure predates the `{ }` commit keys
