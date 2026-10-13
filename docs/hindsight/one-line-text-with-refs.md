# A single-line, clipped label that draws the references in its text

notedeck_ui::markdown draws refs only inside wrapped markdown paragraphs, so a
one-line surface (a list row, a header) walks find_next and lays out its own runs.

- headway:dave/amateur-fuel-stomach — wrote title_with_refs_ui + title_run_ui in dave's session_list.rs (child ui, sideways clip, painted runs so no widget steals the row's hover) to show a headway: title as its chip
- headway:dave/parade-east-fence — the chat header became title_with_refs_ui's second caller: it took a font parameter, went pub(crate) out of session_list.rs, and the header test sits in session_list's tests because that's where the ref-title harness lives
