# cashflow's read-only commands run without a signing key

`transfer suggest` won't run on a scratch `--dir` without `cashflow login` or
`$CASHFLOW_NSEC`, so a real-data importer check has to mint a throwaway key.

- headway:cashflow/leaf-afraid-bulk — ~5 min: generated and encoded a throwaway nsec for a scratch `--dir` to run `transfer suggest --json` against real exports
