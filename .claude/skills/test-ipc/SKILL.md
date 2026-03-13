---
name: test-ipc
description: Send a test IPC command to the running daemon and verify response
argument-hint: "[play-pause|next|previous|seek N|status]"
allowed-tools: Bash(echo *), Bash(cat *)
---

Test the IPC socket of the running mpris-bridged daemon.

Socket path: `$XDG_RUNTIME_DIR/mpris-bridge/mpris-bridge.sock`

Command from $ARGUMENTS (default: show current state):

If $ARGUMENTS is empty or "status":
```bash
cat "$XDG_RUNTIME_DIR/mpris-bridge/state.json" | python3 -m json.tool
```

If $ARGUMENTS is a command like "play-pause", "next", "previous":
```bash
echo '{"cmd":"$ARGUMENTS"}' | socat - UNIX-CONNECT:"$XDG_RUNTIME_DIR/mpris-bridge/mpris-bridge.sock"
```

If $ARGUMENTS is "seek N" (e.g. "seek 10"):
```bash
echo '{"cmd":"seek","offset":N}' | socat - UNIX-CONNECT:"$XDG_RUNTIME_DIR/mpris-bridge/mpris-bridge.sock"
```

Report the response and whether `{"ok":true}` was received.
If socket not found — report that daemon is not running.
