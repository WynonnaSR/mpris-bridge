---
paths:
  - "src/dbus.rs"
  - "src/hyprland.rs"
  - "src/main.rs"
---

# D-Bus and Hyprland Rules

## D-Bus match rules

- ALWAYS use narrow `add_match` filters — broad subscriptions cause dbus-broker memory bloat
- Required filters for MPRIS: `arg0namespace='org.mpris.MediaPlayer2'` on NameOwnerChanged
- Required filters for Properties: `path='/org/mpris/MediaPlayer2'` + `arg0='org.mpris.MediaPlayer2.Player'`
- Never subscribe to all PropertiesChanged signals without path/interface filtering

## D-Bus debouncing

- Seed operations (playerctl -l): minimum 300ms debounce between calls
- Refresh operations (status update): minimum 250ms debounce
- Use `Instant::elapsed()` pattern — check elapsed before spawning task

## Hyprland IPC

- Use native Hyprland socket — do NOT spawn `hyprctl` subprocess for events
- Event socket: `/tmp/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock`
- Request socket: `/tmp/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock`
- Send `j/activewindow` to request socket to get active window JSON
- Always handle `HYPRLAND_INSTANCE_SIGNATURE` not being set (graceful retry)
- Reconnect on socket disconnect with 1-2s delay

## Reconnection

- D-Bus: exponential backoff 1s → 30s max on connection error
- Hyprland socket: fixed 1-2s retry delay (socket recreates on Hyprland restart)
