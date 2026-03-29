# Confirmation Before Done

## Problem

Moving a task to Done should require human confirmation for two reasons:

1. The Done column fills up fast and it's hard to find tasks there — accidental moves are costly
2. Agents should not be able to autonomously move tasks to Done

## Design

### TUI: Confirmation when moving Review→Done via `m` key

When `m` (or batch `M`) is pressed and a task would transition from Review to Done:

1. Enter `InputMode::ConfirmDone(TaskId)` (new variant)
2. Show `"Move task to Done? (y/n)"` in the status bar (reuses existing confirmation pattern)
3. `y` → move task to Done, persist. **No cleanup** — worktree and tmux window are left intact. Cleanup happens later when the task is archived (`x`), which already calls `take_cleanup()`.
4. Any other key → cancel, return to Normal mode

**Batch moves:** If any selected task is in Review, show `"Move N tasks to Done? (y/n)"` and gate the entire batch on confirmation. Non-Review tasks in the selection move immediately; the Review→Done tasks wait for the `y` confirmation.

### MCP: Reject `status=done`

In `handle_update_task`, when the requested status is `Done`:

- Return a JSON-RPC error (code `-32602`, invalid params) with message: `"Cannot set status to done via MCP. Please ask the human operator to move the task to done from the TUI."`
- No database write occurs

This is a simple early-return check before the `patch_task` call.

### Unchanged flows

- **`f` (Finish):** Merge + cleanup with existing confirmation — no changes needed
- **Archive (`x`):** Already cleans up leftover worktree/tmux via `take_cleanup()` — serves as cleanup safety net for tasks that reached Done without cleanup

## Files to modify

| File | Change |
|------|--------|
| `src/tui/types.rs` | Add `InputMode::ConfirmDone(TaskId)`, add `Message::ConfirmDone` and `Message::CancelDone` |
| `src/tui/mod.rs` | Add `handle_confirm_done()` and `handle_cancel_done()`. Modify `handle_move_task()` to intercept Review→Done. |
| `src/tui/input.rs` | Add `handle_key_confirm_done()` handler, route `InputMode::ConfirmDone` in key dispatch |
| `src/tui/ui.rs` | Add status bar rendering for `InputMode::ConfirmDone` |
| `src/mcp/handlers.rs` | Add early-return rejection when `status=done` in `handle_update_task` |
| `src/tui/tests.rs` | Tests for: confirmation flow, cancel flow, batch with mixed statuses, MCP rejection |
