#!/usr/bin/env bash
#
# Publish NIP-89 application-handler listings (kind:31990) for Damus and Notedeck.
#
# SAFETY / HOW TO USE:
#   1. Review the whole script first — especially every value marked  # VERIFY.
#   2. Dry run (default): builds + signs locally and PRINTS the events. Nothing is sent.
#          ./publish-nip89.sh
#   3. Publish for real: appends the relay list so nak sends the events.
#          PUBLISH=1 ./publish-nip89.sh
#   You'll be prompted (twice, once per app) to paste your nsec. It is read by nak
#   directly from the terminal — it never appears on the command line or on disk.
#   Prefer a remote signer? Replace `--prompt-sec` with `--connect bunker://...`.
#
set -euo pipefail

# --- relays to publish to (only used when PUBLISH=1) ------------------------
RELAYS=(
  wss://relay.damus.io
  wss://nos.lol
  wss://relay.primal.net
  wss://relay.nostr.band
)

PUBLISH="${PUBLISH:-0}"
relay_args=()
if [ "$PUBLISH" = "1" ]; then
  relay_args=("${RELAYS[@]}")
  echo ">>> PUBLISH=1: events will be SENT to: ${RELAYS[*]}" >&2
else
  echo ">>> dry run: building + printing events only (set PUBLISH=1 to send)" >&2
fi

# Kind legend (NIP-published only; custom/app-internal kinds are omitted).
# The two apps DIFFER — each k-tag set is derived from that app's own source:
#   shared:    0=metadata 1=note 3=follows 4=legacy-DM(NIP-04) 6=repost 7=reaction
#              9735=zap 10000=mute-list 10002=relay-list 30000=follow-sets
#              30023=long-form 39089=starter-packs
#   Damus +    42=public-chat 9802=highlights 10015=interests 30315=user-status
#   Notedeck + 14=DM 1059=gift-wrap  (NIP-17 private DMs; Damus uses legacy kind 4 only)
#              10050=DM-relay-list
# (Calendar kinds 31922/31923 are handled by the Horizon app on the platform, not the
#  core Notedeck client, so they're left out of the Notedeck listing.)

echo "=== Damus (kind:31990) ===" >&2
nak event \
  --prompt-sec \
  -k 31990 \
  -d damus \
  -c '{"name":"Damus","display_name":"Damus","about":"nostr client for iOS.","website":"https://damus.io","picture":"https://damus.io/logo_icon.png"}' \
  -t k=0 -t k=1 -t k=3 -t k=4 -t k=6 -t k=7 -t k=42 -t k=9735 -t k=9802 \
  -t k=10000 -t k=10002 -t k=10015 -t k=30000 -t k=30023 -t k=30315 -t k=39089 \
  -t web="https://damus.io/<bech32>" \
  "${relay_args[@]}"
  #                                           ^ generic web/universal-link handler
  # VERIFY the "picture" URL above (and the damus.io/<bech32> universal-link pattern).
  # Optional iOS deep-link scheme, if you want it (VERIFY the scheme format first):
  #   -t ios="damus:<bech32>"

echo "=== Notedeck (kind:31990) ===" >&2
nak event \
  --prompt-sec \
  -k 31990 \
  -d notedeck \
  -c '{"name":"Notedeck","display_name":"Notedeck","about":"A multiplatform nostr client and app platform.","website":"https://damus.io/notedeck","picture":"https://damus.io/img/notedeck-icon.png"}' \
  -t k=0 -t k=1 -t k=3 -t k=4 -t k=6 -t k=7 -t k=14 -t k=1059 -t k=9735 \
  -t k=10000 -t k=10002 -t k=10050 -t k=30000 -t k=30023 -t k=39089 \
  "${relay_args[@]}"
  # Notedeck is a native desktop app with no public web/universal-link handler,
  # so it's advertised via k-tags + metadata only. If Notedeck registers a URL
  # scheme (e.g. nostr:), add a handler tag here, e.g.:
  #   -t web="https://damus.io/notedeck/<bech32>"     # VERIFY
  # picture is live at https://damus.io/img/notedeck-icon.png ; VERIFY the "website" URL above.

echo ">>> done" >&2
