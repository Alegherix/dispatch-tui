# SubStatus Main-Branch Integration — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix three bugs where (status, sub_status) can desync, realign the HookNotification hook with the sub_status model, and restore Allium spec sections lost during rebase.

**Architecture:** Invariant enforcement lives at two levels — the `TaskPatch::status()` builder auto-resets sub_status (so all code paths naturally carry valid pairs), and `update_status_if` is updated in parallel SQL to do the same. Migration 16 adds a DB-level CHECK constraint as belt-and-suspenders. The hook script change is a one-liner in `hooks/task-status-hook`.

**Tech Stack:** Rust, SQLite/rusqlite, Bash (hook script). Tests use in-memory SQLite via `Database::open_in_memory()` or raw `Connection::open_in_memory()` + `Database::init_schema()`.

---

## Files changed

| File | Change |
|---|---|
| `src/db.rs` | `TaskPatch::status()` auto-reset, `patch_task` assert, `update_status_if` SQL, migration 16 |
| `hooks/task-status-hook` | Notification line changed to `running --sub-status needs_input` |
| `src/mcp/handlers/tasks.rs` | Remove duplicate `Sub-status:` line in `format_task_detail` |
| `docs/specs/dispatch.allium` | Restore tag, TaskUsage, pr_url, tag-routing, HookNotification rules |

---

## Task 1: `TaskPatch::status()` auto-resets sub_status

**Files:**
- Modify: `src/db.rs` (lines 37-40 and 650-717)

### Step 1.1: Write failing tests

Add these tests to the `#[cfg(test)] mod tests` block at the bottom of `src/db.rs`:

```rust
#[test]
fn task_patch_status_auto_resets_sub_status() {
    // Calling .status() without .sub_status() should still produce a valid pair
    let patch = TaskPatch::new().status(TaskStatus::Review);
    assert_eq!(patch.status, Some(TaskStatus::Review));
    assert_eq!(patch.sub_status, Some(SubStatus::AwaitingReview));
}

#[test]
fn task_patch_status_running_defaults_to_active() {
    let patch = TaskPatch::new().status(TaskStatus::Running);
    assert_eq!(patch.sub_status, Some(SubStatus::Active));
}

#[test]
fn task_patch_status_sub_status_can_override_default() {
    // Explicitly chaining .sub_status() after .status() overrides the auto-reset
    let patch = TaskPatch::new()
        .status(TaskStatus::Review)
        .sub_status(SubStatus::Approved);
    assert_eq!(patch.status, Some(TaskStatus::Review));
    assert_eq!(patch.sub_status, Some(SubStatus::Approved));
}

#[test]
fn patch_task_status_change_resets_sub_status_in_db() {
    // End-to-end: after a status-only patch, sub_status in DB reflects the new default
    let db = Database::open_in_memory().unwrap();
    let id = db.create_task("T", "d", "/r", None, TaskStatus::Running).unwrap();
    db.patch_task(id, &TaskPatch::default().sub_status(SubStatus::Stale)).unwrap();

    db.patch_task(id, &TaskPatch::default().status(TaskStatus::Review)).unwrap();

    let task = db.get_task(id).unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Review);
    assert_eq!(task.sub_status, SubStatus::AwaitingReview);
}
```

- [ ] Add the four tests above to `src/db.rs` `mod tests`

### Step 1.2: Run tests to confirm they fail

```bash
cargo test task_patch_status task_patch_status_running patch_task_status_change -- --nocapture 2>&1 | tail -20
```

Expected: compilation error or FAILED (the auto-reset logic doesn't exist yet).

- [ ] Run tests and observe failure

### Step 1.3: Implement — `TaskPatch::status()` auto-reset

Replace `src/db.rs` lines 37-40:

```rust
// Before:
pub fn status(mut self, status: TaskStatus) -> Self {
    self.status = Some(status);
    self
}
```

With:

```rust
pub fn status(mut self, status: TaskStatus) -> Self {
    self.sub_status = Some(SubStatus::default_for(status));  // auto-reset: valid pair guaranteed
    self.status = Some(status);
    self
}
```

- [ ] Make the edit

### Step 1.4: Add debug_assert in `patch_task`

In `src/db.rs`, find `fn patch_task` (around line 650). Insert the assert **after** the `if !patch.has_changes()` guard:

```rust
fn patch_task(&self, id: TaskId, patch: &TaskPatch<'_>) -> Result<()> {
    if !patch.has_changes() {
        return Ok(());
    }
    debug_assert!(
        !matches!((patch.status, patch.sub_status), (Some(s), Some(ss)) if !ss.is_valid_for(s)),
        "invalid (status, sub_status) pair in patch: {:?}/{:?}",
        patch.status, patch.sub_status
    );
    let conn = self.conn()?;
    // ... rest unchanged
```

- [ ] Make the edit

### Step 1.5: Run tests — all four must pass

```bash
cargo test task_patch_status task_patch_status_running patch_task_status_change -- --nocapture 2>&1 | tail -20
```

Expected: `4 passed`

- [ ] Run tests and confirm all pass

### Step 1.6: Run full test suite

```bash
cargo test 2>&1 | tail -30
```

Expected: all tests pass. Fix any regressions before proceeding.

- [ ] Full test suite passes

### Step 1.7: Commit

```bash
git add src/db.rs
git commit -m "fix: TaskPatch::status() auto-resets sub_status to default

Calling .status(new_status) now also sets sub_status = default_for(new_status).
This guarantees valid (status, sub_status) pairs through all code paths that
use TaskPatch to change status. Callers that need a non-default sub_status
chain .sub_status() afterward (last write wins in generated SQL).

Also adds debug_assert in patch_task to catch invalid pairs during development."
```

- [ ] Commit

---

## Task 2: Fix remaining DB write paths that omit sub_status

**Files:**
- Modify: `src/db.rs` (lines ~530-540 for `create_task`, lines ~580-589 for `update_status_if`)

Both `create_task` and `update_status_if` write to the DB without setting `sub_status`. This
must be fixed before migration 16 adds a CHECK constraint, because the constraint would reject
any rows with mismatched (status, sub_status) — including rows created by these functions.

### Step 2.1: Write failing tests

Add to `mod tests` in `src/db.rs`:

```rust
#[test]
fn create_task_sets_default_sub_status_for_running() {
    // create_task with status=Running must produce sub_status=active, not 'none'
    let db = Database::open_in_memory().unwrap();
    let id = db.create_task("T", "d", "/r", None, TaskStatus::Running).unwrap();
    let task = db.get_task(id).unwrap().unwrap();
    assert_eq!(task.sub_status, SubStatus::Active);
}

#[test]
fn create_task_sets_default_sub_status_for_backlog() {
    let db = Database::open_in_memory().unwrap();
    let id = db.create_task("T", "d", "/r", None, TaskStatus::Backlog).unwrap();
    let task = db.get_task(id).unwrap().unwrap();
    assert_eq!(task.sub_status, SubStatus::None);
}

#[test]
fn update_status_if_resets_sub_status_to_default() {
    let db = Database::open_in_memory().unwrap();
    let id = db.create_task("T", "d", "/r", None, TaskStatus::Running).unwrap();
    db.patch_task(id, &TaskPatch::default().sub_status(SubStatus::Stale)).unwrap();

    let updated = db.update_status_if(id, TaskStatus::Review, TaskStatus::Running).unwrap();
    assert!(updated, "should have updated");

    let task = db.get_task(id).unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Review);
    assert_eq!(task.sub_status, SubStatus::AwaitingReview); // default for review
}

#[test]
fn update_status_if_leaves_sub_status_unchanged_when_condition_fails() {
    let db = Database::open_in_memory().unwrap();
    let id = db.create_task("T", "d", "/r", None, TaskStatus::Running).unwrap();
    db.patch_task(id, &TaskPatch::default().sub_status(SubStatus::Active)).unwrap();

    let updated = db.update_status_if(id, TaskStatus::Review, TaskStatus::Backlog).unwrap();
    assert!(!updated, "condition was wrong, should not have updated");

    let task = db.get_task(id).unwrap().unwrap();
    assert_eq!(task.sub_status, SubStatus::Active); // unchanged
}
```

- [ ] Add the four tests above to `src/db.rs` `mod tests`

### Step 2.2: Run tests to confirm they fail

```bash
cargo test create_task_sets_default_sub_status update_status_if_resets update_status_if_leaves -- --nocapture 2>&1 | tail -10
```

Expected: FAILED (sub_status is 'none' after create, and stale/active after update).

- [ ] Run tests and observe failure

### Step 2.3: Implement — fix `create_task` INSERT

Find `fn create_task` in the `impl TaskStore for Database` block (look for the INSERT INTO tasks). Replace the INSERT to include `sub_status`:

```rust
// Before (approximate — find the exact INSERT in fn create_task):
conn.execute(
    "INSERT INTO tasks (title, description, repo_path, plan, status) VALUES (?1, ?2, ?3, ?4, ?5)",
    params![title, description, repo_path, plan, status.as_str()],
)
```

```rust
// After:
let sub_status = SubStatus::default_for(status);
conn.execute(
    "INSERT INTO tasks (title, description, repo_path, plan, status, sub_status) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    params![title, description, repo_path, plan, status.as_str(), sub_status.as_str()],
)
```

- [ ] Make the edit to `create_task`

### Step 2.4: Implement — fix `update_status_if` SQL

Replace `src/db.rs` `fn update_status_if` (around line 580-589):

```rust
// Before:
fn update_status_if(&self, id: TaskId, new_status: TaskStatus, expected: TaskStatus) -> Result<bool> {
    let conn = self.conn()?;
    let rows = conn
        .execute(
            "UPDATE tasks SET status = ?1, updated_at = datetime('now') WHERE id = ?2 AND status = ?3",
            params![new_status.as_str(), id.0, expected.as_str()],
        )
        .context("Failed to conditional-update status")?;
    Ok(rows > 0)
}
```

```rust
// After:
fn update_status_if(&self, id: TaskId, new_status: TaskStatus, expected: TaskStatus) -> Result<bool> {
    let default_sub = SubStatus::default_for(new_status);
    let conn = self.conn()?;
    let rows = conn
        .execute(
            "UPDATE tasks SET status = ?1, sub_status = ?4, updated_at = datetime('now') \
             WHERE id = ?2 AND status = ?3",
            params![new_status.as_str(), id.0, expected.as_str(), default_sub.as_str()],
        )
        .context("Failed to conditional-update status")?;
    Ok(rows > 0)
}
```

- [ ] Make the edit to `update_status_if`

### Step 2.5: Run tests — all four must pass

```bash
cargo test create_task_sets_default_sub_status update_status_if_resets update_status_if_leaves -- --nocapture 2>&1 | tail -10
```

Expected: `4 passed`

- [ ] Run tests

### Step 2.6: Run full test suite

```bash
cargo test 2>&1 | tail -30
```

- [ ] Full test suite passes

### Step 2.7: Commit

```bash
git add src/db.rs
git commit -m "fix: create_task and update_status_if set correct sub_status

create_task now inserts sub_status = default_for(status) instead of
relying on the column DEFAULT 'none', which was invalid for non-backlog
statuses. update_status_if similarly resets sub_status when status changes.

Both fixes are prerequisites for the migration 16 CHECK constraint."
```

- [ ] Commit

---

## Task 3: Migration 16 — cleanup + CHECK constraint

**Files:**
- Modify: `src/db.rs` (inside `fn init_schema` or `fn migrate`, after the v15 block)

### Step 3.1: Write failing tests

Add to `mod tests` in `src/db.rs`:

```rust
#[test]
fn schema_version_is_16() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.conn.lock().unwrap();
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
    assert_eq!(version, 16, "fresh DB should be at schema version 16");
}

#[test]
fn check_constraint_rejects_review_with_active_substatus() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.conn.lock().unwrap();
    conn.execute(
        "INSERT INTO tasks (title, description, repo_path, status, sub_status) \
         VALUES ('T', 'D', '/r', 'backlog', 'none')",
        [],
    ).unwrap();
    let result = conn.execute(
        "UPDATE tasks SET status = 'review', sub_status = 'active' WHERE id = 1",
        [],
    );
    assert!(result.is_err(), "CHECK constraint must reject (review, active)");
}

#[test]
fn check_constraint_accepts_review_with_awaiting_review() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.conn.lock().unwrap();
    conn.execute(
        "INSERT INTO tasks (title, description, repo_path, status, sub_status) \
         VALUES ('T', 'D', '/r', 'backlog', 'none')",
        [],
    ).unwrap();
    let result = conn.execute(
        "UPDATE tasks SET status = 'review', sub_status = 'awaiting_review' WHERE id = 1",
        [],
    );
    assert!(result.is_ok(), "valid pair should be accepted");
}

#[test]
fn migration_16_cleans_invalid_review_needs_input() {
    // Simulate a v15 DB that has (review, needs_input) rows from old hook behavior
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "PRAGMA foreign_keys=ON;
         CREATE TABLE tasks (
             id INTEGER PRIMARY KEY,
             title TEXT NOT NULL,
             description TEXT NOT NULL,
             repo_path TEXT NOT NULL,
             status TEXT NOT NULL DEFAULT 'backlog',
             worktree TEXT,
             tmux_window TEXT,
             plan TEXT,
             epic_id INTEGER,
             sub_status TEXT NOT NULL DEFAULT 'none',
             pr_url TEXT,
             tag TEXT,
             sort_order INTEGER,
             created_at TEXT NOT NULL DEFAULT (datetime('now')),
             updated_at TEXT NOT NULL DEFAULT (datetime('now'))
         );
         CREATE TABLE repo_paths (
             id INTEGER PRIMARY KEY,
             path TEXT NOT NULL UNIQUE,
             last_used TEXT NOT NULL DEFAULT (datetime('now'))
         );
         CREATE TABLE epics (
             id INTEGER PRIMARY KEY,
             title TEXT NOT NULL,
             description TEXT NOT NULL,
             repo_path TEXT NOT NULL,
             done INTEGER NOT NULL DEFAULT 0,
             plan TEXT,
             sort_order INTEGER,
             created_at TEXT NOT NULL DEFAULT (datetime('now')),
             updated_at TEXT NOT NULL DEFAULT (datetime('now'))
         );
         CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE task_usage (
             task_id INTEGER PRIMARY KEY REFERENCES tasks(id) ON DELETE CASCADE,
             cost_usd REAL NOT NULL DEFAULT 0.0,
             input_tokens INTEGER NOT NULL DEFAULT 0,
             output_tokens INTEGER NOT NULL DEFAULT 0,
             cache_read_tokens INTEGER NOT NULL DEFAULT 0,
             cache_write_tokens INTEGER NOT NULL DEFAULT 0,
             updated_at TEXT NOT NULL DEFAULT (datetime('now'))
         );
         CREATE TABLE filter_presets (name TEXT PRIMARY KEY, repo_paths TEXT NOT NULL);
         CREATE TABLE review_prs (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             number INTEGER NOT NULL,
             title TEXT NOT NULL,
             url TEXT NOT NULL,
             repo TEXT NOT NULL,
             author TEXT NOT NULL,
             state TEXT NOT NULL DEFAULT 'open',
             review_decision TEXT,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL
         );
         PRAGMA user_version = 15;",
    ).unwrap();

    // Insert invalid rows that migration 16 must clean up
    conn.execute(
        "INSERT INTO tasks (title, description, repo_path, status, sub_status) \
         VALUES ('ReviewBlocked', 'desc', '/r', 'review', 'needs_input')",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO tasks (title, description, repo_path, status, sub_status) \
         VALUES ('ValidReview', 'desc', '/r', 'review', 'awaiting_review')",
        [],
    ).unwrap();

    // Run migrations
    Database::init_schema(&conn).unwrap();

    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0)).unwrap();
    assert_eq!(version, 16);

    // (review, needs_input) must be converted to (review, awaiting_review)
    let ss: String = conn.query_row(
        "SELECT sub_status FROM tasks WHERE title = 'ReviewBlocked'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(ss, "awaiting_review", "legacy (review, needs_input) must be cleaned up");

    // Valid row must be unchanged
    let ss2: String = conn.query_row(
        "SELECT sub_status FROM tasks WHERE title = 'ValidReview'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(ss2, "awaiting_review");
}
```

- [ ] Add the four tests above to `mod tests` in `src/db.rs`

### Step 3.2: Run tests to confirm they fail

```bash
cargo test schema_version_is_16 check_constraint migration_16_cleans -- --nocapture 2>&1 | tail -20
```

Expected: FAILED (`schema_version_is_16` gets 15, constraint tests may fail differently).

- [ ] Run tests and observe failure

### Step 3.3: Implement — migration 16 block

In `src/db.rs`, inside the `fn init_schema` function (or wherever the versioned migration blocks live — look for `if current_version < 15 {`), add the following block immediately after the v15 block:

```rust
if current_version < 16 {
    // Migration 16: clean up invalid (status, sub_status) pairs and add CHECK constraint.
    //
    // Before this migration, (review, needs_input) rows could exist from old hook behavior.
    // Clean them up first so the CHECK constraint can be added without constraint violations.
    let _ = conn.execute_batch(
        "-- Legacy (review, needs_input) from old HookNotification hook → awaiting_review
         UPDATE tasks SET sub_status = 'awaiting_review'
         WHERE status = 'review' AND sub_status = 'needs_input';

         -- Any other invalid running pairs → active
         UPDATE tasks SET sub_status = 'active'
         WHERE status = 'running'
           AND sub_status NOT IN ('active', 'needs_input', 'stale', 'crashed');

         -- Any other invalid review pairs → awaiting_review
         UPDATE tasks SET sub_status = 'awaiting_review'
         WHERE status = 'review'
           AND sub_status NOT IN ('awaiting_review', 'changes_requested', 'approved');

         -- Any other invalid terminal-status pairs → none
         UPDATE tasks SET sub_status = 'none'
         WHERE status IN ('backlog', 'done', 'archived') AND sub_status != 'none';"
    );

    conn.execute_batch(
        "CREATE TABLE tasks_new (
            id          INTEGER PRIMARY KEY,
            title       TEXT NOT NULL,
            description TEXT NOT NULL,
            repo_path   TEXT NOT NULL,
            status      TEXT NOT NULL DEFAULT 'backlog',
            worktree    TEXT,
            tmux_window TEXT,
            plan        TEXT,
            epic_id     INTEGER REFERENCES epics(id),
            sub_status  TEXT NOT NULL DEFAULT 'none',
            pr_url      TEXT,
            tag         TEXT,
            sort_order  INTEGER,
            created_at  TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at  TEXT NOT NULL DEFAULT (datetime('now')),
            CHECK (
                (status = 'backlog'  AND sub_status = 'none') OR
                (status = 'running'  AND sub_status IN ('active','needs_input','stale','crashed')) OR
                (status = 'review'   AND sub_status IN ('awaiting_review','changes_requested','approved')) OR
                (status = 'done'     AND sub_status = 'none') OR
                (status = 'archived' AND sub_status = 'none')
            )
        );
        INSERT INTO tasks_new
            SELECT id, title, description, repo_path, status, worktree, tmux_window, plan,
                   epic_id, sub_status, pr_url, tag, sort_order, created_at, updated_at
            FROM tasks;
        DROP TABLE tasks;
        ALTER TABLE tasks_new RENAME TO tasks;"
    ).context("Failed to rebuild tasks table with CHECK constraint for migration 16")?;

    conn.pragma_update(None, "user_version", 16i64)
        .context("Failed to update schema version to 16")?;
}
```

Also update all existing schema version assertions in `src/db.rs`. Grep for them:

```bash
grep -n "version, 15" src/db.rs
```

You will find assertions in at least:
- `schema_version_is_latest` test (or similarly named) — change `15` to `16`
- `migration_13_converts_needs_input` test — the migration now runs through v16, so change `assert_eq!(version, 15)` to `assert_eq!(version, 16)`
- Any other test that asserts a schema version of 15

Change all `assert_eq!(version, 15...)` → `assert_eq!(version, 16...)` in the tests block.

- [ ] Add the migration 16 block to `src/db.rs`
- [ ] Update existing schema version assertions from 15 to 16

### Step 3.4: Run tests — all four must pass

```bash
cargo test schema_version_is_16 check_constraint migration_16_cleans -- --nocapture 2>&1 | tail -20
```

Expected: `4 passed`

- [ ] Run tests

### Step 3.5: Run full test suite

```bash
cargo test 2>&1 | tail -30
```

Fix any regressions. In particular, the existing `schema_version_is_latest` test may have been checking for 15 — update it to 16 if you didn't already.

- [ ] Full test suite passes

### Step 3.6: Commit

```bash
git add src/db.rs
git commit -m "feat: migration 16 — CHECK constraint on (status, sub_status)

Cleans up any legacy invalid pairs (e.g. review/needs_input from old hook
behavior) and rebuilds the tasks table with a CHECK constraint that enforces
SubStatus.is_valid_for(status) at the DB layer."
```

- [ ] Commit

---

## Task 4: HookNotification fix

**Files:**
- Modify: `hooks/task-status-hook` (line 27)

The hook is embedded into the binary via `include_str!` in `src/setup.rs`. Changing the file on disk is sufficient — `dispatch setup` re-installs the updated script.

### Step 4.1: Write a failing test

The existing test `hook_script_skips_dispatch_mcp_in_pretooluse` checks for `running` in PreToolUse. Add a test that asserts the Notification line uses the new command. In `src/setup.rs` `mod tests`:

```rust
#[test]
fn hook_script_notification_uses_sub_status_needs_input() {
    // Notification must NOT change status to review — it keeps running and
    // sets sub_status=needs_input so the task stays in the Blocked visual column.
    assert!(
        HOOK_SCRIPT.contains("--sub-status needs_input"),
        "Notification handler must use --sub-status needs_input, not --needs-input"
    );
    assert!(
        !HOOK_SCRIPT.contains("--needs-input"),
        "Deprecated --needs-input flag must not appear in the hook script"
    );
}
```

- [ ] Add the test to `src/setup.rs` `mod tests`

### Step 4.2: Run test to confirm it fails

```bash
cargo test hook_script_notification -- --nocapture 2>&1 | tail -10
```

Expected: FAILED (`--sub-status needs_input` not found, `--needs-input` still present).

- [ ] Run test and observe failure

### Step 4.3: Implement — update the hook script

Edit `hooks/task-status-hook` line 27. Change:

```bash
# Before:
Notification) dispatch update "$ID" review  --only-if running --needs-input ;;
```

To:

```bash
# After:
Notification) dispatch update "$ID" running --only-if running --sub-status needs_input ;;
```

The full updated `case` block will be:

```bash
case "$EVENT" in
    PreToolUse)
        TOOL=$(echo "$INPUT" | jq -r '.tool_name // empty')
        # Skip dispatch MCP calls — management operations, not implementation work.
        # Without this, the wrap-up skill's get_task and wrap_up calls clobber review status.
        [[ "$TOOL" == mcp__dispatch__* ]] && exit 0
        dispatch update "$ID" running
        ;;
    Stop)         dispatch update "$ID" review  --only-if running ;;
    Notification) dispatch update "$ID" running --only-if running --sub-status needs_input ;;
    *)            exit 0 ;;
esac
```

- [ ] Make the edit to `hooks/task-status-hook`

### Step 4.4: Run tests — must pass

```bash
cargo test hook_script -- --nocapture 2>&1 | tail -20
```

Expected: all `hook_script_*` tests pass including the new one.

- [ ] Run tests

### Step 4.5: Run full test suite

```bash
cargo test 2>&1 | tail -30
```

- [ ] Full test suite passes

### Step 4.6: Commit

```bash
git add hooks/task-status-hook src/setup.rs
git commit -m "fix: HookNotification keeps task Running, sets sub_status=needs_input

Previously the Notification hook moved tasks to review status, producing
(review, needs_input) which is invalid and invisible on the board. Now it
keeps status=running and sets sub_status=needs_input, placing the task in
the Blocked visual column where it belongs.

When the agent resumes (PreToolUse fires), update_status_if resets sub_status
back to active automatically."
```

- [ ] Commit

---

## Task 5: Remove duplicate sub_status line in MCP format_task_detail

**Files:**
- Modify: `src/mcp/handlers/tasks.rs` (around line 126)

### Step 5.1: Locate and remove the duplicate

In `src/mcp/handlers/tasks.rs`, find `fn format_task_detail`. There are two identical lines:

```rust
text.push_str(&format!("\nSub-status: {}", task.sub_status.as_str()));
```

The first occurrence (around line 107, before the `if let Some(ref epic_id)` block) is the correct position. The second occurrence (around line 126, where the old `if task.needs_input { ... }` block used to be) is the duplicate.

Remove the second `text.push_str(&format!("\nSub-status: {}", task.sub_status.as_str()));` line.

After the edit, `format_task_detail` should have exactly one `Sub-status:` line.

- [ ] Remove the duplicate line

### Step 5.2: Verify with cargo test

```bash
cargo test mcp -- --nocapture 2>&1 | tail -20
```

Expected: all MCP tests pass.

- [ ] Run tests

### Step 5.3: Commit

```bash
git add src/mcp/handlers/tasks.rs
git commit -m "fix: remove duplicate Sub-status line in MCP format_task_detail"
```

- [ ] Commit

---

## Task 6: Allium spec restoration

**Files:**
- Modify: `docs/specs/dispatch.allium`

Use the `allium:tend` skill to restore sections that were lost during rebase conflict resolution. The following is the list of changes needed and their exact content.

### Step 6.1: Restore `tag` and derived fields to `entity Task`

In `entity Task`, restore these fields (they appear after `pr_url`):

```
entity Task {
    ...
    pr_url: String?
    tag: String?       -- ← ADD (was on main, lost in rebase)
    sort_order: Integer?
    ...

    -- Derived
    has_plan: plan != null
    is_stale: sub_status == stale
    is_crashed: sub_status == crashed
    is_epic: tag = "epic"     -- ← ADD
    pr_number: derived from pr_url     -- ← ADD (extracted at runtime from URL, not persisted)
}
```

- [ ] Add `tag: String?` after `pr_url` in entity Task
- [ ] Add `is_epic: tag = "epic"` and `pr_number: derived from pr_url` to the derived section

### Step 6.2: Restore `entity TaskUsage`

Add the following block after `entity Epic`:

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
        -- Uses accumulation semantics: one row per task, upserted on each
        -- report. Numeric fields (cost_usd, input_tokens, output_tokens,
        -- cache_read_tokens, cache_write_tokens) are summed with the
        -- existing values rather than replaced. updated_at reflects the
        -- time of the most recent report.
}
```

- [ ] Add `entity TaskUsage` block

### Step 6.3: Restore `pr_url` and `tag` in `rule UpdateTaskViaMcp`

Find the `rule UpdateTaskViaMcp` rule. The `when` clause and `ensures` currently read:

```
when: McpUpdateTask(task, status?, sub_status?, plan?, title?, description?,
                    repo_path?, sort_order?)
```

Restore `pr_url?` and `tag?`:

```
when: McpUpdateTask(task, status?, sub_status?, plan?, title?, description?,
                    repo_path?, sort_order?, pr_url?, tag?)
```

Also restore the missing `ensures` clauses (add after the sort_order ensure):

```
    ensures:
        if pr_url != null: task.pr_url = pr_url
    ensures:
        if tag != null: task.tag = tag
```

- [ ] Update `when` clause in UpdateTaskViaMcp
- [ ] Add `pr_url` and `tag` ensures clauses

### Step 6.4: Restore `rule ReportUsageViaMcp`

Add this rule after `rule UpdateEpicViaMcp`:

```
rule ReportUsageViaMcp {
    when: McpReportUsage(task, cost_usd, input_tokens, output_tokens, cache_read_tokens?, cache_write_tokens?)

    ensures: TaskUsage.accumulate(
        task: task,
        cost_usd: cost_usd,
        input_tokens: input_tokens,
        output_tokens: output_tokens,
        cache_read_tokens: cache_read_tokens ?? 0,
        cache_write_tokens: cache_write_tokens ?? 0,
        updated_at: now
    )

    @guidance
        -- Upserts a single TaskUsage row for the given task. If a row
        -- already exists, numeric fields are summed with existing values.
        -- cache_read_tokens and cache_write_tokens default to zero when
        -- omitted by the caller.
}
```

- [ ] Add `rule ReportUsageViaMcp` block

### Step 6.5: Restore tag-based routing in `rule DispatchEpicSubtask`

Find the `rule DispatchEpicSubtask` ensures block. The current simplified version is:

```
    ensures:
        let candidates = epic.active_subtasks where status = backlog
        let next = sort_by_order(candidates).first
        if next != null:
            if next.has_plan:
                AgentLaunched(task: next, mode: implementation)
            else:
                AgentLaunched(task: next, mode: brainstorm)
```

Restore the tag-based routing:

```
    ensures:
        let candidates = epic.active_subtasks where status = backlog
        let next = sort_by_order(candidates).first
        if next != null:
            if next.has_plan:
                AgentLaunched(task: next, mode: implementation)
            else if next.tag = "epic":
                AgentLaunched(task: next, mode: brainstorm)
            else if next.tag = "feature":
                AgentLaunched(task: next, mode: plan)
            else:
                AgentLaunched(task: next, mode: standard)
```

Also restore the `@guidance` comment explaining routing:

```
    @guidance
        -- Routing is based on the subtask's plan and tag:
        --   has_plan        → implementation mode
        --   tag = "epic"    → brainstorm mode
        --   tag = "feature" → plan mode
        --   other           → standard mode (dispatch without plan)
```

- [ ] Restore tag-based routing in DispatchEpicSubtask

### Step 6.6: Update `rule HookNotification` to reflect new behavior

Find the current `rule HookNotification`. Update `ensures` to reflect that status stays `running`:

```
rule HookNotification {
    when: HookFired(hook: notification, task: task)
    requires: task.status = running

    ensures: task.sub_status = needs_input

    @guidance
        -- HookNotification fires when the agent raises a notification
        -- (e.g. a permission prompt). The hook calls:
        --   dispatch update <id> running --only-if running --sub-status needs_input
        -- This sets sub_status=needs_input while keeping status=running.
        -- The task appears in the Blocked visual column (running, needs_input).
        -- When the agent resumes, HookPreToolUse fires and update_status_if
        -- resets sub_status back to active (default for running).
}
```

- [ ] Update HookNotification rule

### Step 6.7: Update `rule ClearNeedsInput` to match new hook behavior

The task no longer transitions through `review` — it stays `running`. Update:

```
rule ClearNeedsInput {
    -- When HookPreToolUse fires, update_status_if(running, running) resets
    -- sub_status to active via the auto-reset behavior.
    when: HookFired(hook: pre_tool_use, task: task)

    requires: task.sub_status = needs_input

    ensures: task.sub_status = SubStatus.default_for(running)  -- i.e. active

    @guidance
        -- sub_status = needs_input is set by HookNotification when the agent
        -- raises a notification. It is cleared when the agent resumes and
        -- HookPreToolUse fires, which calls:
        --   dispatch update <id> running
        -- The update_status_if auto-reset sets sub_status = active.
        -- This allows re-notification if the agent raises another notification.
}
```

- [ ] Update ClearNeedsInput rule

### Step 6.8: Run cargo test and commit

```bash
cargo test 2>&1 | tail -20
```

Expected: all tests pass (Allium spec is not compiled, just committed).

```bash
git add docs/specs/dispatch.allium
git commit -m "docs: restore Allium spec sections lost during rebase

Restores tag field, is_epic/pr_number derived attrs, TaskUsage entity,
ReportUsageViaMcp rule, pr_url+tag in UpdateTaskViaMcp, tag-based routing
in DispatchEpicSubtask, and updates HookNotification/ClearNeedsInput rules
to reflect that Notification now keeps status=running."
```

- [ ] Run tests
- [ ] Commit

---

## Final verification

```bash
cargo test 2>&1 | tail -5
cargo clippy -- -D warnings 2>&1 | tail -20
```

Expected: all tests pass, no clippy warnings.

- [ ] Final test run passes
- [ ] No clippy warnings
