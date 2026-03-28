# notedeck-release

CLI tool that publishes [NIP-82](https://github.com/nostr-protocol/nips/blob/master/82.md) software distribution events for notedeck release artifacts. These events are consumed by the notedeck auto-updater to discover new versions, verify SHA256 integrity, and download updates.

## Event format

Follows the zapstore convention with two event kinds:

**Kind 3063 — Software Asset** (one per artifact):
```json
{
  "kind": 3063,
  "content": "",
  "tags": [
    ["i", "com.damus.notedeck"],
    ["url", "https://github.com/damus-io/notedeck/releases/download/v1.2.0/notedeck-x86_64-linux.tar.gz"],
    ["x", "<sha256_hex>"],
    ["version", "1.2.0"],
    ["f", "linux-x86_64"],
    ["name", "notedeck-x86_64-linux.tar.gz"],
    ["m", "application/gzip"],
    ["size", "12345678"]
  ]
}
```

**Kind 30063 — Software Release** (one per version, references all assets):
```json
{
  "kind": 30063,
  "content": "",
  "tags": [
    ["d", "com.damus.notedeck@1.2.0"],
    ["i", "com.damus.notedeck"],
    ["version", "1.2.0"],
    ["c", "main"],
    ["e", "<asset_event_id_1>"],
    ["e", "<asset_event_id_2>"]
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
| `--channel` | Release channel: `main` (default), `beta`, `nightly`, `dev` |
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

**Beta channel**:

```sh
cargo run -p notedeck_release -- \
  --version 0.8.1-beta.1 \
  --nsec <secret_key> \
  --channel beta \
  --relay wss://relay.damus.io
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

This runs in dry-run mode and checks that every generated event has valid NIP-82 kinds, tags, SHA256, and URLs.
