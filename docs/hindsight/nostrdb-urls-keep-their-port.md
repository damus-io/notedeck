# nostrdb content URLs keep their port

nostrdb's content parser cuts a URL at its `:port`, leaving a bare-host link.
A Blossom or test server on a non-default port can't be linked as media.

- headway:ux-feedback/torch-legend-session — ~3 min: the attachment snapshot's comment showed its `https://127.0.0.1:9/…` screenshot URL as a broken link instead of media; moved the test server to `https://blossom.invalid` rather than fix the tokenizer
