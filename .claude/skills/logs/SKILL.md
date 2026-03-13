---
name: logs
description: Show recent mpris-bridged service logs
argument-hint: "[lines]"
allowed-tools: Bash(journalctl --user -u mpris-bridged*)
---

Show service logs. Number of lines = $ARGUMENTS (default: 50).

```
journalctl --user -u mpris-bridged -n ${ARGUMENTS:-50} --no-pager
```

After showing logs, analyze and highlight:
- Any `error:` lines — these need attention
- Reconnect/backoff events — indicate instability
- `write_state error` — file IO problems
- `spawn follower failed` — playerctl issues

Summarize: "X errors, Y reconnects in last N lines" — then show raw logs.
