# Allocation ratchets count accesskit apart

Kittest runs accesskit, which the app does only under a screen reader, so a
ratchet counts an accessible row's label as if it were text formatted per frame.

- headway:cashflow/snake-chunk-february — ~6 min: making register rows clickable `Role::Button`s with their payee moved `transactions_alloc` 102 → 154 (two accesskit allocations per row on screen), against a card spec of "must stay at 102±4"; attributed it, confirmed `register_widget_info` is free in release, and re-recorded with the reason, where a count without accesskit's sites would not have moved
