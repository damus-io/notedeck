# Notedeck Calendar - NIP-52 Calendar Events

A calendar application for Notedeck that implements NIP-52 (Calendar Events) for the Nostr protocol.

## Features

- **View Calendar Events**: Display date-based and time-based calendar events
- **Create Events**: Create new calendar events with full NIP-52 support
- **RSVP System**: Accept, decline, or tentatively respond to events
- **Calendar Management**: Create and manage multiple calendars
- **Social Features**: Comment, repost, and react to calendar events
- **Collaborative Calendars**: Request to add events to other users' calendars

## Event Types (NIP-52)

### Date-Based Calendar Events (kind 31922)
- All-day or multi-day events
- ISO 8601 date format (YYYY-MM-DD)
- Use cases: holidays, vacations, anniversaries

### Time-Based Calendar Events (kind 31923)
- Events with specific times
- Unix timestamp format
- Timezone support (IANA format)
- Use cases: meetings, appointments, scheduled events

### Calendars (kind 31924)
- Collection of calendar events
- Addressable by kind+pubkey+d-tag
- Support for event requests

### RSVPs (kind 31925)
- Responses to calendar events
- Status: accepted, declined, tentative
- Free/busy indication

## Architecture

Following the Notedeck app pattern (similar to Dave):

```
notedeck_calendar
├── UI Layer (ui/mod.rs, ui/calendar.rs)
├── Event Management (events.rs)
├── Calendar System (calendars.rs)
├── RSVP System (rsvp.rs)
└── Core Logic (lib.rs)
```

## Usage

The calendar app is integrated into Notedeck and can be accessed from the main application interface.

## License

GPL
