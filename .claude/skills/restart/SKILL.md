---
name: restart
description: Restart mpris-bridged systemd service and show status
allowed-tools: Bash(systemctl --user restart mpris-bridged*), Bash(systemctl --user status mpris-bridged*)
---

Restart the daemon:

```
systemctl --user restart mpris-bridged
systemctl --user status mpris-bridged
```

Report:
- Active state (active/failed/activating)
- PID and uptime
- Last 5 log lines if status is failed
