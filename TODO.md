# TODO

Priority scale: high, medium, low, very-low

1) Eww click-to-seek not working
- Description: Clicking the Eww scale should set absolute position. Currently it doesn’t always move playback, while Waybar scroll seek works.
- Likely cause: `:onchange {}` value timing/format; some players prefer integer seconds; missing debounce.
- Plan:
  - Confirm Eww passes seconds; round in `mpris-bridgec set-position` (already rounds).
  - Add optional `--debounce-ms` to `mpris-bridgec set-position` to coalesce rapid slider updates.
  - If Eww supports “on-release” event for scale, switch to it.
- Priority: high

2) YouTube prev/next temporarily disabled after switching
- Description: After prev/next on YouTube, `canPrev/canNext` appear disabled for a short time (<~1–2s) and re‑enable after play‑pause.
- Likely cause: Capabilities update later than status/track; we read caps too early.
- Plan:
  - Add a one‑shot delayed caps recheck (250–500 ms) after track/status/URL change.
  - Soft cache with min interval (>=250 ms) and short grace period to avoid flicker from true→false→true.
- Priority: medium

3) Marquee (scrolling text) for long labels (Waybar/Eww)
- Description: Very long `artist - title` don’t fit.
- Plan:
  - Extend `mpris-bridgec watch` with `--marquee <width> --marquee-speed <ms>` (rotating/scroll output, still Pango‑escaped).
  - Eww alternative: CSS keyframes marquee or periodic text shift via `defpoll`.
- Priority: medium

4) Yandex.Music length shows 00:00 while position is valid
- Description: In browser playback, `mpris:length` is 0 but position advances.
- Plan:
  - If `length == 0 && position > 0`, attempt a second read once after a short delay.
  - If still 0, avoid showing `0:00` (display blank/unknown) to reduce confusion.
- Priority: medium

5) MPV integration
- Description: No data appears from mpv.
- Plan:
  - Ensure mpv’s MPRIS script is enabled (`--script=.../mpris.so` or distro package).
  - Consider optional mpv IPC socket integration as a secondary provider.
- Priority: very-low

6) Build warnings cleanup
- Description: Eliminate warnings on `cargo build --release`.
- Plan:
  - `cargo clippy --all-targets -- -D warnings`
  - Remove unused imports, narrow `#[allow(...)]`, feature‑gate optional parts (e.g., Hypr focus) if helpful.
- Priority: medium

7) Manual test cases
- Plan:
  - Basics:
    - Start `mpris-bridged`, verify files exist, run `mpris-bridgec watch`.
    - Spotify: Play/Pause/Next/Prev; `canNext/canPrev` update.
    - Focus switch Firefox ↔ Spotify; selection follows priority/focus hint.
  - YouTube:
    - No playlist: `canPrev=0`, `canNext=1`.
    - Playlist: both caps true; verify behavior after delayed caps recheck is implemented.
  - Yandex:
    - Position increments; check whether `length` updates after delay.
  - Fault tolerance:
    - Kill `playerctl -F`; watchdog respawns within ~2s.
    - Restart Hyprland; focus listener restarts and updates selection.
    - Simulate D‑Bus hiccup; DBus listener reconnects (backoff).
  - Eww:
    - Click slider at various positions; verify rounding/debounce.
  - Waybar:
    - Titles with `' " & < >` don’t trigger Pango warnings with `--pango-escape`.
- Priority: medium

8) Config hot‑reload
- Description: Currently config changes require a service restart.
- Plan:
  - Implement SIGHUP and/or inotify‑based reload; re‑read `~/.config/mpris-bridge/config.toml`.
  - Apply safe fields on the fly (presentation, art timeouts, include/exclude).
  - For disruptive changes (priority/focus rules), recompute selection and/or recreate follower safely.
- Priority: medium

9) Cache limits/retention
- Description: Cover art cache grows unbounded.
- Plan:
  - Add config limits: e.g., `[art] max_cache_mb`, `[art] ttl_days`, `[art] prune_on_start`.
  - Optional periodic prune (systemd timer) documented in README.
- Priority: medium