# A scratch notedeck keeps off the tool socket

A notedeck started with its own --datapath still binds the user's
$XDG_RUNTIME_DIR/notedeck/notedeck.sock, taking it from their running notedeck.

- headway:headway/destroy-weather-whip — a headless acceptance run took the socket from jb55's long-running notedeck, so `nd` couldn't reach it until it restarted; scripts/sns-acceptance now sets NOTEDECK_SOCKET, and scripts/headway-live-rig still doesn't
