# notedeck-release

CLI tool that publishes [NIP-94](https://github.com/nostr-protocol/nips/blob/master/94.md) file metadata events (kind 1063) for notedeck release artifacts. These events are consumed by the notedeck auto-updater to discover new versions, verify SHA256 integrity, and download updates.

## Event format

One event per artifact, following the zapstore convention:

```json
{
  "kind": 1063,
  "content": "",
  "tags": [
    ["url", "https://github.com/damus-io/notedeck/releases/download/v1.2.0/notedeck-x86_64-linux.tar.gz"],
    ["x", "<sha256_hex>"],
    ["m", "application/gzip"],
    ["size", "12345678"],
    ["version", "1.2.0"],
    ["name", "notedeck-x86_64-linux.tar.gz"]
  ]
}
```

## Usage

```
notedeck-release --version <semver> --nsec <nsec_or_hex> [options] [files...]
```

### Options

| Flag | Description |
|------|-------------|
| `--version` | Release version (semver, e.g. `1.2.0`) |
| `--nsec` | Secret key (nsec bech32 or 64-char hex) |
| `--relay` | Relay URL to publish to (repeatable) |
| `--dry-run` | Print signed events as JSON without publishing |

### Modes

**GitHub mode** (default): Fetches artifacts from the GitHub Release matching the given version tag.

```sh
cargo run -p notedeck_release -- \
  --version 0.8.0 \
  --nsec <secret_key> \
  --relay wss://relay.damus.io \
  --relay wss://nos.lol
```

**Local mode**: Pass artifact file paths as positional arguments to skip the GitHub fetch.

```sh
cargo run -p notedeck_release -- \
  --version 0.8.0 \
  --nsec <secret_key> \
  --dry-run \
  artifacts/*.tar.gz artifacts/*.dmg
```

### Recognized artifact extensions

`.tar.gz`, `.zip`, `.dmg`, `.deb`, `.rpm`, `.exe`, `.msi`

Other assets in the GitHub Release (e.g. source archives) are skipped.

## Testing

An integration test validates the full pipeline against the latest GitHub Release:

```sh
cargo test -p notedeck_release -- --ignored
```

This runs in dry-run mode and checks that every generated event has valid kind, tags, SHA256, and URLs.
