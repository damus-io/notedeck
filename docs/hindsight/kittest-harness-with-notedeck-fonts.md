# One kittest harness builder that installs notedeck's fonts

A snapshot drawing a glyph egui's fonts lack panics on its first pass unless it
installs notedeck's fonts there and skips drawing, a guard each suite copies.

- headway:dave/among-unusual-hazard — ~4 min: two docker snapshot runs panicked on "No glyph for ↑"; setup_fonts after build was too late, so copied widget_tests' cumulative_pass_nr() == 0 guard into the render closure
