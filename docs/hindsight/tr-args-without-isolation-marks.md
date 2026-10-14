# tr! arguments stay plain text in strings that are matched, not only drawn

Fluent wraps every `{arg}` in U+2068/U+2069 once the key is in the FTL, but the
fallback before export doesn't, so a test of a `tr!` string passes, then fails.

- headway:notedeck/jump-bike-wife — ~4 min: "Open #{tag} timeline" matched the Cmd-K query until the export added its key, then the bar's title match broke on the isolation marks; retitled the row `#tag` with a plain subtitle
