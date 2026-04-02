# Dispatch

Terminal kanban board for dispatching Claude Code agents into isolated git worktrees via tmux.

**Stack**: Rust (2021 edition), ratatui TUI, SQLite (rusqlite), Axum HTTP/MCP server, tokio async runtime.

## Build & Test

```bash
cargo build
cargo test
cargo run -- tui
```

Pre-commit hook runs `cargo fmt --check` and `cargo clippy -- -D warnings` automatically — no need to run these manually.

## Allium Specification

`docs/specs/dispatch.allium` is the **source of truth** for domain logic: task lifecycle, status transitions, sub-status invariants, dispatch rules, and epic behavior. Consult it before changing core behavior. Use `allium:tend` and `allium:weed` skills to keep spec and code aligned.

## Architecture

Key patterns that aren't obvious from reading the code:

- **Message → Command**: `App::update()` processes input messages and returns `Command`s (side effects). Keep rendering pure, effects in commands.
- **Inline-mutation convention**: Input handlers in `input.rs` directly mutate `self.input.mode`, cursor positions, and other UI-only state, returning `vec![]` (no commands). This is intentional — not an Elm Architecture violation. The rule: if a state change has no side effects (no DB write, no process spawn, no network call), mutate inline and return empty. If it needs a side effect, return a `Command`.
- **ProcessRunner trait**: Abstraction over git/tmux shell commands. Tests use `MockProcessRunner` — never shell out in tests.
- **TaskPatch builder**: Selective field updates for the database. `None` = don't change, `Some(None)` = set field to NULL.
- **MCP server**: Runs on port 3142 (configurable via `DISPATCH_PORT`). Agents call JSON-RPC methods in `src/mcp/handlers/` to update task status.
- **Integration tests**: Use `Database::open_in_memory()` with a real SQLite instance — no mocking the database layer.

## Tag System

Tags (`bug`, `feature`, `chore`, `epic`) drive dispatch behavior via `DispatchMode::for_task()` in `models.rs`:

| Tag | No plan | Has plan |
|-----|---------|----------|
| `epic` | Brainstorm (ideation, no edits) | Dispatch |
| `feature` | Plan (write implementation plan) | Dispatch |
| `bug`, `chore`, none | Dispatch | Dispatch |

A task with a plan always dispatches directly regardless of tag. Tags are selected during task creation: `b`=bug, `f`=feature, `c`=chore, `e`=epic, Enter=none.

## Timing Constants

- **Tick interval** (2s): `TICK_INTERVAL` in `runtime.rs` — captures tmux output, checks staleness.
- **Status TTL** (5s): `STATUS_MESSAGE_TTL` in `tui/mod.rs` — transient status bar messages auto-clear.
- **PR poll** (30s): `PR_POLL_INTERVAL` in `tui/mod.rs` — polls PR status for tasks in review.

## Key Files

- `src/tui/input.rs` — Key event handlers, inline-mutation convention lives here.
- `src/tui/mod.rs` — `App` struct, `update()` dispatcher, `column_items_for_status()` (hot path called every render to build column contents).
- `src/dispatch.rs` — Agent launch functions including `dispatch_review_agent()` which creates an isolated worktree and tmux window for PR review.
- `src/models.rs` — Domain types, `DispatchMode::for_task()` tag routing logic.
- `src/runtime.rs` — Async event loop, command execution, tick scheduling.

## Documentation

- `docs/reference.md` — Key bindings, configuration, environment variables, troubleshooting
- `docs/specs/` — Allium specifications for domain logic
- `docs/plans/` — Implementation plans (working artifacts, never committed)
