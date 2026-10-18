# ledgerdb sums an account's own postings, not just its subtree

`Query::flows` and `top_n` total only whole subtrees, so a reader that needs one
account's own postings queries every account and subtracts its children's.

- headway:cashflow/borrow-vote-brass — ~6 min: the Projects model (views/projects/model.rs `group`) walks every account under income/routed/expenses into a `HashMap<AccountId, Amounts>` of subtree sums (three `flows` each) just to take children back out; the rollups already hold per-account cells
