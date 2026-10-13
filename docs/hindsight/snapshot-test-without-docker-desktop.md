# scripts/snapshot-test can render on macOS when Docker Desktop is down

On macOS `--update` only renders inside the lavapipe container, so with Docker
Desktop down a card can't record the pngs it stales or adds and lands them stale.

- headway:dave/beef-calm-cross — Docker Desktop wouldn't start from the session and rendering on the NixOS host was out of reach, so the clamped-output change landed without its two new pngs and with ~9 tool-row pngs stale
