# notedeck_dashboard's snapshot test renders against a fixed clock

Its "updated Ns" header reads the wall clock, so the suite never matches and is
off in `scripts/snapshot-test`, leaving dashboard changes with no visual check.

- headway:cashflow/elder-spirit-fly — ~8 min: the "no visual change" check for moving the charts into notedeck_ui failed on the committed pngs; had to render the base to scratch and diff against it to see only the "updated 61s" label and the refresh button differ
