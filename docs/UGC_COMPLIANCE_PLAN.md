# Google Play Store UGC Compliance Plan for notedeck_columns

## Overview

Implement User Generated Content (UGC) compliance features required by Google Play Store for the notedeck_columns Nostr client app.

## Current State

- **Has**: Client-side muting (pubkeys, hashtags, threads), blurhash media obfuscation for non-followed users
- **Missing**: TOS acceptance, user blocking, content reporting, age verification

## Requirements from Google Play

1. TOS acceptance before UGC creation
2. Define objectionable content in TOS
3. In-app reporting system for users and content
4. User blocking functionality (required for DM/mention features)

---

## Implementation Plan

### Phase 1: Data Structures & Storage

**1.1 Create compliance module** - `crates/notedeck/src/compliance.rs`
```rust
pub struct ComplianceData {
    pub tos_accepted: bool,
    pub tos_accepted_at: Option<u64>,
    pub tos_version: String,  // e.g., "1.0"
    pub age_verified: bool,
}
```

**1.2 Add blocked users to Muted** - `crates/notedeck/src/muted.rs`
- Add `blocked_pubkeys: BTreeSet<[u8; 32]>` field
- Add `is_blocked(pk)` method
- Blocked = completely hidden (stronger than muted)

**1.3 Extend Settings** - `crates/notedeck/src/persist/settings_handler.rs`
- Add `tos_accepted`, `tos_accepted_at`, `tos_version`, `age_verified` fields
- Add `blocked_pubkeys` (or store per-account)

### Phase 2: TOS & Age Verification Screen

**2.1 Create TOS route** - `crates/notedeck_columns/src/route.rs`
- Add `Route::TosAcceptance`

**2.2 Create TOS UI** - `crates/notedeck_columns/src/ui/side_panel/tos.rs` (new)
- Full-screen modal with:
  - Scrollable TOS text (embedded, you provide content)
  - Checkbox: "I confirm I am 17 years or older"
  - Checkbox: "I agree to the Terms of Service and Community Guidelines"
  - "Accept and Continue" button (disabled until both checked)

**2.3 Gate UGC creation**
- Modify `crates/notedeck_columns/src/ui/note/post.rs` - check TOS before post
- Modify note reply flow - check TOS before reply
- Modify `crates/notedeck_messages/` - check TOS before DM

**2.4 Trigger on first launch**
- In app startup, if `!settings.tos_accepted`, show TOS screen before main UI

### Phase 3: Block User Feature

**3.1 Add block to context menus**
- `crates/notedeck_ui/src/note/context.rs` - add `BlockAuthor` option
- `crates/notedeck_ui/src/profile/context.rs` - add `Block` option

**3.2 Block confirmation dialog** - `crates/notedeck_columns/src/ui/side_panel/block.rs` (new)
- "Block @username?"
- "You won't see their posts, replies, or messages"
- Block / Cancel buttons

**3.3 Filter blocked content**
- Modify timeline rendering to skip blocked pubkeys
- Modify DM conversation list to hide blocked users
- Modify notification filtering

**3.4 Blocked users management**
- Add to Settings UI: "Blocked Users" section with list and unblock option

### Phase 4: Report Feature (NIP-56)

**4.1 Create report event builder** - `crates/enostr/src/report.rs` (new)
```rust
// NIP-56 Report Event (kind 1984)
pub fn create_report_note(note_id, author_pk, reason) -> NostrEvent
pub fn create_report_profile(pubkey, reason) -> NostrEvent
```

Reasons (NIP-56): `nudity`, `malware`, `profanity`, `illegal`, `spam`, `impersonation`, `other`

**4.2 Add report to context menus**
- Note context menu: "Report Note"
- Profile context menu: "Report User"

**4.3 Report dialog UI** - `crates/notedeck_columns/src/ui/side_panel/report.rs` (new)
- Select reason (dropdown/radio)
- Optional description text
- Submit / Cancel buttons
- On submit: sign and publish NIP-56 event to relays

### Phase 5: Settings Integration

**5.1 Add "Content & Safety" section** to `crates/notedeck_columns/src/ui/settings.rs`
- Blocked Users (list with unblock)
- Muted Users (existing, but surface here too)
- View Terms of Service
- Content filtering toggle (hide sensitive media by default)

---

## Key Files to Modify

| File | Changes |
|------|---------|
| `crates/notedeck/src/muted.rs` | Add `blocked_pubkeys`, `is_blocked()` |
| `crates/notedeck/src/persist/settings_handler.rs` | Add TOS/compliance fields |
| `crates/notedeck_columns/src/route.rs` | Add TOS, Report, BlockConfirm routes |
| `crates/notedeck_ui/src/note/context.rs` | Add Block/Report menu options |
| `crates/notedeck_ui/src/profile/context.rs` | Add Block/Report menu options |
| `crates/notedeck_columns/src/ui/settings.rs` | Add Content & Safety section |
| `crates/notedeck_columns/src/ui/note/post.rs` | Gate posting behind TOS |

## New Files to Create

| File | Purpose |
|------|---------|
| `crates/notedeck/src/compliance.rs` | ComplianceData struct |
| `crates/notedeck_columns/src/ui/side_panel/tos.rs` | TOS acceptance screen |
| `crates/notedeck_columns/src/ui/side_panel/block.rs` | Block confirmation dialog |
| `crates/notedeck_columns/src/ui/side_panel/report.rs` | Report dialog |
| `crates/enostr/src/report.rs` | NIP-56 report event creation |

---

## TOS Content Requirements

The embedded TOS must define prohibited content (per Google Play):
- Illegal content
- Child sexual abuse material
- Harassment and bullying
- Hate speech and discrimination
- Impersonation
- Malware, phishing, spam
- Sexually explicit content (note: Nostr is decentralized, explain user's responsibility)

Include:
- Age requirement (17+)
- How to report content
- How to block users
- Disclaimer: decentralized protocol means content cannot be deleted from all relays

**You will need to provide the actual TOS text.**

---

## Verification

After implementation:
1. Fresh install → TOS screen appears before main UI
2. Cannot post/reply/DM until TOS accepted
3. Block user from note → user's content disappears from timelines
4. Block user from profile → same result
5. Report note → NIP-56 event published to relays (check with relay or other client)
6. Report profile → NIP-56 event published
7. Blocked users visible in Settings, can unblock
8. App restart → blocked users remain blocked, TOS remains accepted

---

## Out of Scope (Nostr Protocol Limitations)

- Cannot delete content from relays (decentralized)
- Cannot prevent blocked users from seeing your content
- Cannot implement server-side moderation
- Reports are informational (relays/clients may or may not act on them)

These limitations should be disclosed in the TOS.
