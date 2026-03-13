---
name: clippy
description: Run clippy linter and summarize actionable warnings
allowed-tools: Bash(cargo clippy*)
---

Run:
```
cargo clippy --all-targets 2>&1
```

Then summarize results:
- Group warnings by category (unused, style, perf, correctness)
- For each warning show: file:line — warning message — suggested fix
- Skip warnings that are already suppressed with `#[allow(...)]`
- Highlight any `clippy::correctness` or `clippy::perf` warnings first — these are most important
- If clean: report "Clippy clean ✓"

Be concise — no raw clippy output, just the summary table.
