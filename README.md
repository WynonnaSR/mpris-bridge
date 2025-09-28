# mpris-bridge

mpris-bridge is a lightweight, event‑driven bridge that exposes MPRIS player state as JSON for Waybar/Eww, with built‑in IPC and a tiny CLI.

- Daemon: `mpris-bridged` (reactive player selection via D‑Bus + Hyprland focus, single `playerctl -F` follower)
- CLI: `mpris-bridgec` (play/pause/next/prev/seek, and a `watch` mode for Waybar)
- Runtime (XDG):
  - Snapshot: `$XDG_RUNTIME_DIR/mpris-bridge/state.json`
  - Events: `$XDG_RUNTIME_DIR/mpris-bridge/events.jsonl`
  - Socket: `$XDG_RUNTIME_DIR/mpris-bridge/mpris-bridge.sock` (one JSON command per line)
- Cover art cache: `$XDG_CACHE_HOME/mpris-bridge/art`

Version: 0.3.3

---

## Features

- Event‑driven selection (no polling):
  - D‑Bus (zbus 3.x) reacting to `NameOwnerChanged` and `PropertiesChanged`
  - Hyprland focus hint via `hyprctl -i events`
  - Priority list, include/exclude, remember last, fallback policy
- Resilience:
  - D‑Bus auto‑reconnect with backoff
  - Hypr focus listener auto‑restart when the process exits
  - Follower watchdog (respawn `playerctl -F` if it dies)
- Art handling:
  - Supports `file://` and `http(s)` URLs, cached on disk (SHA1), timeout and copy/symlink modes
- YouTube policy:
  - Firefox + YouTube without `list=` → force `canPrev=0`, `canNext=1`
  - In playlists → defer to real MPRIS capabilities
- IPC:
  - `play-pause`, `next`, `previous`, `seek ±seconds`, `set-position seconds`
  - Optional `--player`; defaults to currently selected one
- CLI (`mpris-bridgec`):
  - Socket‑first with fallback to `playerctl`
  - `watch` mode for Waybar with `--format`, `--truncate`, `--pango-escape`

---

## Requirements

- Runtime: `playerctl`, `systemd` (for `busctl` and user unit), Hyprland `hyprctl` (for focus hints)
- Build: Rust stable (edition 2021), no OpenSSL dev (reqwest uses rustls)

---

## Installation

1) Build and install
```bash
cargo build --release
install -Dm755 target/release/mpris-bridged  ~/.local/bin/mpris-bridged
install -Dm755 target/release/mpris-bridgec ~/.local/bin/mpris-bridgec
```

2) Configuration
- Create `~/.config/mpris-bridge/config.toml` (example below)

3) Systemd (user)
- Create `~/.config/systemd/user/mpris-bridged.service` (example below), then:
```bash
systemctl --user daemon-reload
systemctl --user enable --now mpris-bridged
systemctl --user status mpris-bridged
```

4) Verify runtime
```bash
ls -l "$XDG_RUNTIME_DIR/mpris-bridge"
tail -f "$XDG_RUNTIME_DIR/mpris-bridge/events.jsonl" | jq -r
mpris-bridgec watch --truncate 80 --pango-escape
```

---

## Reloading config

- The daemon reads `~/.config/mpris-bridge/config.toml` at startup only (no periodic reload).
- After changing the config (for example, `[art].cache_dir`), restart the service:
```bash
systemctl --user restart mpris-bridged
```
- `systemctl --user daemon-reload` only reloads unit files, not the app config.

Notes on cache:
- Changing `[art].cache_dir` does not migrate/clean old files automatically.
- You can prune old cache manually:
```bash
rm -rf "${XDG_CACHE_HOME:-$HOME/.cache}/mpris-bridge/art"/*
```

---

## Configuration

Path: `~/.config/mpris-bridge/config.toml`

```toml
# mpris-bridged: MPRIS -> unified JSON for eww/waybar

[selection]
priority        = ["firefox", "spotify", "vlc", "mpv"]
prefer_focused  = true
remember_last   = true
fallback        = "any"   # "any" | "none"

[mpris]
include         = []      # empty = all
exclude         = []
debounce_ms     = 120
position_tick_ms = 500

[art]
enabled         = true
download_http   = true
timeout_ms      = 5000
cache_dir       = "$XDG_CACHE_HOME/mpris-bridge/art"
default_image   = "$HOME/.config/eww/scripts/cover.png"
current_path    = "$HOME/.config/eww/image.jpg"
use_symlink     = false

[output]
snapshot_path   = "$XDG_RUNTIME_DIR/mpris-bridge/state.json"
events_path     = "$XDG_RUNTIME_DIR/mpris-bridge/events.jsonl"
pretty_snapshot = false

[presentation]
truncate_title  = 120
truncate_artist = 120

[logging]
level           = "warn"
```

---

## Systemd unit (user)

Path: `~/.config/systemd/user/mpris-bridged.service`

```ini
[Unit]
Description=mpris-bridge (MPRIS -> unified JSON for eww/waybar)
After=graphical-session.target

[Service]
Type=simple
ExecStart=%h/.local/bin/mpris-bridged
Restart=on-failure
RestartSec=1
Environment=XDG_RUNTIME_DIR=%t
RuntimeDirectory=mpris-bridge

[Install]
WantedBy=default.target
```

---

## Waybar integration

```jsonc
"custom/media": {
  "format": "{}",
  "tooltip": false,
  "max-length": 40,
  "exec": "mpris-bridgec watch --truncate 80 --pango-escape",

  "on-click":        "mpris-bridgec play-pause",
  "on-click-right":  "mpris-bridgec next",
  "on-click-middle": "mpris-bridgec previous",
  "on-scroll-up":    "mpris-bridgec seek 5",
  "on-scroll-down":  "mpris-bridgec seek -5"
}
```

Tips:
- `--pango-escape` prevents Pango markup errors on titles with `' " & < >`.

---

## Eww integration

Listen for event stream (recommended name):

```lisp
(deflisten mpris-bridge-events
  :json true
  :initial "{\"name\":\"\",\"title\":\"\",\"artist\":\"\",\"status\":\"\",\"position\":0,\"positionStr\":\"0:00\",\"length\":0,\"lengthStr\":\"0:00\",\"thumbnail\":\"$HOME/.config/eww/scripts/cover.png\",\"canNext\":0,\"canPrev\":0}"
  "sh -lc 'tail -n0 -F \"$XDG_RUNTIME_DIR/mpris-bridge/events.jsonl\"'")
```

Controls:

```lisp
(button
  :class {mpris-bridge-events.canPrev == 1 ? "back-btn" : "back-btn disabled"}
  :onclick "mpris-bridgec previous --player ${mpris-bridge-events.name}"
  "󰒮")

(button
  :class "play-btn"
  :onclick "mpris-bridgec play-pause --player ${mpris-bridge-events.name}"
  {mpris-bridge-events.status == "Playing" ? "󰓛" : "󰐊"})

(button
  :class {mpris-bridge-events.canNext == 1 ? "next-btn" : "next-btn disabled"}
  :onclick "mpris-bridgec next --player ${mpris-bridge-events.name}"
  "󰒭")

(scale
  :min 0
  :max {mpris-bridge-events.length > 0 ? mpris-bridge-events.length : 1}
  :value {mpris-bridge-events.position}
  :onchange "mpris-bridgec set-position {} --player ${mpris-bridge-events.name}"
  :hexpand true)
```

---

## IPC protocol (Unix socket)

Socket: `$XDG_RUNTIME_DIR/mpris-bridge/mpris-bridge.sock`  
One JSON line per command:
- `{"cmd":"play-pause","player":null}`
- `{"cmd":"next","player":"spotify"}`
- `{"cmd":"previous","player":null}`
- `{"cmd":"seek","offset":5.0,"player":null}`
- `{"cmd":"set-position","position":120.0,"player":null}`

Reply: `{"ok":true}` or `{"ok":false}`.

Prefer `mpris-bridgec` over hand‑crafting JSON.

---

## JSON schema (outputs)

Example event:

```json
{
  "name": "spotify",
  "title": "Song Title",
  "artist": "Artist",
  "status": "Playing",
  "position": 63.5,
  "positionStr": "1:03",
  "length": 244.64,
  "lengthStr": "4:04",
  "thumbnail": "/home/user/.config/eww/image.jpg",
  "canNext": 1,
  "canPrev": 1
}
```

Units:
- `position`, `length` in seconds (float)
- `positionStr`, `lengthStr` as `M:SS`

---

## Cache management

- Location: `$XDG_CACHE_HOME/mpris-bridge/art`
- No built‑in size/TTL limit yet (grows with unique art URLs).
- You can prune manually or set up a user timer to delete old files, e.g. weekly prune of files older than 60 days.

Example (optional):

```bash
# ~/.local/bin/mpris-bridge-cache-prune.sh
#!/usr/bin/env bash
set -euo pipefail
CACHE_DIR="${XDG_CACHE_HOME:-$HOME/.cache}/mpris-bridge/art"
DAYS="${1:-60}"
if [[ -d "$CACHE_DIR" ]]; then
  find "$CACHE_DIR" -type f -mtime +"$DAYS" -print -delete
fi
```

```ini
# ~/.config/systemd/user/mpris-bridge-cache-prune.service
[Unit]
Description=Prune old album arts from mpris-bridge cache

[Service]
Type=oneshot
ExecStart=%h/.local/bin/mpris-bridge-cache-prune.sh 60
```

```ini
# ~/.config/systemd/user/mpris-bridge-cache-prune.timer
[Unit]
Description=Weekly prune of mpris-bridge cache

[Timer]
OnCalendar=weekly
Persistent=true
AccuracySec=1h
RandomizedDelaySec=1h
Unit=mpris-bridge-cache-prune.service

[Install]
WantedBy=timers.target
```

Enable:
```bash
install -Dm755 ~/.local/bin/mpris-bridge-cache-prune.sh ~/.local/bin/mpris-bridge-cache-prune.sh
systemctl --user daemon-reload
systemctl --user enable --now mpris-bridge-cache-prune.timer
```

---

## Troubleshooting

- Config changes not applied:
  - Run `systemctl --user restart mpris-bridged`
  - `daemon-reload` affects units only
- Waybar Pango warnings:
  - Use `mpris-bridgec watch --pango-escape` or `"escape": true` in the module
- No events written:
  - `journalctl --user -u mpris-bridged -e -n 200`
  - Check follower: `ps -ef | grep 'playerctl .* -F'`
  - Check D‑Bus: `busctl --user monitor org.mpris.MediaPlayer2.spotify`
- Art not updating:
  - Ensure `mpris:artUrl` is present and HTTP download is allowed in `[art]`

---

## License

MIT (or your preferred license)