# mpris-bridge: Architecture, Algorithms, and Integration

This document describes the current implementation of:
- `main.rs` — the daemon (“bridged”)
- `mpris-bridgec.rs` — the CLI client (“bridgec”)

Contents:
- Overview
- Components and interactions
- Algorithms (textual and visual)
- Data formats and IPC API
- Configuration
- Integration with Waybar/Eww and other UIs
- End-user features
- Reliability, performance, and safety
- Debugging and diagnostics
- FAQ and notes


## Overview

mpris-bridge is a lightweight layer between MPRIS players and UI surfaces (Waybar, Eww, etc.). It:

- Automatically selects the “active” player based on statuses, priorities, and Hyprland window focus.
- Subscribes to D‑Bus signals only for MPRIS (narrow match rules) and reacts to player appear/disappear and property changes.
- Maintains a single “follower” process (`playerctl -F`) for the selected player to stream metadata, position, and artwork.
- Exports state to an atomic snapshot `state.json` and a stream `events.jsonl`.
- Exposes a local UNIX socket for control (play-pause/next/previous/seek/set-position).
- Ships a CLI client `mpris-bridgec` for control and a convenient “watch” mode for text labels.


## Components and interactions

- Daemon `mpris-bridged`:
  - Subscribes to D‑Bus (narrow filters for MPRIS).
  - Listens to Hyprland via `hyprctl -i events` to infer focused app → focus hint.
  - Maintains a set of known players and their statuses.
  - Computes and maintains the “selected” player.
  - Runs a single follower (`playerctl -F`) bound to the selected player.
  - Writes JSON state and event lines.
  - Serves control commands over a local UNIX socket.

- CLI `mpris-bridgec`:
  - Sends JSON commands over the socket (with fallback to direct `playerctl` if the socket is unavailable).
  - Watch mode: prints a formatted label from `state.json` at start, then tails `events.jsonl` for live updates (with formatting, truncation, optional Pango escaping).

- Files and socket (defaults under `$XDG_RUNTIME_DIR/mpris-bridge/`):
  - `state.json` — latest full UI state (UiState).
  - `events.jsonl` — event stream (one JSON UiState per line).
  - `mpris-bridge.sock` — IPC socket (0600 permissions).


## Algorithms

### 1) Selecting the active player

Inputs:
- Known players: from `playerctl -l`, filtered by `include`/`exclude` prefixes.
- Status map: “name → status (Playing/Paused/Stopped)”.
- Focus hint: derived from Hyprland active window class (e.g., firefox/spotify/vlc/mpv).

Steps:
1. Build `players` = known ∩ include/exclude.
2. If `players` is empty → None.
3. Build `playing` = subset with status “Playing”.
4. If `playing` is non-empty:
   - If `focus hint` exists and any `playing` begins with the hint prefix → select it.
   - Else traverse `priority` (from config) and select the first `playing` that matches a prefix.
   - Else select the first `playing`.
5. If `playing` is empty:
   - If `remember_last` and `last_selected` is still present in `players` → select it.
   - Else if `focus hint` matches any `players` prefix → select it.
   - Else traverse `priority` against `players` and pick the first match.
   - Else if `fallback == "any"` → select `players[0]`, otherwise None.

On selection change:
- Immediately emit a “quick snapshot” for visual instant update.
- Restart the follower on the new selected player.

Visual (simplified):

```
[Known players + statuses + focus hint]
            |
            v
    Any "Playing"? --- no ---> remember_last? --- yes ---> last still present? --- yes ---> select last
         |                                     |                                |                      |
        yes                                    no                               no                     |
         v                                      v                                v                     v
focus hint among Playing?               focus hint among players?      priority among players?  fallback == any?
         |                                      |                               |                     |
        yes                                    yes                             yes                   yes
         v                                      v                               v                     v
     select by hint                        select by hint                 select by priority       select first
         |                                      |                               |                     |
        no                                     no                              no                    no
         v                                      v                               v                     v
priority among Playing?              fallback == any?                       return None           return None
    yes → select; no → first
```

### 2) D‑Bus subscription and preventing broker memory growth

To avoid large queues in `dbus-broker`, the daemon uses narrow `add_match` rules:

- `NameOwnerChanged` only for names under `org.mpris.MediaPlayer2.*` (via `arg0namespace`).
- `PropertiesChanged` only at path `/org/mpris/MediaPlayer2` and only for interfaces:
  - `org.mpris.MediaPlayer2.Player`
  - `org.mpris.MediaPlayer2`
  (via `path` + `arg0`)

This drastically reduces signal volume delivered to the process, preventing queue buildup in the broker.

Additionally, heavy operations are debounced and offloaded to background tasks:

- `seed_players()` (enumerate players) — at most once per ~300 ms.
- `refresh_statuses()` (mass status query) — at most once per ~250 ms.
- Heavy work executes in `task::spawn`, keeping the main D‑Bus loop responsive.

Visual:

```
DBus broker -> bridged (MessageStream)
     |            |
     |   [Header match: iface/member/path/arg0?] --- no ---> drop early
     |            |
    yes           v
               [Debounce gate]
                  |       \
                 pass     hold
                  |
            task::spawn(async {
              seed / refresh
              recompute_selected
              quick snapshot if changed
            })
```

### 3) Follower for the selected player

- Spawns: `playerctl -p <name> metadata --format "...8 fields..." -F`
- Each line contains:
  `{{status}}|{{playerName}}|{{title}}|{{artist}}|{{mpris:length}}|{{mpris:artUrl}}|{{position}}|{{xesam:url}}`

On each line:
- Update status map for the selected player.
- If one of status/title/artist/url changed:
  - Query CanGoNext/CanGoPrevious via `busctl get-property` (cached per change).
  - Apply Firefox/YouTube policy: if no playlist → allow next, disable prev.
- Build `UiState`:
  - Truncate title/artist to configured limits.
  - Convert µs to seconds for length/position and format `MM:SS`.
  - Resolve thumbnail:
    - `file://...` → copy/symlink to `current_cover`.
    - `http(s)://...` → cache by SHA‑1 in `cache_dir`, then copy/symlink.
    - Otherwise use `default_cover`.
- Persist:
  - Atomically write `state.json` (tmp file rename).
  - Append a JSON line to `events.jsonl`.

Visual:

```
[playerctl -F] ---> [follower task]
         |                 |
         |           parse 8 fields
         |                 |
         |        update status[name]
         |                 |
         |   read caps if (status/title/artist/url) changed
         |                 |
         |        build UiState + cover
         |                 |
         |   write state.json + append events.jsonl
```

### 4) IPC server

- UNIX socket `mpris-bridge.sock` (0600), synchronous handler runs on a blocking thread:
  - Reads JSON commands with `cmd` and optional `player`.
  - Resolves target player: explicit `player` or current selection.
  - Executes via `playerctl`:
    - `play-pause`, `next`, `previous`
    - `seek {offset}` → `playerctl position "N+" | "N-"`
    - `set-position {position}` → `playerctl position "N"`
  - Replies with `{"ok":true}\n` or `{"ok":false}\n`.

- The CLI client sends the same JSON and expects exactly one reply line.

Visual:

```
client (bridgec/UI) --json--> [UNIX socket server] --runs--> playerctl ...
                                 |                           ^
                                 v                           |
                            {"ok":true/false}  <------------
```


## Data formats and IPC API

### UiState (snapshot and event lines)
Example (camelCase):
```json
{
  "name": "spotify",
  "title": "Song Title",
  "artist": "Artist",
  "status": "Playing",
  "position": 42.1,
  "positionStr": "0:42",
  "length": 210.0,
  "lengthStr": "3:30",
  "thumbnail": "/home/user/.config/eww/image.jpg",
  "canNext": 1,
  "canPrev": 1
}
```

- `state.json` — always the latest snapshot (written atomically).
- `events.jsonl` — one UiState JSON per line (append-only stream).

### IPC commands (JSON over UNIX socket)
- Play/pause:
```json
{"cmd":"play-pause","player":"optional_name"}
```
- Next/previous:
```json
{"cmd":"next","player":"optional_name"}
{"cmd":"previous","player":"optional_name"}
```
- Seek relative seconds:
```json
{"cmd":"seek","offset":5.0}     // forward 5s
{"cmd":"seek","offset":-10.0}   // back 10s
```
- Set absolute position:
```json
{"cmd":"set-position","position":120.0}
```

Reply: `{"ok":true}\n` or `{"ok":false}\n`.


## Configuration

Read from `~/.config/mpris-bridge/config.toml`.

- `selection`:
  - `priority = ["firefox", "spotify", "vlc", "mpv"]`
  - `remember_last = true`
  - `fallback = "any" | "none"`
  - `include = []` — allowed MPRIS name prefixes
  - `exclude = []` — excluded prefixes

- `art`:
  - `enabled = true`
  - `download_http = true`
  - `timeout_ms = 5000`
  - `cache_dir`, `default_image`, `current_path`, `use_symlink`

- `output`:
  - `snapshot_path` (default `$XDG_RUNTIME_DIR/mpris-bridge/state.json`)
  - `events_path` (default `$XDG_RUNTIME_DIR/mpris-bridge/events.jsonl`)
  - `pretty_snapshot = false`

- `presentation`:
  - `truncate_title = 120`
  - `truncate_artist = 120`

- `logging`:
  - `level = "warn"` (reserved, not actively used)

Path tokens are expanded (`$HOME`, `$XDG_RUNTIME_DIR`, etc.) by `expand()`.


## Integration with Waybar/Eww and other UIs

Two common consumption modes:

1) Via `mpris-bridgec watch`:
   - On start: prints one formatted line using `state.json`.
   - Then tails `events.jsonl` for updates.
   - Options:
     - `--format` template, e.g. `"{artist}{sep}{title}"`
     - `--truncate N` to shorten the line (`…` suffix)
     - `--pango-escape` to escape `& < > ' "` for Pango markup

   Example Waybar custom module:
   ```json
   {
     "custom/mpris": {
       "exec": "mpris-bridgec watch --format \"{artist}{sep}{title}\" --pango-escape"
     }
   }
   ```

2) Reading files directly:
   - Poll `state.json` or follow `events.jsonl`.
   - `events.jsonl` is best for reactive updates without timers.
   - For polling, 200–500 ms intervals are usually enough for smooth UI.

Control buttons:
- Execute `mpris-bridgec play-pause`, `mpris-bridgec next`, `mpris-bridgec previous`, `mpris-bridgec seek +5`, `mpris-bridgec set-position 60`, etc.


## End-user features

- Smart auto-selection of the active player (Playing/focus/priority/last‑known).
- Instant updates of title/artist/position for the selected player.
- Artwork support:
  - Local files (`file://`), HTTP(S) with caching, or default image.
- Navigation capabilities exposed (`canNext`, `canPrev`) with a special policy for YouTube in Firefox (no playlist → next only).
- Control via CLI or IPC from any UI.
- Highly responsive and resource-efficient (no D‑Bus queue buildup).


## Reliability, performance, and safety

- D‑Bus:
  - Narrow `add_match` filters eliminate unrelated signals → low broker queues → stable memory use.
  - Main loop only parses headers and schedules heavy tasks asynchronously.

- Follower watchdog:
  - A periodic tick (every 2s) checks `follower_alive`; respawns if needed.

- File I/O:
  - `state.json` is written atomically via a temp file rename.
  - `events.jsonl` is append-only (see “Notes on events.jsonl” below).

- IPC socket:
  - Created with 0600 permissions, user-scoped.

- Safety:
  - No `unsafe` Rust.
  - External tools (`playerctl`, `busctl`, `hyprctl`) are used with suppressed stdio where appropriate.


## Debugging and diagnostics

Helpful commands:
- Broker status:
  - `systemctl --user status dbus-broker.service`
  - `watch -n1 "cat /proc/$(systemctl --user show -p MainPID --value dbus-broker.service)/status | egrep 'VmRSS|VmSize'"`
- Observe MPRIS signals:
  - `busctl --user monitor "type='signal',interface='org.freedesktop.DBus.Properties',path='/org/mpris/MediaPlayer2'"`
  - `busctl --user monitor "type='signal',interface='org.freedesktop.DBus',member='NameOwnerChanged',arg0namespace='org.mpris.MediaPlayer2'"`
- Player list/status:
  - `playerctl -l`
  - `playerctl -p <name> status`
- Logs:
  - Launch the daemon in a terminal to see `eprintln!` diagnostics.
- IPC test:
  - `printf '{"cmd":"play-pause"}\n' | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/mpris-bridge/mpris-bridge.sock`


## FAQ and notes

- Why are updates “instant”?
  - The selected player’s metadata is streamed from `playerctl -F` (no polling, no debounce).

- Why debounce on D‑Bus?
  - To avoid running heavy `playerctl -l` and mass `status` too frequently, and to keep the D‑Bus processing loop fast.

- Can selection switch even faster?
  - It’s already visually instant in normal use. You can tweak debounce (e.g., 150–200 ms) if you need more aggressiveness.

- Notes on `events.jsonl` growth:
  - `state.json` does not grow; it’s a single snapshot.
  - `events.jsonl` is an append-only stream and will grow during the session (typically inside `$XDG_RUNTIME_DIR`, cleared on logout).
  - If you only need the latest state, you can:
    - Use `state.json` exclusively (polling or inotify).
    - Manually truncate in place (safe for tailing readers):
      - `: > "$XDG_RUNTIME_DIR/mpris-bridge/events.jsonl"`
    - Future work may add a configurable size limit and safe in-place truncation.

- What if a player is a non-standard MPRIS implementation?
  - Most players use path `/org/mpris/MediaPlayer2` and interfaces `org.mpris.MediaPlayer2(.Player)`.
  - The follower ensures the selected player’s data is current; `NameOwnerChanged` (filtered by prefix) still detects appear/disappear.


---

If you need, we can:
- Expose debounce and event log limits in `config.toml`.
- Switch capability/status reads from external tools to direct zbus calls.
- Add built-in events log rotation with in-place truncation to keep tailing clients unaffected.