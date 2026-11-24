# App caches pump while the app is closed

Chrome runs `App::update` only for opened apps, so a cache an app shares with its
inline renderers goes stale in other apps until the user opens it once.

- headway:headway/blanket-push-base — retiring headway's author fold dropped the read-path pump `RealtimeCache::revision` gave plaintext-board chips; with no per-session hook to pump `BoardCache`, chips in Dave stay a snapshot until Headway is opened
