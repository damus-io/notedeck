---
name: cashflow
description: Categorize jb55's personal finance ledger from the command line via the `cashflow` CLI (crates/cashflow_cli) — burn down the uncategorized ("unknown") transactions with rules, overrides, splits and transfer links, previewing every write first. Use when asked to categorize transactions, clear the triage queue, add or fix a categorization rule, link transfers between own accounts, or review what an agent session categorized — e.g. "categorize my amex rows", "burn down the unknowns", "why is this charge in expenses:unknown", "undo what that session did".
---

# Categorizing with the cashflow CLI

`cashflow` reads raw, content-addressed journal segments from `$CASHFLOW_DIR`
(default `~/.local/share/cashflow`) and **derives** every transaction's
category at load time from the vault's edits: an override or split first,
then the first matching rule, then the importer's default (`expenses:unknown`,
`income:unknown`, `income:zap`, `income:lnurl`). The edits are small nostr
notes in a sealed vault; the bulk data never leaves the journal. Source:
`crates/cashflow_cli`.

So categorizing is **writing edits**, never touching segments:

- **Never re-import to fix a category.** Importing only adds segments; the
  category of a row already imported comes from the edits, and a re-import
  changes nothing about it.
- **Never edit files under `journal/`.** Segments are named by their hash and
  a changed one fails verification.

## Running it

```bash
cargo build -p cashflow_cli          # produces target/debug/cashflow
target/debug/cashflow <command>      # or: cargo run -q -p cashflow_cli -- <command>
```

Below, `cashflow` means whichever form you use. `cashflow --help` lists the
commands; `cashflow <command> --help` is each one's page, with its flags and
JSON shape spelled out in prose. Read a page before using a command this skill
doesn't show.

Every vault command reconciles with a relay (default notedeck's embedded one,
`ws://127.0.0.1:6677`) before reading and publishes after writing. With no
relay it works offline against its own cache and a write says `(offline:
stored locally, not published)`; the next run that reaches the relay pushes
it. The `warning: connecting to …` lines on stderr are that, not a failure.

If a vault command fails because no key is stored, ask jb55 to run `cashflow
login`. **Never handle the key yourself**: don't read, print or pass an nsec.

## Privacy: counts leave the session, rows don't

The triage queue shows **real merchant names, amounts, dates and record ids**.
They stay in the session. Never put a payee, amount, account number, record
id or rule pattern in a commit, a card description or comment, an explainer,
a hindsight line or any message outside this conversation. Report outward in
counts only: "categorized 412 of 1,930 unknown rows with 14 rules and 6
overrides".

## The loop

```bash
cashflow transfer suggest --json     # 0. once, first: link transfers (see "Transfers first")
cashflow unknown --json              # 1. the queue, biggest group first
# 2. decide the group's account (see "Choosing the account")
cashflow rule add <account> --match '<pattern>' --source <src> --dry-run   # 3. preview
cashflow rule add <account> --match '<pattern>' --source <src>             # 4. write
cashflow unknown --json | jq .rows   # 5. the count went down by what step 3 said
```

Repeat from 1. Step 0 comes first because both sides of a card payment sit in
the queue until they are linked, and no rule should take them.

### 1. Read the queue

`cashflow unknown --json` (`--source amex`, `--limit 50` to narrow):

```json
{ "rows": 51, "groups_total": 7, "home": "CAD",
  "groups": [ { "payee": "corner grocer", "account": "expenses:unknown",
                "rows": 18, "total": "887.24 CAD", "total_qty": "88724",
                "unconverted": 0, "sources": ["amex"], "pattern": "corner grocer",
                "record_ids": ["amex:…", "…"],
                "rule_add": ["cashflow","rule","add","<account>","--match","corner grocer","--source","amex"] } ] }
```

- `rows` is the whole queue's size: the number the loop drives down.
- A group is one normalized payee (lower-cased, words with digits dropped) on
  one default account, ranked by its total in the home currency. Take them in
  order: one rule on the first group moves the most money.
- `rule_add` is the rule that takes the whole group. Replace `<account>` and
  append `--dry-run`. Its `--source` is the segment's source: the importer's
  name (`amex`, `rbc`, `cln`, …), or `ledger` for rows from a ledger-cli file. It is `null` when the payee has no usable substring:
  categorize those rows one by one, or write a rule on `--meta` instead.

To see a group's rows before deciding: `cashflow report reg expenses:unknown
--payee '<pattern>'` (date, payee, amount). To see one row's raw metadata
(`raw-merchant`, `raw-city`, `raw-description`, `raw-zap-p`, …), export its
day and find its id: `cashflow export --begin <day> --end <next day>`, then
the block holding `; id: <record-id>`.

### 2. Rule or override

- **Rule** (`rule add`) for anything that recurs: a payee, a pattern, a
  metadata value. It also catches the rows future imports bring.
- **Override** (`categorize <record-id> <account>`) for a one-off: a single
  purchase, a payee that means different things on different days.
- **Split** (`split <record-id> a=60 b`) for one charge that is two things.
  Amounts are in the row's commodity, positive in its direction; one bare
  account takes the rest; the legs must add up exactly.

An override or split beats every rule, so an override is also how to pull one
row out of a rule that is otherwise right.

Rules are tried in order and the first match wins. `add` puts the new rule
**last**, which can't take rows another rule already decides; use `--first`,
`--before <rule>` or `--after <rule>` only when you mean to. Conditions:
`--match` (payee substring, case-ignored; repeat for any of), `--meta
key=value` (metadata substring; repeat for all of), `--amount min..max` (the
counter leg in smallest units, cents or msat, sign ignored), `--source`,
`--layer`. Keep the `--source` the queue suggests: a pattern that is safe on
one card's merchant strings can match CLN descriptions too.

Prefer the queue's pattern to a shorter one. "grocer" catches tomorrow's
grocer and also "Grocery Outlet Hardware"; widen a pattern only after reading
the dry run's sample.

### 3. Dry run first, every time

Every write takes `--dry-run`: `rule add|mv|rm|import`, `categorize`,
`uncategorize`, `split`, `transfer link|unlink|suggest --accept-all`. The
rule commands print, and with `--json` return:

```json
{ "dry_run": true, "moved": 18,
  "moves": [ { "from": "expenses:unknown", "to": "expenses:food:groceries", "rows": 18 } ],
  "taken": [ { "rule": "void-candy-frog", "rows": 18 } ],
  "sample": [ { "date": "…", "id": "…", "payee": "…", "from": "…", "to": "…" } ] }
```

**Stop and ask jb55 before writing** when the dry run shows any of:

- `taken` is not empty: a rule change takes rows **from an existing rule**, so
  it overrides a decision someone already made;
- `moved` is **over 200**, or `transfer suggest --accept-all` would link over
  200 pairs: big enough that a wrong choice is expensive to find later;
- `replaced` is not null on `categorize` or `split`: it replaces an override or
  split someone already wrote.

Then ask, with the row count, the total and the account you'd pick, and go on
to the next group meanwhile.

Otherwise check that `moves[].from` is the default you meant to clear and that
the rows belong. `sample` holds only five; for a group bigger than that, read
it whole with `cashflow report reg <default> --payee '<pattern>'` (the rows a
rule would take from elsewhere don't show there, but then `taken` isn't
empty). Then run the same command without `--dry-run`.

A dry run's `added` id is a placeholder: the real `add` names its rule afresh.
Read the id from the write's own output, or from `rule ls`.

### 4. Check the count

`cashflow unknown --json | jq .rows` should have fallen by the write's
`moved`, by one for an override or split, and by two for each transfer
`linked` (both sides leave the queue). If it didn't, `cashflow rule test <record-id> --json` says what
decides one row now (`decided_by`: `override`, `split`, `transfer`, `rule`
or `default`, plus the rule and the pattern that matched).

## Transfers first

Paying the card from chequing is imported twice, once by each source, and
reads as an expense on one side and income on the other until linked. A link
sends both legs to `transfers`, where they net to zero.

```bash
cashflow transfer suggest --json                 # pairs: own account each side, equal and opposite, ≤ 5 days apart
cashflow transfer suggest --accept-all --dry-run
cashflow transfer suggest --accept-all
cashflow transfer link <record-id> <record-id>   # a pair suggest missed
```

Read the suggested pairs before `--accept-all`: two unrelated rows of the
same amount a few days apart look like a transfer too. Link the right ones
one at a time if any pair is wrong. A row with an override or split can't be
linked, and a linked row can't be categorized: `uncategorize` or `transfer
unlink` it first.

## Choosing the account

The account tree is what the books already hold:

```bash
cashflow report bal --depth 3        # every account with a balance
cashflow account ls --all            # the tree, plus any names, icons and projects set on it
cashflow rule ls --json              # where the existing rules send things
```

Put rows under an account that exists, or a new one below a top level, named
in the style of its siblings: `expenses:food:groceries` beside
`expenses:food:restaurants`, or `expenses:transport` when no branch fits.
Prefer an existing branch to a new one, and the shallowest name that says
what it is. **Never invent a top-level account**: the top levels (`assets`,
`liabilities`, `income`, `expenses`, `equity`, `transfers`) carry meaning in
every report. When nothing fits and you would have to guess, leave the group
and list it for jb55 at the end with its row count and total, not its payee.

Don't rename or restructure accounts to fit a rule. `cashflow account alias
<from> <to>` renames a subtree at derive time without touching any rule, but
the chart is jb55's: suggest an alias, don't write one.

Not everything in the queue needs moving. `income:zap` and `income:lnurl` are
already income, and are in the queue only so they can be split per project
with a `--meta` rule (`raw-zap-p`, `raw-lnurl-address`). Leave them unless
jb55 asked for that.

## Provenance and undo

Every write is stamped with `$AGENTIUM_SESSION` when it is an
`agentium:<word-id>` ref. Check it before the first write (`echo
"$AGENTIUM_SESSION"`); outside a session the writes carry no author and can't
be told apart from jb55's own.

To review what a session did:

```bash
cashflow rule ls --by agentium:<word-id> --json  # its rules, each with "matches" (rows decided now)
cashflow edits --by agentium:<word-id> --json    # its overrides, splits and transfer links
```

To undo one: `cashflow rule rm <rule>`, `cashflow uncategorize <record-id>`
(an override or split), `cashflow transfer unlink <a> <b>`. Rules are named by
word-id or any unique prefix of it. Each undo takes `--dry-run` too.

## Finishing

Report the queue's `rows` before and after, the number of rules, overrides,
splits and links written, and the groups left for jb55 to decide (counts and
totals only). `cashflow edits --by "$AGENTIUM_SESSION"` and `cashflow rule ls
--by "$AGENTIUM_SESSION"` are the review list to hand over.
