# A vault write that finds the stored revision through the open transaction

Under an open read txn, `Vault::write` stamps blind and can lose the newest-tie.
Each caller threads the stored `created_at` through as `after` by hand instead.

- headway:cashflow/occur-then-item — ~6 min: `manifest::write` retiring a manifest read in the same txn stayed unretired in the test; found by printing stamps, fixed by threading `after` through every manifest write
