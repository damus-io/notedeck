# `BoardAction::AddCard` carries every field a new card can start with

`AddCard` has no priority, estimate or due date, so a card born with one needs
a second edit, made once the first has handed back the new card's id.

- headway:headway/office-flower-churn — the list's `+` under a priority group couldn't say the priority in the add: it needed `ListState::pending_priority`, `BoardUiState::card_added`, and `lib.rs` switching `store::apply` to `apply_outcome` to route the created id into a `SetPriority` follow-up edit; adding the field meant touching ~30 `AddCard` literals across store tests, snapshot tests and the CLI
