# Notedeck

Notedeck is a shared Rust library that provides the core functionality for building Nostr client applications. It serves as the foundation for various Notedeck applications like notedeck_chrome, notedeck_columns, and notedeck_dave.

## Overview

The Notedeck crate implements common data types, utilities, and logic used across all Notedeck applications. It provides a unified interface for interacting with the Nostr protocol, managing accounts, handling note data, and rendering UI components.

Key features include:

- **Nostr Protocol Integration**: Connect to relays, subscribe to events, publish notes
- **Account Management**: Handle user accounts, keypairs, and profiles
- **Note Handling**: Cache and process notes efficiently
- **UI Components**: Common UI elements and styles
- **Image Caching**: Efficient image and GIF caching system
- **Wallet Integration**: Lightning wallet support with zaps functionality
- **Theme Support**: Customizable themes and styles
- **Storage**: Persistent storage for settings and data

## Applications

This crate serves as the foundation for several Notedeck applications:

- **notedeck_chrome** - The browser chrome, manages a toolbar for switching between different clients
- **notedeck_columns** - A column-based Nostr client interface
- **notedeck_dave** - A nostr ai assistant

## License

GPLv2
