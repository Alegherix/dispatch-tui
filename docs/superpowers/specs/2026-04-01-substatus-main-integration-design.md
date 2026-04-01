# SubStatus Integration Design — Main Branch Reconciliation

## Overview

This spec cements the sub_status implementation after rebasing onto main. It documents:

1. **Invariant enforcement** — strong typing so (status, sub_status) is always valid
2. **HookNotification fix** — align the hook script with the sub_status model
3. **MCP format fix** — remove duplicate sub_status line
4. **Allium spec restoration** — reinstate main-branch features lost during conflict resolution

The sub_status core (enum, VisualColumn, migration 15, UI, MCP, AgentTracking simplification) is
correctly implemented per the original spec and is not changed here.

---

## Background: Main Branch Context

Main added these features before the rebase:

| Feature | Files |
|---|---|
| `tag` field on Task (bug/feature/chore/epic) | `models.rs`, `db.rs`, `mcp/handlers/tasks.rs` |
| Tag-based dispatch routing in epics | `tui/input.rs`, `dispatch.rs` |
| `InputTag` creation flow step | `tui/types.rs`, `tui/mod.rs`, `tui/input.rs` |
| `pr_url` in MCP `update_task` | `mcp/handlers/tasks.rs`, `mcp/handlers/dispatch.rs` |
| `TaskUsage` + `report_usage` MCP tool | `models.rs`, `db.rs`, `mcp/handlers/tasks.rs` |
| `review_prs` table + Review Board | `models.rs`, `db.rs`, `tui/`, `github.rs` |
| `DEFAULT_PORT` constant | `lib.rs` |

All of these are preserved on this branch. This spec does not change any of them.

---

## Section 1: (status, sub_status) Invariant Enforcement

### The Invariant

Every task row must satisfy `sub_status.is_valid_for(status) == true`. Currently this is not
enforced at the API boundary: callers can change `status` via `TaskPatch` or `update_status_if`
without touching `sub_status`, leaving stale invalid values in the DB.

This causes the primary runtime bug: when the `HookStop` fires
(`dispatch update <id> review --only-if running`), `update_status_if` changes `status` to
`review` but leaves `sub_status` as `active`. The task is now `(review, active)`, which matches no
visual column, making it invisible on the board.

### Fix A — `TaskPatch::status()` auto-resets sub_status

Change the `status()` builder method so it always also sets
`sub_status = SubStatus::default_for(new_status)`:

```rust
pub fn status(mut self, status: TaskStatus) -> Self {
    self.sub_status = Some(SubStatus::default_for(status));  // auto-reset
    self.status = Some(status);
    self
}
```

Callers that need a non-default sub_status chain `.sub_status()` afterward — the last write wins
in the generated SQL. Example:

```rust
// Set to review with default (awaiting_review):
TaskPatch::new().status(TaskStatus::Review)

// Set to review with a specific sub_status:
TaskPatch::new().status(TaskStatus::Review).sub_status(SubStatus::Approved)
```

### Fix B — `update_status_if` also resets sub_status

`update_status_if` is used by the CLI hook path and must also reset sub_status. Update the SQL:

```sql
UPDATE tasks
SET    status     = ?1,
       sub_status = ?4,
       updated_at = datetime('now')
WHERE  id = ?2 AND status = ?3
```

Where `?4 = SubStatus::default_for(new_status).as_str()`. The function signature gains a
`sub_status: SubStatus` parameter (or computes it internally as `SubStatus::default_for(new_status)`
if the caller always wants the default).

Since every caller of `update_status_if` only passes `new_status` (no explicit sub_status), the
function can compute the default internally — no signature change needed.

### Fix C — debug_assert in `patch_task`

When both `status` and `sub_status` are present in the same patch, assert compatibility at
development time:

```rust
if let (Some(s), Some(ss)) = (patch.status, patch.sub_status) {
    debug_assert!(
        ss.is_valid_for(s),
        "invalid (status, sub_status) pair: {:?}/{:?}", s, ss
    );
}
```

### Fix D — SQL CHECK constraint (migration 16)

Belt-and-suspenders enforcement at the DB layer. Migration 16:

1. Clean up any pre-existing invalid rows:
   ```sql
   -- Legacy (review, needs_input) from old hook behavior → awaiting_review
   UPDATE tasks SET sub_status = 'awaiting_review'
   WHERE status = 'review' AND sub_status = 'needs_input';

   -- Any other invalid pair → reset to default
   UPDATE tasks SET sub_status = 'none'
   WHERE status IN ('backlog','done','archived') AND sub_status != 'none';

   UPDATE tasks SET sub_status = 'active'
   WHERE status = 'running'
     AND sub_status NOT IN ('active','needs_input','stale','crashed');

   UPDATE tasks SET sub_status = 'awaiting_review'
   WHERE status = 'review'
     AND sub_status NOT IN ('awaiting_review','changes_requested','approved');
   ```

2. Rebuild the table with a CHECK constraint:
   ```sql
   CREATE TABLE tasks_new (
       ...
       sub_status TEXT NOT NULL DEFAULT 'none',
       ...
       CHECK (
           (status = 'backlog'  AND sub_status = 'none') OR
           (status = 'running'  AND sub_status IN ('active','needs_input','stale','crashed')) OR
           (status = 'review'   AND sub_status IN ('awaiting_review','changes_requested','approved')) OR
           (status = 'done'     AND sub_status = 'none') OR
           (status = 'archived' AND sub_status = 'none')
       )
   );
   ```

3. Bump schema version to 16.

---

## Section 2: HookNotification Alignment

### The Problem

The hook script (`task-status-hook`) currently handles Notification as:

```bash
Notification) dispatch update "$ID" review --only-if running --needs-input ;;
```

With sub_status, this call:
1. Calls `update_status_if(review, running)` → task moves to `(review, awaiting_review)`
   (after Fix B above)
2. Calls `patch_task(sub_status=needs_input)` → task becomes `(review, needs_input)`

But `(review, needs_input)` is now invalid per the CHECK constraint and matches no visual column.

### The Fix

Change the Notification handler to keep the task in Running and set sub_status:

```bash
Notification) dispatch update "$ID" running --only-if running --sub-status needs_input ;;
```

Semantic: the agent is still running (it has not stopped) — it just needs human input. The task
becomes `(running, needs_input)` and appears in the **Blocked** visual column.

When the agent resumes, `PreToolUse` fires:
```bash
PreToolUse) dispatch update "$ID" running ;;
```
`update_status_if` resets sub_status to `active` (Fix B), so the task returns to
`(running, active)` — the **Active** column.

### `setup.rs` Update

The `run_setup` function generates the hook command strings. Update the Notification entry to
match the new command.

### `--needs-input` Flag Status

The `--needs-input` CLI flag is already marked deprecated in the code. It maps to
`--sub-status needs_input`. It can remain for backward compatibility but should not be used in
the generated hook template.

---

## Section 3: MCP `format_task_detail` Duplicate

The `format_task_detail` function in `src/mcp/handlers/tasks.rs` has `Sub-status:` added twice.
The first occurrence (before the `epic_id` block) is the correct position. The second occurrence
(which replaced the old `needs_input` block) is a duplicate and must be removed.

---

## Section 4: Allium Spec Restoration

The following were present in `docs/specs/dispatch.allium` on `main` but were dropped during
rebase conflict resolution. All must be restored.

### Task entity fields

Restore to the `entity Task` block:
- `tag: String?` field
- `pr_number: derived from pr_url` derived attribute
- `is_epic: tag = "epic"` derived attribute

### TaskUsage entity

Restore the full `entity TaskUsage` block:
```
entity TaskUsage {
    task: Task
    cost_usd: Float
    input_tokens: Integer
    output_tokens: Integer
    cache_read_tokens: Integer
    cache_write_tokens: Integer
    updated_at: Timestamp
    @guidance
        -- Accumulation semantics: one row per task, upserted on each
        -- report. Numeric fields are summed with existing values.
}
```

### UpdateTaskViaMcp rule

Restore `pr_url?` and `tag?` parameters:
```
when: McpUpdateTask(task, status?, sub_status?, plan?, title?, description?,
                    repo_path?, sort_order?, pr_url?, tag?)
```
And their `ensures` clauses:
```
ensures: if pr_url != null: task.pr_url = pr_url
ensures: if tag != null: task.tag = tag
```

### ReportUsageViaMcp rule

Restore the full `rule ReportUsageViaMcp` block.

### DispatchEpicSubtask rule — tag-based routing

Restore the routing logic based on `tag`:
```
if next.has_plan:
    AgentLaunched(task: next, mode: implementation)
else if next.tag = "epic":
    AgentLaunched(task: next, mode: brainstorm)
else if next.tag = "feature":
    AgentLaunched(task: next, mode: plan)
else:
    AgentLaunched(task: next, mode: standard)
```

### HookNotification rule update

Update to reflect the new behavior (task stays Running):
```
rule HookNotification {
    when: HookFired(hook: notification, task: task)
    requires: task.status = running

    ensures: task.sub_status = needs_input
    -- Note: status remains running. The agent has not stopped; it needs
    -- human input. The task appears in the Blocked visual column.
}
```

### ClearNeedsInput rule update

The clear now happens via PreToolUse resetting status→running (which auto-resets sub_status):
```
rule ClearNeedsInput {
    when: HookFired(hook: pre_tool_use, task: task)
    requires: task.sub_status = needs_input

    ensures: task.sub_status = SubStatus.default_for(running)  -- i.e. active
    -- Cleared by the auto-reset in update_status_if when status is
    -- set to running by HookPreToolUse.
}
```

---

## Status Transition Summary (updated)

```
Backlog/None ──d──> Running/Active
Running/Active ──(tick: no output)──> Running/Stale
Running/Active ──(tick: window gone)──> Running/Crashed
Running/* ──(new tmux output)──> Running/Active  (recovery)
Running/Active ──(MCP: sub_status=needs_input)──> Running/Blocked
Running/Active ──(hook: notification)──> Running/Blocked
Running/Blocked ──(hook: pre_tool_use)──> Running/Active
Running/Blocked ──W(pr)──> Review/PR Created
Running/Blocked ──W(rebase)──> Done/None
Running/* ──(hook: stop)──> Review/PR Created
Review/PR Created ──(poll: changes_requested)──> Review/Revise
Review/PR Created ──(poll: approved)──> Review/Approved
Review/Approved ──(poll: merged)──> Done/None (auto)
Review/* ──W(rebase)──> Done/None
```

---

## Implementation Checklist

1. `src/db.rs` — `TaskPatch::status()` auto-resets sub_status
2. `src/db.rs` — `update_status_if` SQL includes sub_status reset
3. `src/db.rs` — `debug_assert` in `patch_task` for (status, sub_status) compatibility
4. `src/db.rs` — Migration 16: clean invalid rows + add CHECK constraint
5. `src/setup.rs` — Update Notification hook command to `running --sub-status needs_input`
6. `src/mcp/handlers/tasks.rs` — Remove duplicate `Sub-status:` line in `format_task_detail`
7. `docs/specs/dispatch.allium` — Restore tag, TaskUsage, pr_url, tag-routing, HookNotification
8. Tests for each change (TaskPatch auto-sync, update_status_if, migration 16 cleanup)
