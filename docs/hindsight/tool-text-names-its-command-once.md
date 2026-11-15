# Tool text names its command once

Tool text spells its command as a literal beside the spec's command path, so a
CLI rename means editing every string, and nothing fails when one is missed.

- headway:notedeck/payment-bounce-consider — 197 backticked `headway …`/`agentium …`/`notebook …` spellings in 50 files, prefixed with `nd` by a regex sweep whose first lookbehind silently skipped every "and \`headway …\`"; a `ToolSpec::command_line()` for the tool's own name, plus a bin_testing check rejecting a retired spelling, would have made it a handful of edits and caught the misses
