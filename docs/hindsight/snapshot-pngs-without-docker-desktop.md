# Record snapshot pngs when Docker Desktop isn't running

--update needs a Docker daemon a session can't start on a Mac, so a new
snapshot ships unrecorded and the next card's --update records it unreviewed.

- headway:personal/transfer-pave-all — the two headway_review_explainer_figures pngs couldn't be recorded (`open -a Docker` never brought the daemon up); checked them with --preview and committed the tests only
- headway:personal/grab-south-total — the review key-hints png went unrecorded after its pane gained a second record; Docker Desktop wouldn't start, so the commit ships with a --preview check only
