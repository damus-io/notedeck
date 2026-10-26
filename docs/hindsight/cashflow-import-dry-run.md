# cashflow import --dry-run

`cashflow import` always writes a segment, so probing an importer against a
live source means a scratch `$CASHFLOW_DIR` with config and secret copied in.

- headway:cashflow/picnic-gasp-woman — about 60 single-month appstore probes against Apple, each needing a scratch CASHFLOW_DIR with config.toml and appstore.p8 copied in, so their segments stayed out of the real journal
