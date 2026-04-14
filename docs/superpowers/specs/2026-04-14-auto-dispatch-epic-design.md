# Auto Dispatch Epic — Design Spec

**Date:** 2026-04-14
**Status:** Approved

## Context

When an epic has a plan and subtasks, pressing `d` dispatches the first backlog subtask. As agents complete their work they call the `dispatch_next` MCP tool, which automatically chains to the next backlog subtask. This creates a fully automatic pipeline: one agent finishes → next one starts → and so on until all subtasks are done.

This is desirable for most epics, but sometimes the user wants to review intermediate results before dispatching the next task. The feature adds a per-epic `auto_dispatch` flag that controls whether `dispatch_next` chains automatically or stops and waits for manual dispatch.

---

## Data Model

New column on the `epics` table:

```sql
ALTER TABLE epics ADD COLUMN auto_dispatch BOOLEAN NOT NULL DEFAULT 1;
```

- Default: `true` — existing epics keep current behaviour, no migration of data needed.
- The `Epic` struct in `src/models.rs` gains `auto_dispatch: bool`.
- `EpicPatch` in `src/db/mod.rs` gains `.auto_dispatch(bool)`.
- `db/queries.rs` reads and writes the field in the existing epic CRUD queries.
- New migration registered in `MIGRATIONS` array in `src/db/migrations.rs`. Schema version test updated.

---

## MCP Behaviour

In `handle_dispatch_next()` (`src/mcp/handlers/tasks.rs`), after finding the next backlog task, fetch the parent epic and check the flag:

```rust
let epic = db.get_epic(next_task.epic_id)?;
if !epic.auto_dispatch {
    return JsonRpcResponse::ok(id, json!({"content": [{"type": "text", "text":
        format!("auto dispatch is disabled for epic #{} — dispatch the next task manually",
            parsed.epic_id)
    }]}));
}
// ... proceed with dispatch
```

- Returns the same informational-message pattern as "no backlog tasks" — not an error.
- The calling agent receives a clear message and exits cleanly without retrying.

---

## UI

### Header indicator (epic view only)

In `render_title()` in `src/tui/ui.rs`, when in `ViewMode::Epic`, add an entry to `right_parts` (alongside the notification bell):

| State | Text | Style |
|-------|------|-------|
| `auto_dispatch = true` | `"auto dispatch [U]"` | Green |
| `auto_dispatch = false` | `"manual dispatch [U]"` | Muted |

The indicator is always visible when viewing an epic so the user always knows the current state and that `U` is the toggle key.

### Key binding

- Key: `U` (Shift+u), scoped to `ViewMode::Epic` only.
- Sends `Message::ToggleEpicAutoDispatch(epic_id)`.
- Handler in `src/tui/mod.rs` patches the epic: `db.patch_epic(epic_id, EpicPatch::new().auto_dispatch(!current))` then triggers a DB refresh.
- No kanban-board binding — `U` only fires inside an epic view.

### Action hints

Add `[U] auto dispatch` to the epic view key hint bar (`src/tui/ui.rs`, around line 2529).

---

## Allium Spec Updates

- `docs/specs/epics.allium`: add `ToggleAutoDispatch` rule.
- `docs/specs/epics.allium`: add `auto_dispatch` guard to `DispatchNextViaMcp` rule.

---

## Testing

### Unit tests

1. **DB migration test** (`src/db/tests.rs`): create DB at previous schema version, run migration, assert `auto_dispatch` column exists with default `1`.
2. **`toggle_auto_dispatch` DB test**: create epic, patch `auto_dispatch = false`, read back and assert value persisted.
3. **MCP handler test** (`src/mcp/handlers/tests.rs`): call `dispatch_next` on an epic with `auto_dispatch = false`; assert response contains "disabled" message and no task status change.
4. **TUI message test** (`src/tui/tests.rs`): send `Message::ToggleEpicAutoDispatch` and assert `EpicPatch` with correct value is returned as a `Command`.

### Manual verification

1. Create an epic with subtasks and a plan.
2. Press `d` to dispatch first subtask — agent starts.
3. In the epic view, confirm header shows `"auto dispatch [U]"` in green.
4. Press `U` — header switches to `"manual dispatch [U]"` in muted.
5. Let the running agent complete and call `dispatch_next` — assert no new subtask is dispatched (agent receives "auto dispatch is disabled" message).
6. Press `d` on the epic manually — assert next subtask dispatches.
7. Press `U` again — header switches back to `"auto dispatch [U]"` in green.
