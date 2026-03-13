# mpris-bridge

Event-driven MPRIS media player bridge for **Wayland / Hyprland**.
Streams media player metadata as JSON for Waybar/Eww.

## Binaries

- `mpris-bridged` — daemon (`src/main.rs` or modular `src/`)
- `mpris-bridgec` — CLI client (`src/bin/mpris-bridgec.rs`)

## Build & Install

```bash
cargo build --release
cp target/release/mpris-bridged ~/.local/bin/
cp target/release/mpris-bridgec ~/.local/bin/
systemctl --user restart mpris-bridged
```

## Key Constraints (NEVER violate)

- `#![deny(unsafe_code)]` — unsafe Rust is forbidden
- Never hold `RwLock` across `.await` — clone data first, then drop lock
- No blocking calls in async context — use `task::spawn_blocking()` for sync IO
- D-Bus match rules must stay narrow — prevents dbus-broker memory bloat
- Target platform: **Linux + Wayland + Hyprland only**

## Architecture

```
D-Bus (zbus) ──┐
Hyprland IPC ──┤──▶ Selection Engine ──▶ Follower (playerctl -F) ──▶ state.json
IPC Socket  ◀──┘                                                  ──▶ events.jsonl
```

Key subsystems:
- **D-Bus listener** — narrow match filters, debounced (300ms seed, 250ms refresh)
- **Selection engine** — priority list + Hyprland focus hint + playback status + remember-last
- **Follower manager** — single `playerctl -F` process, watchdog respawns every 2s
- **Cover art handler** — local `file://` + HTTP(S) with SHA-1 cache
- **Hyprland focus** — native IPC socket (`$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock`)
- **IPC server** — Unix socket, JSON commands, blocking thread per connection

## Runtime Paths

| Purpose | Path |
|---|---|
| Snapshot | `$XDG_RUNTIME_DIR/mpris-bridge/state.json` |
| Event stream | `$XDG_RUNTIME_DIR/mpris-bridge/events.jsonl` |
| IPC socket | `$XDG_RUNTIME_DIR/mpris-bridge/mpris-bridge.sock` |
| Config | `~/.config/mpris-bridge/config.toml` |
| Art cache | `$XDG_CACHE_HOME/mpris-bridge/art/` |

## JSON Output Schema (DO NOT CHANGE — Waybar/Eww depend on this)

```json
{
  "name":        "spotify",
  "title":       "Song Title",
  "artist":      "Artist Name",
  "status":      "Playing|Paused|Stopped",
  "position":    63.5,
  "positionStr": "1:03",
  "length":      244.64,
  "lengthStr":   "4:04",
  "thumbnail":   "/path/to/cover.jpg",
  "canNext":     1,
  "canPrev":     1
}
```

## IPC Command Format (DO NOT CHANGE)

```json
{"cmd": "play-pause"}
{"cmd": "next"}
{"cmd": "previous"}
{"cmd": "seek", "offset": 10.0}
{"cmd": "set-position", "position": 60.0}
```

Optional `"player"` field targets a specific player instance.

## External Runtime Dependencies

- `playerctl` — metadata streaming and media control
- `busctl` — reading D-Bus properties (CanGoNext/CanGoPrevious)
- Hyprland IPC socket — window focus events (no `hyprctl` subprocess needed)
- `systemd --user` — service management

## Code Patterns

**When parsing playerctl output:** always expect exactly 8 pipe-separated fields:
`status|playerName|title|artist|length_us|artUrl|position_us|url`

**When writing D-Bus code:** keep `add_match` rules narrow — filter by namespace, path, and interface simultaneously.

**When adding config options:** add `#[serde(default)]`, update `impl Default`, and update `examples/config/config.toml`.

**When modifying selection logic:** the priority order is:
1. Focus hint + playing
2. Priority list + playing
3. Any playing
4. Remember-last
5. Focus hint (paused)
6. Priority list (paused)
7. Fallback any / none

## Module Structure (target)

```
src/
  main.rs        — fn main() only
  config.rs      — Config structs + Default impls
  context.rs     — Ctx struct
  state.rs       — UiState + write_state()
  utils.rs       — fmt_time, expand, truncate, ensure_dirs, map_class_to_hint
  selection.rs   — recompute_selected, set_selected_*
  follower.rs    — spawn_follower, follower_manager, emit_quick_snapshot
  art.rs         — update_art, ensure_current_cover
  ipc.rs         — IpcCmd, ipc_server_blocking
  dbus.rs        — dbus_listener, seed_players, refresh_statuses
  hyprland.rs    — hypr_focus_listener
```
