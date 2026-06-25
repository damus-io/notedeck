# notedeck_horizon

Horizon — a timeblocking nostr calendar app for Notedeck.

Horizon lays your day and week out on a timeline so you can block time
intentionally. Time blocks are modelled as
[NIP-52](https://github.com/nostr-protocol/nips/blob/master/52.md) calendar
events stored in nostrdb:

- `31922` — date-based events (all-day)
- `31923` — time-based events (the core timeblocking primitive)
- `31924` — calendars (collections of events)
- `31925` — RSVPs

## Status

Scaffold. Today it renders an empty day/week timeline grid with a live "now"
indicator and date navigation. Reading and writing NIP-52 blocks is next.

## Running

The app is feature-gated in `notedeck_chrome`:

```sh
cargo run -p notedeck_chrome --features horizon
```

It then appears as a tab in the chrome alongside the other Notedeck apps.
