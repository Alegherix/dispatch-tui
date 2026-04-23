use super::*;

impl TuiRuntime {
    pub(super) fn exec_persist_fix_agent(
        &self,
        app: &mut App,
        github_repo: &str,
        number: i64,
        kind: models::AlertKind,
        tmux_window: &str,
        worktree: &str,
    ) -> Vec<Command> {
        if let Err(e) =
            self.database
                .set_alert_agent(github_repo, number, kind, tmux_window, worktree)
        {
            return app.update(Message::Error(Self::db_error("persisting fix agent", e)));
        }

        use crate::models::{SecurityWorkflowState, SecurityWorkflowSubState, WorkflowItemKind};
        use crate::tui::types::WorkflowKey;

        let workflow_kind = match kind {
            models::AlertKind::Dependabot => WorkflowItemKind::DependabotAlert,
            models::AlertKind::CodeScanning => WorkflowItemKind::CodeScanAlert,
        };
        let key = WorkflowKey::new(github_repo.to_string(), number, workflow_kind);
        if let Err(e) = self.database.upsert_pr_workflow(
            github_repo,
            number,
            workflow_kind,
            SecurityWorkflowState::Ongoing.as_db_str(),
            Some(SecurityWorkflowSubState::Investigating.as_db_str()),
        ) {
            tracing::warn!("Failed to persist security workflow state on dispatch: {e}");
        }
        app.update(Message::SecurityWorkflowUpdated {
            key,
            state: SecurityWorkflowState::Ongoing,
            sub_state: Some(SecurityWorkflowSubState::Investigating),
        });

        vec![]
    }

    pub(super) fn exec_dispatch_fix_agent(&self, req: tui::FixAgentRequest) {
        // repo is already resolved to a local path by the TUI
        let tx = self.msg_tx.clone();
        let runner = self.runner.clone();
        tokio::task::spawn_blocking(move || {
            let github_repo = req.github_repo.clone();
            let number = req.number;
            let kind = req.kind;
            match dispatch::dispatch_fix_agent(req, &*runner) {
                Ok(result) => {
                    let _ = tx.send(Message::FixAgentDispatched {
                        github_repo,
                        number,
                        kind,
                        tmux_window: result.tmux_window,
                        worktree: result.worktree_path,
                    });
                }
                Err(e) => {
                    let _ = tx.send(Message::FixAgentFailed {
                        github_repo,
                        number,
                        kind,
                        error: e.to_string(),
                    });
                }
            }
        });
    }

    pub(super) fn exec_dispatch_review_agent(&self, req: ReviewAgentRequest) {
        // repo is already resolved to a local path by the TUI
        let tx = self.msg_tx.clone();
        let runner = self.runner.clone();
        tokio::task::spawn_blocking(move || {
            match crate::dispatch::dispatch_review_agent(&req, &*runner) {
                Ok(result) => {
                    let _ = tx.send(Message::ReviewAgentDispatched {
                        github_repo: req.github_repo,
                        number: req.number,
                        tmux_window: result.tmux_window,
                        worktree: result.worktree_path,
                    });
                }
                Err(e) => {
                    let _ = tx.send(Message::ReviewAgentFailed {
                        github_repo: req.github_repo,
                        number: req.number,
                        error: format!("{e:#}"),
                    });
                }
            }
        });
    }
}
