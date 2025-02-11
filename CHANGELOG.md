# Unreleased

## Added

- Added the ability to tag users in posts (kernelkind)
- Added ctrl-enter shortcut to send post (Ethan Tuttle)

## Changed

- Updated NIP-05 verification text to "Nostr address" (Derek Ross)

## Fixed

- Added *experimental* last post per pubkey algo feeds (William Casarin)
- Fixed stale feeds (William Casarin)

# Notedeck Alpha 2 - v0.3 - 2025-01-31

## Added
- Clicking a mention now opens profile page (William Casarin)
- Note previews when hovering reply descriptions (William Casarin)
- Media uploads (kernelkind)
- Profile editing (kernelkind)
- Add hashtags to posts (Daniel Saxton)
- Enhanced command-line interface for user interactions (Ken Sedgwick)
- Various Android updates and compatibility improvements (Ken Sedgwick, William Casarin)
- Debug features for user relay-list and mute list synchronization (Ken Sedgwick)

## Changed
- Add confirmation when deleting columns (kernelkind)
- Enhance Android build and performance (Ken Sedgwick)
- Image cache handling using sha256 hash (kieran)
- Introduction of decks_cache and improvements (kernelkind)
- Migrated to egui v0.29.1 (William Casarin)
- Only show column delete button when not navigating (William Casarin)
- Show profile pictures in column headers (William Casarin)
- Show usernames in user columns (William Casarin)
- Switch to only notes & replies on some tabs (William Casarin)
- Tombstone muted notes (Ken)
- Pointer interactions enhancements in UI (William Casarin)
- Persistent theme setup across sessions (kernelkind)
- Increased ping intervals for network performance (William Casarin)
- Nostrdb update for async support (Ken Sedgwick)

## Fixed
- Fix GIT_COMMIT_HASH compilation issue (William Casarin)
- Fix avatar alignment in profile previews (William Casarin)
- Fix broken quote repost hitbox (William Casarin)
- Fix crash when navigating in debug mode (William Casarin)
- Fix long delays when reconnecting (William Casarin)
- Fix repost button size (William Casarin)
- Fixed since kind filters (kernelkind)
- Clippy warnings resolved (Dimitris Apostolou)

## Refactoring & Improvements
- Numerous internal structural improvements and modularization (William Casarin, Ken Sedgwick)
