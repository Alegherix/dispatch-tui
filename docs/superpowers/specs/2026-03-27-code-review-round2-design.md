# Code Review Round 2 — Design

**Date:** 2026-03-27
**Scope:** Address all verified findings from the second code review, building on fixes already merged from the first round.

---

## Context

A comprehensive code review identified 18 claims. After verification against the current codebase (which includes the `10-code-review-fixes` branch), 11 claims are fully true, 3 partially true, and 4 false (already fixed or inaccurate). This spec addresses all verified issues.

### What was already fixed (first round)
- Mutex poisoning: all 14 `.lock().unwrap()` → `.lock().map_err()`
- ProcessRunner trait injected into tmux/dispatch/runtime
- `dispatch_agent` has 3 tests via MockProcessRunner
- File-based tracing with `tracing` crate
- RAII temp files in editor via `tempfile` crate
- CLAUDE.md documentation improvements

### What remains (this round)
- **Bugs:** stale `add_note` prompt, dead `_mcp_port` param
- **Reliability:** non-atomic MCP updates, swallowed errors
- **Code quality:** dispatch duplication, exec duplication, input.rs duplication, handle_key_normal complexity
- **Type safety:** raw `i64` task IDs, long parameter lists
- **Tests:** brainstorm/quick_dispatch agents, repo_paths DB methods, runtime.rs

---

## Workstream 1: Bug Fixes

### 1a. Stale `add_note` in quick dispatch prompt

**File:** `src/dispatch.rs:233-235`

The `build_quick_dispatch_prompt` references `add_note`, an MCP tool that was removed in commit `6a9ab58`. The test `build_quick_dispatch_prompt_mentions_mcp` asserts the stale text.

**Fix:** Replace lines 233-235 with text referencing the actual MCP tools (`update_task`, `get_task`, `create_task`):

```rust
"An MCP server is available at http://localhost:{mcp_port}/mcp — use it to \
query and update tasks (tool: task-orchestrator). Use update_task to rename \
this task with a descriptive title, and get_task to check current state."
```

Update the test to assert on `update_task` instead of `add_note`.

### 1b. Remove dead `_mcp_port` parameter from `build_prompt`

**File:** `src/dispatch.rs:197`

`build_prompt` takes `_mcp_port: u16` but never uses it (the regular dispatch prompt doesn't mention MCP — status transitions are hook-driven).

**Fix:** Remove the parameter from `build_prompt`. Update the call site in `dispatch_agent` (line 57) to stop passing `mcp_port` to the prompt builder. The public `dispatch_agent` signature keeps `mcp_port` for API consistency with `brainstorm_agent` and `quick_dispatch_agent`.

Update the prompt builder test accordingly.

---

## Workstream 2: Reliability

### 2a. Atomic MCP task updates

**File:** `src/mcp/handlers.rs:270-332`, `src/db.rs`

`handle_update_task` performs up to 3 separate DB calls (`update_status`, `update_plan`, `update_title_description`) without a transaction. If the second call fails, the first has already committed.

**Fix:** Add a new `TaskStore` method that runs all partial updates in a single transaction:

```rust
// TaskStore trait addition:
fn update_task_partial(
    &self,
    id: i64,
    status: Option<TaskStatus>,
    plan: Option<Option<&str>>,       // None = don't touch, Some(None) = clear, Some(Some("x")) = set
    title: Option<&str>,              // None = don't touch, Some("x") = set
    description: Option<&str>,        // None = don't touch, Some("x") = set
) -> Result<()>;
```

The `Database` implementation opens a transaction, applies each `Some` field as an UPDATE, and commits atomically. If any step fails, the whole transaction rolls back.

`handle_update_task` calls this single method instead of 3 separate ones. The individual methods (`update_status`, `update_plan`, `update_title_description`) remain for other callers.

After `TaskId` newtype is introduced (Workstream 4), the `id` parameter becomes `TaskId`.

### 2b. Surface swallowed errors

**File:** `src/runtime.rs`

Replace silent discards with `tracing::warn!` for errors that could indicate real problems:

| Location | Pattern | Fix |
|----------|---------|-----|
| `exec_save_repo_path` (line 392) | `let _ = self.database.save_repo_path(...)` | `if let Err(e) = ... { tracing::warn!("failed to save repo path: {e}"); }` |
| `exec_save_repo_path` (line 393) | `.unwrap_or_default()` | `.unwrap_or_else(\|e\| { tracing::warn!("failed to list repo paths: {e}"); vec![] })` |
| `exec_quick_dispatch` (line 195) | Same `let _ =` pattern | Same fix |
| `exec_quick_dispatch` (line 196) | Same `.unwrap_or_default()` | Same fix |
| `exec_edit_in_editor` (lines 349-386) | Editor spawn/exit failure silently ignored | Add `tracing::warn!` for spawn failure; add `tracing::warn!` for non-zero exit code |

**Not changed:** `tx.send()` calls (11 sites). These use `let _ =` intentionally — if the receiver is dropped, the app is shutting down and there's nothing to log to. Add a `// receiver dropped = app shutting down` comment on the first occurrence for clarity.

---

## Workstream 3: Dispatch Unification

### 3a. Extract `dispatch_with_prompt` in `dispatch.rs`

**File:** `src/dispatch.rs:54-113`

The three dispatch functions (`dispatch_agent`, `brainstorm_agent`, `quick_dispatch_agent`) share identical post-provision logic: write prompt to `.claude-prompt`, send keys to tmux.

**Fix:** Extract a private helper:

```rust
fn dispatch_with_prompt(
    task: &Task,
    prompt: &str,
    runner: &dyn ProcessRunner,
) -> Result<DispatchResult> {
    let provision = provision_worktree(task, runner)?;

    let prompt_file = format!("{}/.claude-prompt", provision.worktree_path);
    fs::write(&prompt_file, prompt)
        .with_context(|| format!("failed to write {prompt_file}"))?;
    tmux::send_keys(
        &provision.tmux_window,
        "claude \"$(cat .claude-prompt)\"",
        runner,
    )
    .context("failed to send keys to tmux window")?;

    tracing::info!(task_id = task.id, worktree = %provision.worktree_path, "agent dispatched");

    Ok(DispatchResult {
        worktree_path: provision.worktree_path,
        tmux_window: provision.tmux_window,
    })
}
```

The three public functions become:

```rust
pub fn dispatch_agent(task: &Task, mcp_port: u16, runner: &dyn ProcessRunner) -> Result<DispatchResult> {
    let prompt = build_prompt(task.id, &task.title, &task.description, task.plan.as_deref());
    dispatch_with_prompt(task, &prompt, runner)
}

pub fn brainstorm_agent(task: &Task, mcp_port: u16, runner: &dyn ProcessRunner) -> Result<DispatchResult> {
    let prompt = build_brainstorm_prompt(task.id, &task.title, &task.description, mcp_port);
    dispatch_with_prompt(task, &prompt, runner)
}

pub fn quick_dispatch_agent(task: &Task, mcp_port: u16, runner: &dyn ProcessRunner) -> Result<DispatchResult> {
    let prompt = build_quick_dispatch_prompt(task.id, &task.title, &task.description, mcp_port);
    dispatch_with_prompt(task, &prompt, runner)
}
```

Existing tests continue to pass — they test the public functions, which now delegate internally.

### 3b. Unify `exec_dispatch` and `exec_brainstorm` in `runtime.rs`

**File:** `src/runtime.rs:241-285`

These two methods are identical except for which dispatch function they call and the log/error label.

**Fix:** Extract a generic helper:

```rust
fn spawn_dispatch<F>(&self, task: models::Task, dispatch_fn: F, label: &'static str)
where
    F: FnOnce(&models::Task, u16, &dyn ProcessRunner) -> Result<DispatchResult> + Send + 'static,
{
    let tx = self.msg_tx.clone();
    let port = self.port;
    let runner = self.runner.clone();

    tokio::task::spawn_blocking(move || {
        let id = task.id;
        tracing::info!(task_id = id, label, "dispatching");
        match dispatch_fn(&task, port, &*runner) {
            Ok(result) => {
                let _ = tx.send(Message::Dispatched {
                    id,
                    worktree: result.worktree_path,
                    tmux_window: result.tmux_window,
                });
            }
            Err(e) => {
                let _ = tx.send(Message::Error(format!("{label} failed: {e:#}")));
            }
        }
    });
}

fn exec_dispatch(&self, task: models::Task) {
    self.spawn_dispatch(task, dispatch::dispatch_agent, "Dispatch");
}

fn exec_brainstorm(&self, task: models::Task) {
    self.spawn_dispatch(task, dispatch::brainstorm_agent, "Brainstorm");
}
```

`exec_quick_dispatch` stays separate — it has unique pre-dispatch logic (DB insert, state update, repo path save) that doesn't fit the shared pattern.

### 3c. Extract `finish_task_creation` in `input.rs`

**File:** `src/tui/input.rs:208-215` and `237-244`

The `InsertTask + SaveRepoPath` command pair is duplicated in `handle_key_text_input` (Enter on InputRepoPath, and digit shortcut on InputRepoPath).

**Fix:** Extract a private method on `App`:

```rust
fn finish_task_creation(&mut self, repo_path: String) -> Vec<Command> {
    let draft = self.task_draft.take().unwrap_or_default();
    self.mode = InputMode::Normal;
    self.status_message = None;
    vec![
        Command::InsertTask {
            title: draft.title,
            description: draft.description,
            repo_path: repo_path.clone(),
        },
        Command::SaveRepoPath(repo_path),
    ]
}
```

Both call sites become `self.finish_task_creation(repo_path)`.

---

## Workstream 4: Type Safety

### 4a. `TaskId` newtype

**File:** `src/models.rs` (definition), then ~68 sites across the codebase

Raw `i64` is used for task IDs everywhere, making it possible to accidentally pass a row count, column index, or other integer where a task ID is expected.

**Fix:** Introduce a newtype in `models.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(pub i64);

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
```

Propagate through:
- `Task.id: TaskId`
- All `Message` variants that carry `id: i64` → `id: TaskId`
- All `Command` variants that carry `id: i64` → `id: TaskId`
- `TaskStore` trait methods: `id: i64` → `id: TaskId`
- `dispatch.rs` functions: `task_id: i64` → `task_id: TaskId`
- `mcp/handlers.rs`: parse incoming i64 from JSON, wrap in `TaskId`
- `tui/mod.rs` handler methods
- Tests: wrap literals in `TaskId(n)`

The compiler guides every change — any missed site is a compile error. Access the inner `i64` via `.0` for SQLite params and string formatting.

### 4b. `format_editor_content` takes `&Task`

**File:** `src/editor.rs:9`

Currently takes 5 separate string params. Since it always formats the same fields from a `Task`, take `&Task` directly:

```rust
pub fn format_editor_content(task: &Task) -> String {
    format!(
        "--- TITLE ---\n{}\n--- DESCRIPTION ---\n{}\n--- REPO_PATH ---\n{}\n--- STATUS ---\n{}\n--- PLAN ---\n{}\n",
        task.title,
        task.description,
        task.repo_path,
        task.status.as_str(),
        task.plan.as_deref().unwrap_or(""),
    )
}
```

Update the call site in `runtime.rs:exec_edit_in_editor` and tests.

### 4c. Simplify `Command::InsertTask` with `TaskDraft`

**File:** `src/tui/types.rs`

`Command::InsertTask { title, description, repo_path }` duplicates fields already in `TaskDraft` (title, description). Add `repo_path` to `TaskDraft` and reuse:

Actually, `TaskDraft` is a multi-step builder (title entered first, description second, repo_path third). Adding `repo_path` to `TaskDraft` changes its semantics — it becomes a complete "new task" description rather than an in-progress draft. This is a reasonable evolution:

```rust
#[derive(Debug, Clone, Default)]
pub struct TaskDraft {
    pub title: String,
    pub description: String,
    pub repo_path: String,
}
```

```rust
// Command becomes:
InsertTask(TaskDraft),
```

The multi-step input flow builds up `TaskDraft` incrementally (title → description → repo_path) and emits `Command::InsertTask(draft)` at the end. `exec_insert_task` destructures the draft.

Note: `repo_path` is set last in the input flow, so the draft's `repo_path` field starts empty and is filled in `finish_task_creation`. This is fine — the draft isn't validated until it becomes a Command.

---

## Workstream 5: Test Coverage

### 5a. `brainstorm_agent` and `quick_dispatch_agent` tests

**File:** `src/dispatch.rs` test module

Mirror the existing `dispatch_agent` tests using `MockProcessRunner`:

```rust
#[test]
fn brainstorm_creates_worktree_then_opens_tmux() {
    // Arrange: mock git worktree add (success), tmux new-window (success),
    //          tmux send-keys (success)
    // Act: brainstorm_agent(&task, 3142, &mock)
    // Assert: git worktree add called first, then tmux new-window, then send-keys
}

#[test]
fn brainstorm_prompt_includes_planning_instructions() {
    // Assert prompt contains "brainstorming session" and "implementation plan"
}

#[test]
fn quick_dispatch_creates_worktree_then_opens_tmux() {
    // Same structure as brainstorm test
}

#[test]
fn quick_dispatch_prompt_includes_rename_instructions() {
    // Assert prompt contains "update_task" and "rename"
}
```

### 5b. `save_repo_path` / `list_repo_paths` tests

**File:** `src/db.rs` test module

```rust
#[test]
fn save_and_list_repo_paths() {
    let db = Database::open_in_memory().unwrap();
    assert!(db.list_repo_paths().unwrap().is_empty());
    db.save_repo_path("/home/user/project").unwrap();
    db.save_repo_path("/home/user/other").unwrap();
    let paths = db.list_repo_paths().unwrap();
    assert_eq!(paths.len(), 2);
    assert!(paths.contains(&"/home/user/project".to_string()));
    assert!(paths.contains(&"/home/user/other".to_string()));
}

#[test]
fn save_repo_path_deduplicates() {
    let db = Database::open_in_memory().unwrap();
    db.save_repo_path("/home/user/project").unwrap();
    db.save_repo_path("/home/user/project").unwrap();
    assert_eq!(db.list_repo_paths().unwrap().len(), 1);
}

#[test]
fn list_repo_paths_empty_by_default() {
    let db = Database::open_in_memory().unwrap();
    assert!(db.list_repo_paths().unwrap().is_empty());
}
```

### 5c. `update_task_partial` transaction test

**File:** `src/db.rs` test module

```rust
#[test]
fn update_task_partial_applies_all_fields_atomically() {
    let db = Database::open_in_memory().unwrap();
    let id = db.create_task("title", "desc", "/repo", None, TaskStatus::Backlog).unwrap();
    db.update_task_partial(id, Some(TaskStatus::Ready), Some(Some("plan.md")), Some("new title"), None).unwrap();
    let task = db.get_task(id).unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Ready);
    assert_eq!(task.plan.as_deref(), Some("plan.md"));
    assert_eq!(task.title, "new title");
    assert_eq!(task.description, "desc"); // unchanged
}

#[test]
fn update_task_partial_none_fields_unchanged() {
    let db = Database::open_in_memory().unwrap();
    let id = db.create_task("title", "desc", "/repo", Some("plan.md"), TaskStatus::Ready).unwrap();
    db.update_task_partial(id, None, None, None, None).unwrap(); // no-op
    let task = db.get_task(id).unwrap().unwrap();
    assert_eq!(task.title, "title");
    assert_eq!(task.plan.as_deref(), Some("plan.md"));
}
```

### 5d. Basic runtime exec tests

**File:** `src/runtime.rs` (new `#[cfg(test)]` module)

Test the synchronous `exec_*` methods that don't require a terminal or async runtime:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::process::MockProcessRunner;
    use std::sync::Arc;

    fn test_runtime() -> (TuiRuntime, App) {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let runner = Arc::new(MockProcessRunner::new(vec![]));
        let rt = TuiRuntime {
            database: db.clone(),
            msg_tx: tx,
            port: 3142,
            input_paused: Arc::new(AtomicBool::new(false)),
            runner,
        };
        let tasks = db.list_all().unwrap();
        let app = App::new(tasks, vec![]);
        (rt, app)
    }

    #[test]
    fn exec_insert_task_adds_to_db_and_app() {
        let (rt, mut app) = test_runtime();
        rt.exec_insert_task(&mut app, "Test".into(), "Desc".into(), "/repo".into());
        assert_eq!(app.tasks().len(), 1);
        assert_eq!(rt.database.list_all().unwrap().len(), 1);
    }

    #[test]
    fn exec_delete_task_removes_from_db() {
        let (rt, mut app) = test_runtime();
        rt.exec_insert_task(&mut app, "Test".into(), "Desc".into(), "/repo".into());
        let id = app.tasks()[0].id;
        rt.exec_delete_task(&mut app, id);
        assert!(rt.database.list_all().unwrap().is_empty());
    }

    #[test]
    fn exec_save_repo_path_updates_app() {
        let (rt, mut app) = test_runtime();
        rt.exec_save_repo_path(&mut app, "/repo".into());
        assert!(app.repo_paths().contains(&"/repo".to_string()));
    }

    #[test]
    fn exec_refresh_from_db_syncs_state() {
        let (rt, mut app) = test_runtime();
        // Insert directly into DB, bypassing app
        rt.database.create_task("External", "Added via CLI", "/repo", None, TaskStatus::Backlog).unwrap();
        assert!(app.tasks().is_empty());
        rt.exec_refresh_from_db(&mut app);
        assert_eq!(app.tasks().len(), 1);
    }
}
```

---

## Workstream Ordering and Dependencies

```
Workstream 1 (bugs)  ─────────────────────────────┐
                                                    │
Workstream 2 (reliability) ────────────────────────┤
                                                    ├─► can all be separate PRs
Workstream 3 (dispatch unification) ───────────────┤     merged independently
                                                    │
Workstream 5 (tests) ──────────────────────────────┘

Workstream 4 (type safety) ── depends on 1 + 3 being merged first
                               (TaskId touches the same dispatch signatures)
```

Workstreams 1-3 and 5 are independent of each other and can be implemented/merged in any order. Workstream 4 (`TaskId` newtype) is a wide-reaching refactor that's easier to land after the dispatch signatures stabilize from Workstreams 1 and 3.

---

## Constraints

- No changes to the MCP JSON-RPC protocol (wire format unchanged)
- No changes to the SQLite schema (existing DBs continue to work)
- No changes to the Elm Architecture contract (Messages/Commands/update flow)
- `MockProcessRunner` remains test-only
- All existing tests must continue to pass (with updates for renamed params/types)

---

## File Change Summary

| File | Workstream | Changes |
|------|-----------|---------|
| `src/dispatch.rs` | 1a, 1b, 3a | Fix prompt, remove dead param, extract `dispatch_with_prompt` |
| `src/runtime.rs` | 2b, 3b | Surface errors, extract `spawn_dispatch`, add test module |
| `src/mcp/handlers.rs` | 2a | Replace 3 DB calls with single `update_task_partial` |
| `src/db.rs` | 2a, 5b, 5c | Add `update_task_partial` + transaction, add repo_paths tests |
| `src/tui/input.rs` | 3c | Extract `finish_task_creation` |
| `src/tui/types.rs` | 4a, 4c | `TaskId` in Message/Command, `TaskDraft` with repo_path |
| `src/models.rs` | 4a | Add `TaskId` newtype |
| `src/editor.rs` | 4b | `format_editor_content` takes `&Task` |
