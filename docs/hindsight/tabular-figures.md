# Tabular figures in notedeck's face

Onest's digits are proportional and egui can't switch on `tnum`, so number columns
don't line up, and padding digits to a cell opens gaps inside numbers instead.

- headway:cashflow/crush-snake-top — ~8 min: built a per-digit `LayoutSection::leading_space` tabular layout for HUD tooltip values, rendered it, and threw it away when `3,091,036` read as `3,09 1,036`
