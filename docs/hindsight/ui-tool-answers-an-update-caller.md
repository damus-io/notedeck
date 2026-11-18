# A UI tool's answer reaches a caller outside the socket

Only the socket hears a UI tool's answer; `route_call_tool` drops a `CallTool`'s
`Data`, so an agent hears "sent", never "opened" or "no such note".

- headway:dave/fix-armed-only — the native loop's UI-tool calls answer the model `{"sent_to_window": true}` because `AppActionQueue` carries no reply; the next card's nostr navigation tools inherit a model that can't tell a bad note id from a good one
