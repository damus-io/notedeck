# Notedeck Chrome

Notedeck Chrome is the UI framework and container for the Notedeck Nostr browser. It manages multiple applications within a single cohesive interface, providing a consistent navigation experience through a persistent sidebar.

## Overview

Notedeck Chrome acts as the container for various applications within the Notedeck ecosystem, primarily:

- **Columns** - The main Nostr columns interface for viewing timelines and interactions
- **Dave** - An ai assistant
- **Other** - Anything else *tbd*

The Chrome component provides:

- A consistent, unified sidebar for navigation between applications
- Theme management (light/dark mode support)
- Profile picture and account management
- Settings access
- Wallet integration

## Features

- **Application Switching**: Switch between Damus columns view and Dave seamlessly
- **Theme Support**: Toggle between light and dark modes
- **Profile Management**: Quick access to account settings
- **Responsive Design**: Compatible with desktop and mobile interfaces
- **Android Support**: Native support for Android devices

Future:

- **Signer**: Apps will be sandboxed from the users key

## Development Status

Notedeck is currently in **ALPHA**. Expect bugs and please report any issues you encounter.

## Building from Source

For build instructions, see the [DEVELOPER.md](DEVELOPER.md) file.

## License

Licensed under GPLv3 - see the [Cargo.toml](Cargo.toml) file for details.

## Authors

- William Casarin <jb55@jb55.com>
- kernelkind <kernelkind@gmail.com>
