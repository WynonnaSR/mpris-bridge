---
paths:
  - "src/**/*.rs"
---

# Rust Async Rules for mpris-bridge

## Lock usage

- NEVER hold a `RwLock` or `Mutex` guard across an `.await` point
- Correct pattern: acquire lock → clone data → drop lock → then await
  ```rust
  // CORRECT
  let data = ctx.players.read().unwrap().clone();
  drop_implicitly_here;
  some_async_fn(data).await;

  // WRONG — lock held across await
  let guard = ctx.players.read().unwrap();
  some_async_fn(&*guard).await;
  ```

## Blocking calls

- NEVER call blocking IO, `std::thread::sleep`, or `std::process::Command::output()` directly in async functions
- Use `tokio::process::Command` instead of `std::process::Command` in async context
- Use `task::spawn_blocking()` for unavoidable blocking sync operations

## Task spawning

- Prefer named variables for spawned task contexts: `let ctx2 = ctx.clone();`
- Always handle errors from spawned tasks via `eprintln!` at minimum
- Do not silently swallow errors with bare `let _ = ...` on critical operations

## Error handling

- Use `anyhow::Result` and `.context("description")` for all fallible functions
- Use `?` for error propagation, not `.unwrap()` in production paths
- `.unwrap()` is only acceptable for:
  - Static regex patterns that are provably valid
  - Lock poisoning (`.unwrap()` on RwLock is acceptable — poison = bug)
- Always log errors in long-running async loops before `continue`
