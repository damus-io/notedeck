# The commit-msg hook reads trailers as git does

The hook greps for `Changelog-` anywhere, so a blank line splitting the trailers
passes it while git sees no `Headway:`; found after landing, it costs an amend.

- headway:notedeck/display-shine-carry — one amend and a re-record of the review: a blank line before `Co-Authored-By:` hid every trailer from `explain note`
- headway:notedeck/sound-base-opinion — ~3 min: the same blank line, found by `explain note` after landing; amend, move `autowork`, rebuild the page, re-record the review and note
