---
name: wrap-up
description: Use when implementation is complete to wrap up a dispatch worktree. Commits remaining changes, asks the user to choose between rebasing onto the task's base_branch or creating a GitHub PR, then calls the wrap_up MCP tool. The task is moved to done automatically on success.
---

# Wrap Up

Wrap up a dispatch worktree: commit remaining changes, use the `AskUserQuestion` tool with a question like:

> Wrap up task #{id} (`{title}`):
> **(r)** rebase onto `{base_branch}` — fast-forwards `{base_branch}` with this branch, kills this tmux window
> **(p)** create PR — pushes branch and opens a GitHub PR targeting `{base_branch}`
> **(Esc / n)** cancel

Then call the `wrap_up` MCP tool. If the user cancels or says no, exit without calling any tool. 

**Announce at start:** "I'm using the wrap-up skill to complete this task."

## Argument check

If the skill was invoked with an argument (e.g. `/wrap-up rebase` or `/wrap-up pr`):
- Treat the argument as the chosen action (`rebase` or `pr`)
- Skip Step 4 (AskUserQuestion) entirely
- After completing Steps 1–3, go straight to Step 5 with that action

If the argument is anything other than `rebase` or `pr`, ignore it and proceed normally (Step 4 will ask).

**Precondition:** The task must be in "running" or "review" status. The `wrap_up` MCP tool will reject tasks in any other status.

## Step 1: Get the task ID from the current branch

Run:
```bash
git rev-parse --abbrev-ref HEAD
```

Extract the leading integer from the `{id}-{slug}` pattern (e.g. `42-fix-login-bug` → `42`).

If the branch does not match the `{id}-{slug}` pattern, stop and tell the user:
> "This branch doesn't follow the dispatch naming convention (`{id}-{slug}`). Cannot determine task ID."

## Step 2: Get task details and dispatch next epic subtask

Call the `dispatch` MCP tool `get_task` with the task ID from Step 1. Read the `base_branch` field from the response — use it wherever the instructions below refer to `{base_branch}`. If `base_branch` is absent or empty, fall back to `main`.

If the task has an `epic_id`, call the `dispatch` MCP tool `dispatch_next` with that `epic_id`. This fires the next agent immediately — before any user interaction.

If the task does not have an `epic_id`, skip the dispatch_next call.

## Step 3: Commit uncommitted changes

Run:
```bash
git status --porcelain
```

If there are no changes, skip to Step 3.

If there are changes, commit them inline — do NOT invoke a commit skill or delegate to another tool. Run these commands directly:

1. `git add` the relevant files (prefer named files over `git add -A`)
2. `git diff --cached` to review what's staged
3. `git commit -m "..."` with a short message summarizing the changes

Do NOT spend time perfecting the commit message. The goal is to capture the changes, not write a polished commit. Once committed, proceed immediately to Step 3.

## Step 4: Ask the user to choose — MANDATORY

**You MUST use the `AskUserQuestion` tool here.** Do NOT skip this step. Do NOT assume a default. Do NOT proceed to Step 5 without an explicit answer from the user.

Use the `AskUserQuestion` tool with a question like:

> Wrap up task #{id} (`{title}`):
> **(r)** rebase onto `{base_branch}` — fast-forwards `{base_branch}` with this branch, kills this tmux window
> **(p)** create PR — pushes branch and opens a GitHub PR targeting `{base_branch}`
> **(Esc / n)** cancel

If the user cancels or says no, exit without calling any tool.

## Step 5: Execute the chosen action

The task is automatically moved to "done" on success. Do not update the task status yourself.

### If rebase:

Call the `dispatch` MCP tool `wrap_up` with:
- `task_id`: the integer from Step 1
- `action`: `"rebase"`

The tool blocks until the rebase completes. On success, the task is moved to "done" and the tmux window is killed, ending this session. Do not attempt any further actions after a successful rebase.

If the tool returns an error (e.g. rebase conflict, repo not on `{base_branch}`), show the user the exact error message from the response and suggest resolution steps. The task remains in its current status.

### If PR:

Call the `dispatch` MCP tool `wrap_up` with:
- `task_id`: the integer from Step 1
- `action`: `"pr"`

The tool blocks until the PR is created. On success, it returns the PR URL and number. A `/code-review` command will be injected into this session once the PR is ready.

If the tool returns an error (e.g. push failed, PR creation failed), show the user the exact error message from the response. The task remains in its current status.
