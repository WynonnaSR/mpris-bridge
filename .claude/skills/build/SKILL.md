---
name: build
description: Build mpris-bridge in release mode and show errors concisely
argument-hint: "[--release|--debug]"
allowed-tools: Bash(cargo build*)
---

Build the project with `cargo build --release` (or `--debug` if $ARGUMENTS contains "debug").

Run:
```
cargo build --release 2>&1
```

Then:
- If build succeeded: report "Build OK" and binary sizes from `target/release/`
- If build failed: show ONLY the error lines (lines starting with `error[` or `error:`), grouped by file. Skip warnings unless there are no errors.
- If there are warnings only: summarize count and list the most important ones (unused imports, dead code).

Do not show the full cargo output — be concise.
