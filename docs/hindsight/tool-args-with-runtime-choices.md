# A tool argument whose choices come from app state

`ToolArgType::Enum` holds `&'static str`s, so an argument valued in app data
(a saved view, a board) is a free string the action bar's picker can't offer.

- headway:notedeck/mercy-unique-foil — `headway_list_tab` couldn't advertise the account's saved views as `to` values, so it split into a static `to` enum and a free-string `view` with a both-is-an-error rule; the Cmd-K arg step will offer only the built-ins
