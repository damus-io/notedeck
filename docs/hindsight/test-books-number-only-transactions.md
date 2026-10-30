# cashflow's test books number only transactions

`test_util::books_of` ids every unindented line, a `P` price line too, so a
priced fixture's transactions are numbered one past where they read.

- headway:cashflow/blue-liar-minor — ~2 min: `detail::tests::home_amounts_convert_when_a_rate_exists` put a `P` line first and asked for `t:4`, got the card payment instead of the USD charge, and had to ask for `t:5` with a comment
