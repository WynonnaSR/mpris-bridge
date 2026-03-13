---
name: install
description: Build release binaries, install to ~/.local/bin, restart systemd service
allowed-tools: Bash(cargo build*), Bash(cp target/release/mpris-bridge* ~/.local/bin/*), Bash(systemctl --user restart mpris-bridged*)
---

Install mpris-bridge to the local system:

1. Build release binaries:
   ```
   cargo build --release
   ```
   Stop if build fails — show errors.

2. Copy binaries:
   ```
   cp target/release/mpris-bridged ~/.local/bin/
   cp target/release/mpris-bridgec ~/.local/bin/
   ```

3. Restart the service:
   ```
   systemctl --user restart mpris-bridged
   ```

4. Check service status after 2 seconds:
   ```
   systemctl --user status mpris-bridged
   ```

Report: binary sizes, install paths, service status (active/failed).
