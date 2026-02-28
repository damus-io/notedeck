# notedeck_discord: Discord-over-Nostr App Plan

## Overview

A native Discord-like application built as a notedeck app crate, implementing **NIP-29 relay-based groups** with **Flotilla-compatible** relay-level extensions for full interoperability with existing Flotilla spaces. This includes both the core NIP-29 spec (group metadata, membership, moderation) and Flotilla's relay-level management kinds (RELAY_MEMBERS, RELAY_ADD/REMOVE_MEMBER, ROOM_CREATE_PERMISSION).

**Interop target:** Flotilla-compatible — a notedeck_discord user should be able to join and participate in any Flotilla-hosted space.

**Target:** notedeck master branch (current `RelayPool` API), with a prepared migration path for the [Outbox Infrastructure PR #1288](https://github.com/damus-io/notedeck/pull/1288).

---

## Protocol Foundation: NIP-29

NIP-29 defines relay-based groups where the relay itself is the authority. This maps cleanly to Discord's model:

| Discord | NIP-29 | Implementation |
|---------|--------|----------------|
| Server | Space (Relay) | `wss://relay.example.com` — relay with NIP-29 support |
| Channel | Room (Group) | `h` tag group ID, e.g. `relay.example.com'general` |
| Channel roles | Group admin roles | kind `39001` + `39003` — **per-group** (`d` = group-id), not relay-wide |
| Members | Group members | kind `39002` (member list) — per-group scope |
| Messages | Chat events | Any kind with `h` tag (MESSAGE, THREAD, COMMENT) |
| Invite link | Invite code | kind `9009` create-invite with arbitrary `code` tag |
| Kick/Ban | Remove user | kind `9001` remove-user |
| Channel settings | Group metadata | kind `39000` with access flags |

> **Note:** NIP-29 roles/admins are group-scoped (keyed by `d` = group-id), not relay-wide. There is no concept of a "server-wide role" in NIP-29. Relay-level membership is tracked separately via Flotilla extensions (see below).

### NIP-29 Event Kinds

#### Relay-generated metadata (signed by relay master key, `d` tag = group ID)

> **Relay signature validation (REQUIRED):** Kinds 39000-39003 MUST be signed by the relay's own identity key — the `"self"` field in NIP-11, NOT the `"pubkey"` field (which is the admin contact key). On first connect, fetch NIP-11 info and cache the `"self"` value. Reject any 39000-39003 events whose author does not match. Without this check, any client could forge metadata/admin/member snapshots.
>
> **When `"self"` is absent or NIP-11 is unreachable:** The `"self"` field is optional in NIP-11 and the fetch can fail. Policy: degrade to **unverified metadata mode**. Accept 39000-39003 events but tag the space as unverified in local state (`relay_self_key: None`). Display a visual indicator in the UI (e.g. warning badge on the space icon) so the user knows metadata authenticity is not confirmed. All read operations work normally; write operations (join, send messages) are allowed since those are user-signed. Retry NIP-11 fetch periodically — once `"self"` is obtained, retroactively validate cached 39000-39003 events and discard any that fail.

| Kind | Purpose | Key Tags |
|------|---------|----------|
| 39000 | Group metadata | `name`, `picture`, `about`, `private`, `restricted`, `hidden`, `closed` |
| 39001 | Admin list | `p` tags with pubkey + role(s) |
| 39002 | Member list | `p` tags with member pubkeys |
| 39003 | Role definitions | `role` tags with name + description |

#### User-initiated events (`h` tag = group ID)
| Kind | Purpose | Notes |
|------|---------|-------|
| 9021 | Join request | Optional `code` tag for invite |
| 9022 | Leave request | Relay auto-issues kind 9001 in response |

#### Moderation events (admin-only, `h` tag = group ID)
| Kind | Name | Tags |
|------|------|------|
| 9000 | put-user | `p` with pubkey + roles |
| 9001 | remove-user | `p` with pubkey |
| 9002 | edit-metadata | See **edit-metadata tag mapping** below |
| 9005 | delete-event | `e` with event ID |
| 9007 | create-group | — (relay-policy dependent, see **Capability Detection**) |
| 9008 | delete-group | — (relay-policy dependent) |
| 9009 | create-invite | `code` tag (relay-policy dependent) |

#### edit-metadata tag mapping (kind 9002 vs kind 39000)

NIP-29 uses **inverse semantics** between the moderation event (9002) and the snapshot metadata (39000). The 9002 event uses tags that **remove** restrictions, while 39000 uses tags that **assert** restrictions:

| 9002 edit-metadata tag | Effect | Corresponding 39000 tag |
|------------------------|--------|------------------------|
| `unrestricted` | Allow anyone to write | Removes `restricted` |
| `open` | Accept join requests | Removes `closed` |
| `visible` | Show metadata to anyone | Removes `hidden` |
| `public` | Allow anyone to read | Removes `private` |
| *(omit tag)* | Keep restriction active | `restricted` / `closed` / `hidden` / `private` present |

Implementation must map between these two representations:
- **Reading** kind 39000: presence of `restricted`/`private`/`hidden`/`closed` = flag is ON
- **Writing** kind 9002: send `unrestricted`/`public`/`visible`/`open` to REMOVE a flag; omit to keep it

#### Access control flags (in kind 39000 snapshot)
| Flag | Meaning | Discord Equivalent |
|------|---------|-------------------|
| `restricted` | Only members can write | Members-only channel |
| `private` | Only members can read | Private channel |
| `hidden` | Metadata hidden from non-members | Hidden channel |
| `closed` | Join requests rejected (invite-only) | Invite-only server |

#### Flotilla relay-level extensions

Flotilla extends NIP-29 with relay-scoped membership and permission kinds. These are required for full interoperability with Flotilla spaces:

| Kind | Name | Scope | Purpose |
|------|------|-------|---------|
| RELAY_MEMBERS | Relay member list | Relay-wide | List of all relay/space members |
| RELAY_ADD_MEMBER | Add relay member | Relay-wide | Admin adds user to space |
| RELAY_REMOVE_MEMBER | Remove relay member | Relay-wide | Admin removes user from space |
| RELAY_JOIN | Join space | Relay-wide | User requests to join relay/space |
| RELAY_LEAVE | Leave space | Relay-wide | User leaves relay/space |
| ROOM_CREATE_PERMISSION | Room creation perm | Relay-wide | Tracks which pubkeys can create rooms |
| ROOM_ADD_MEMBER | Add room member | Per-room | Admin adds user to specific room |
| ROOM_REMOVE_MEMBER | Remove room member | Per-room | Admin removes user from specific room |
| ROOM_JOIN | Join room | Per-room | User-initiated room join |
| ROOM_LEAVE | Leave room | Per-room | User-initiated room leave |
| ROOM_DELETE | Delete room | Per-room | Room deletion event |

These kinds are imported from `@welshman/util` in Flotilla and synced in `flotilla/src/app/core/sync.ts`. Our implementation must subscribe to and handle all of these for Flotilla-space compatibility.

### Supporting NIPs
- **NIP-51** (kind `10009`): User's saved group list — persistence across sessions
- **NIP-70**: `-` tag for unmanaged relay leak prevention
- **NIP-C7**: Fallback chat for relays without NIP-29 support (stretch goal)
- **NIP-56**: Reports/moderation

### Timeline References
Per NIP-29, messages SHOULD include `previous` tags referencing at least 3 of the last 50 events seen from the relay. This prevents out-of-context rebroadcasting to forked groups.

---

## Architecture

### Crate Structure

```
crates/notedeck_discord/
├── Cargo.toml
├── src/
│   ├── lib.rs              # DiscordApp: App trait impl, top-level state
│   ├── nip29.rs            # NIP-29 event kind constants, parsing, construction
│   ├── space.rs            # Space (relay) model, NIP-29 capability detection
│   ├── room.rs             # Room (group) model, metadata, access flags
│   ├── member.rs           # Membership tracking, join/leave state machine
│   ├── roles.rs            # Role definitions, permission checks
│   ├── timeline.rs         # Per-room message timeline, pagination
│   ├── subscriptions.rs    # NIP-29 subscription management, FilterState wiring
│   ├── relay_compat.rs     # Relay interaction abstraction (migration shim for outbox)
│   └── ui/
│       ├── mod.rs
│       ├── space_sidebar.rs   # Left icon bar (space/relay selector)
│       ├── room_list.rs       # Channel list within a space
│       ├── chat_view.rs       # Message timeline + compose area
│       ├── member_panel.rs    # Member list sidebar
│       ├── room_form.rs       # Create/edit room (admin)
│       ├── space_settings.rs  # Space admin panel
│       └── moderation.rs      # Admin action menus
```

### How It Fits into Notedeck

```
Notedeck (host)
├── ndb (nostrdb)           ← event storage, queried by kind + h-tag
├── RelayPool               ← connections to NIP-29 relays
├── Accounts                ← keypairs, signing, relay lists
├── Images                  ← avatar/icon caching
└── notedeck_discord (app)
    ├── impl App for DiscordApp
    │   └── fn update(&mut self, ctx: &mut AppContext, ui: &mut egui::Ui) -> AppResponse
    ├── Subscribes to NIP-29 kinds via ctx.pool
    ├── Queries nostrdb for cached events
    └── Renders Discord-like UI via egui
```

### Relay Interaction Abstraction

To prepare for the outbox migration, all relay operations go through `relay_compat.rs`:

```rust
// relay_compat.rs — thin abstraction over current RelayPool
// When PR #1288 merges, only this file changes

pub fn subscribe_to_relay(pool: &mut RelayPool, url: &str, filters: Vec<Filter>) -> String;
pub fn send_to_relay(pool: &mut RelayPool, url: &str, event: ClientMessage);
pub fn broadcast_event(pool: &mut RelayPool, event: ClientMessage);
```

**Migration when outbox lands (adapter-based, APIs to be confirmed against merged code):**

The outbox PR (#1288) is in flight and its exact public API may change before merge. The relay_compat.rs abstraction isolates us from these changes. Expected migration points:
1. `RelayPool` (enostr) -> new pool wrapper (notedeck) — exact type TBD
2. `pool.send()` -> broadcast API — exact method TBD
3. `String` sub IDs -> outbox subscription ID type — exact type TBD
4. `FilterStates` -> simplified filter state — confirm variant changes
5. `try_process_events_core()` -> updated event processing entry point — confirm name

Do not code against guessed outbox symbols. Migrate only after PR merges and APIs stabilize.

---

## Data Models

### Space (Relay as Server)

```rust
pub struct Space {
    pub url: String,                    // wss://relay.example.com
    pub relay_self_key: Option<[u8; 32]>, // from NIP-11 "self" field, used to validate 39000-39003 signatures
    pub has_nip29: bool,                // detected via NIP-11 or metadata events
    pub rooms: Vec<RoomId>,             // known groups on this relay
    pub connection: RelayConnectionStatus,
    pub membership: SpaceMembershipStatus,
}

pub enum RelayConnectionStatus {
    Connected,
    Disconnected,
}

/// Space-level membership mirrors Flotilla's relay join/leave model.
/// Derived from RELAY_ADD_MEMBER / RELAY_REMOVE_MEMBER / RELAY_MEMBERS events,
/// plus transient state from RELAY_JOIN request responses.
pub enum SpaceMembershipStatus {
    None,           // no join attempted, not in RELAY_MEMBERS
    Pending,        // RELAY_JOIN sent, awaiting RELAY_ADD_MEMBER confirmation
    Joined,         // confirmed via RELAY_ADD_MEMBER or present in RELAY_MEMBERS
    Left,           // RELAY_LEAVE sent or RELAY_REMOVE_MEMBER received
}
```

### Room (Group as Channel)

```rust
pub struct Room {
    pub id: RoomId,                     // "relay.example.com'group-id"
    pub url: String,                    // relay URL
    pub h: String,                      // group ID (h-tag value)
    pub meta: RoomMeta,                 // from kind 39000
    pub membership: RoomMembershipStatus,
}

pub struct RoomMeta {
    pub name: Option<String>,
    pub picture: Option<String>,
    pub about: Option<String>,
    pub is_restricted: bool,            // members-only write
    pub is_private: bool,               // members-only read
    pub is_hidden: bool,                // metadata hidden from non-members
    pub is_closed: bool,                // reject join requests
}

pub enum RoomMembershipStatus {
    Initial,        // not a member
    Pending,        // join request sent (kind 9021), awaiting approval
    Granted,        // confirmed member (kind 9000 received)
}

// Rejection is ephemeral — derived from relay OK/NOTICE responses,
// not from the event stream. Not persisted across restarts.
pub struct JoinAttemptResult {
    pub status: RoomMembershipStatus,
    pub rejection_reason: Option<String>,  // transient, from relay NOTICE
}
```

### Membership Tracking

Membership is derived from the event stream with the following precedence rules:

**Space-level (relay) membership:**
1. Check RELAY_MEMBERS event — if user's pubkey is listed, they are joined
2. Check RELAY_ADD_MEMBER / RELAY_REMOVE_MEMBER events — latest by timestamp wins
3. If neither exists — SpaceMembershipStatus::None

**Room-level membership (NIP-29 core + Flotilla extensions merged):**
1. Check NIP-29 core: latest of kind 9000 (put-user) vs kind 9001 (remove-user) for user's pubkey — latest by timestamp wins
2. Check Flotilla extensions: latest of ROOM_ADD_MEMBER vs ROOM_REMOVE_MEMBER for user's pubkey — latest by timestamp wins
3. **Merge rule**: take the latest event across both sources. If it's an add/put-user → Granted. If it's a remove → Initial.
4. If neither exists — not a member (unless unmanaged group, where everyone is a member)
5. **NIP-51 kind 10009** — user's personal group list for cross-session persistence (does not override event-derived state, used for bootstrapping subscriptions on startup)

> **Note on rejection:** NIP-29 join rejection is a relay OK/NOTICE error response (nips/29.md:73), not a durable event. After restart, rejection state cannot be reconstructed from the event stream. `rejection_reason` is ephemeral local state only — shown to the user once, then cleared.

### Capability Detection

NIP-29 moderation events (9000-9009) are explicitly optional per spec. Group creation (9007) has no canonical mechanism — relays create groups when asked by users, but relay policy varies. Before showing admin UI:

1. **Detect NIP-29 support**: Check NIP-11 relay info for `supported_nips` containing 29, or presence of kind 39000 events
2. **Gate create/invite UI**: Enable "Create Room" (9007) and "Generate Invite" (9009) when NIP-29 support is detected. Use a tiered check (mirrors Flotilla's `deriveUserCanCreateRoom` logic):
   - If ROOM_CREATE_PERMISSION events exist for this relay (Flotilla extension): allow if user's pubkey is in the permission set **OR** if user is a space admin (admin always bypasses permission check)
   - If no ROOM_CREATE_PERMISSION events exist (non-Flotilla NIP-29 relay): allow create attempt if relay supports NIP-29 — the relay will reject unauthorized attempts via OK/NOTICE
   - If NIP-29 support is undetected: show disabled controls with tooltip ("This relay may not support group creation")
3. **Handle rejection gracefully**: If 9007 is rejected by the relay, surface the error message and disable the button for that session

---

## UI Design

### Layout (Desktop)

```
┌──────┬──────────────┬────────────────────────────────┬──────────────┐
│Space │ Room List     │ Chat Area                      │ Members      │
│Icons │              │                                │              │
│      │ # general    │ ┌──────────────────────────┐   │ Admin        │
│ [S1] │ # random     │ │ alice: hey everyone       │   │  @alice      │
│ [S2] │ # dev        │ │ bob: gm                   │   │              │
│ [S3] │              │ │ carol: building something │   │ Members      │
│      │ ── Admin ──  │ │ ...                       │   │  @bob        │
│ [+]  │ # announce   │ └──────────────────────────┘   │  @carol      │
│      │              │ ┌──────────────────────────┐   │  @dave       │
│      │              │ │ [message input]           │   │              │
│      │              │ └──────────────────────────┘   │              │
└──────┴──────────────┴────────────────────────────────┴──────────────┘
```

### Layout (Mobile)

Toggle between: Space list -> Room list -> Chat view -> Member panel

Following Dave app's responsive pattern with narrow-width detection.

### User Flows

1. **Browse & Connect**: Add relay URL -> detect NIP-29 -> show as space
2. **Browse Rooms**: See room list with name/about/member count from kind 39000
3. **Join Room**: Send kind 9021 -> track pending -> receive kind 9000 confirmation
4. **Read Messages**: Subscribe to message kinds with `#h` filter -> render timeline
5. **Send Message**: Compose -> sign -> inject `h` tag + `previous` tags -> send to relay
6. **Leave Room**: Send kind 9022 -> relay auto-removes
7. **Reactions/Threads**: Standard nostr reactions with `h` tag, reply chains via `e` tags

### Admin/Creator Flows

1. **Create Room**: (gated behind capability detection) kind 9007 -> set metadata via kind 9002 using **inverse tags** (`unrestricted`/`open`/`visible`/`public` to remove flags)
2. **Edit Room**: kind 9002 with inverse tag semantics (see edit-metadata tag mapping above)
3. **Manage Members**: kind 9000 (add with roles) / kind 9001 (remove)
4. **Delete Messages**: kind 9005 with `e` tag
5. **Generate Invites**: kind 9009 with `code` tag (for closed groups)
6. **Delete Room**: kind 9008
7. **Assign Roles**: kind 9000 with `p` tag + role labels

---

## Implementation Phases

### Phase 1: Foundation
| Task | Depends On | Description |
|------|------------|-------------|
| Scaffold crate | — | See **Scaffold Integration Checklist** below |
| NIP-29 data models | Scaffold | Rust types for all event kinds, Room/Space/Member structs, parsing |

### Phase 2: Core Protocol
| Task | Depends On | Description |
|------|------------|-------------|
| Subscription & sync | Data models | Subscribe to NIP-29 relays, sync metadata/members/admins, validate relay signatures |
| Message timeline | Subscriptions | Per-room message feed with h-tag filtering, pagination |
| Membership mgmt | Data models, Subscriptions | Join/leave flows, status tracking, NIP-51 group list. Depends on subscriptions because membership state is derived from moderation events (9000/9001) received via relay sync. |
| Room creation | Membership | Create/edit/delete rooms, all access flags, capability-gated |
| Message compose | Timeline | Text input, h-tag injection, previous-tag refs, signing |

### Phase 3: UI
| Task | Depends On | Description |
|------|------------|-------------|
| Core UI layout | Timeline, Membership | Space sidebar, room list, chat area, responsive |
| User flows | UI layout | Browse, join, read/send, unread indicators |
| Admin flows | UI layout, Room creation | Room creation form, member management, moderation UI |

### Phase 4: Polish
| Task | Depends On | Description |
|------|------------|-------------|
| Roles & permissions | Membership | Parse role defs, permission checks, admin detection |
| Moderation tools | Roles | Delete messages, remove users, reports |
| NIP-C7 fallback | — | Evaluate vanilla chat for non-NIP-29 relays |

### Ongoing
| Task | Description |
|------|-------------|
| Outbox migration | Track PR #1288, migrate when merged via relay_compat.rs |

---

## Dependency Graph

```
Scaffold
└── NIP-29 Data Models
    └── Subscriptions & Sync
        ├── Timeline
        │   ├── UI Layout ←── also depends on Membership
        │   │   ├── User Flows
        │   │   └── Admin Flows ←── also depends on Room Creation
        │   └── Message Compose
        └── Membership
            ├── Room Creation
            └── Roles & Permissions
                └── Moderation Tools
```

---

## Scaffold Integration Checklist

Registering a new app in notedeck requires changes across multiple crates:

1. **Create crate**: `crates/notedeck_discord/Cargo.toml` + `src/lib.rs` with `App` trait impl
2. **Workspace**: Add `notedeck_discord` to root `Cargo.toml` workspace `members` list + `[workspace.dependencies]` section
3. **Chrome Cargo.toml**: Add `notedeck_discord = { workspace = true, optional = true }` to `crates/notedeck_chrome/Cargo.toml` dependencies
4. **Chrome feature flag**: Add `discord = ["notedeck_discord"]` to `crates/notedeck_chrome/Cargo.toml` `[features]`
5. **NotedeckApp enum**: Add `#[cfg(feature = "discord")] Discord(Box<DiscordApp>)` variant to `crates/notedeck_chrome/src/app.rs:19`
6. **App trait forwarding**: Add match arm in `NotedeckApp`'s `impl App` (same file)
7. **Chrome boot**: Add `#[cfg(feature = "discord")] chrome.add_app(NotedeckApp::Discord(...))` in `crates/notedeck_chrome/src/chrome.rs:161`
8. **Sidebar label**: Add match arm for sidebar button text in `crates/notedeck_chrome/src/chrome.rs:789`
9. **Sidebar icon**: Add match arm for sidebar icon rendering in `crates/notedeck_chrome/src/chrome.rs:822`

---

## Coding Standards (from AGENTS.md)

- Implement `App` trait using `AppContext` for ndb/pool/accounts access
- Register in `NotedeckApp` enum with icon/label metadata + sidebar integration
- Reuse `notedeck_ui` components for visual consistency via `NoteContext`
- Use [`shadcn-egui`](https://github.com/alltheseas/shadcn-egui) components for Discord-like UI polish — buttons (6 variants), badges, avatars, cards, modals, context menus, command palette, toasts, tabs, collapsible sidebars, resizable panels, data tables, dropdowns, and toggle groups. Apply theming via `NotedeckTheme::apply(ctx, dark_mode)`
- Subscribe to NIP-29 kinds through `RelayPool` mirroring existing patterns
- Store lightweight view state internally; persist only user preferences
- Respect `ctx.style()` for colors/fonts, support narrow layouts
- No global variables, no blocking the render loop
- `Rc<RefCell<T>>` for single-threaded interior mutability
- `Promise::ready()` for non-blocking async checks
- Wrap user-facing strings with `tr!` macros
- Run `cargo fmt`, `cargo clippy`, `cargo test` before submission

---

## Reference Implementations

- **Flotilla PWA** (`flotilla/`): Full NIP-29 Discord clone in Svelte/TS — canonical reference for protocol behavior, event kinds, membership flows, and UI patterns
- **notedeck_dave** (`notedeck/crates/notedeck_dave/`): Template for notedeck app structure — App trait impl, session management, responsive UI, async streaming
- **notedeck_columns** (`notedeck/crates/notedeck_columns/`): Timeline/subscription patterns, filter state management, multi-relay fan-out
- **NIP-29 spec** (`nips/29.md`): Protocol specification for relay-based groups
- **shadcn-egui** ([github.com/alltheseas/shadcn-egui](https://github.com/alltheseas/shadcn-egui)): shadcn/ui design system ported to egui — provides Discord-appropriate components: modal dialogs (room settings), context menus (message actions), command palette (quick navigation), toasts (notifications), collapsible sidebar (room list), resizable panels (chat/member split), avatars, badges (roles/unread), tabs, dropdowns, and cards. Themed with Notedeck purple (#CC43C5)
