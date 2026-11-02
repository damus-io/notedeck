# reconcile_sync returns a NIP-42 refusal instead of falling back

The fork's reconcile_sync takes an AuthRequired/Restricted NEG-OPEN for a relay
without NIP-77, falls back to a REQ refused the same way, and returns Ok.

- headway:headway/tree-unhappy-feel — ~6 min: the CLI could never see why sns.damus.io refused it, so headway_cli/src/sync.rs check_access sends a REQ for the all-zero id first just to get the typed error; the fork fix (one match arm, propagate the refusal) was written and tested but not pushed
