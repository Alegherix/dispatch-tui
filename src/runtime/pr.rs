use super::*;

impl TuiRuntime {
    pub(super) fn exec_create_pr(
        &self,
        id: TaskId,
        repo_path: String,
        branch: String,
        base_branch: String,
        title: String,
        description: String,
    ) {
        let tx = self.msg_tx.clone();
        let runner = self.runner.clone();

        tokio::task::spawn_blocking(move || {
            match dispatch::create_pr(
                &repo_path,
                &branch,
                &title,
                &description,
                &base_branch,
                &*runner,
            ) {
                Ok(result) => {
                    let _ = tx.send(Message::PrCreated {
                        id,
                        pr_url: result.pr_url,
                    });
                }
                Err(e) => {
                    let _ = tx.send(Message::PrFailed {
                        id,
                        error: e.to_string(),
                    });
                }
            }
        });
    }

    pub(super) fn exec_check_pr_status(&self, id: TaskId, pr_url: String) {
        let tx = self.msg_tx.clone();
        let runner = self.runner.clone();

        tokio::task::spawn_blocking(move || {
            match dispatch::check_pr_status(&pr_url, &*runner) {
                Ok(status) => {
                    if status.state == dispatch::PrState::Merged {
                        let _ = tx.send(Message::PrMerged(id));
                    } else if status.state == dispatch::PrState::Open {
                        let _ = tx.send(Message::PrReviewState {
                            id,
                            review_decision: status.review_decision,
                        });
                    }
                    // Closed PRs: no message
                }
                Err(e) => {
                    tracing::warn!(task_id = id.0, "PR status check failed: {e}");
                }
            }
        });
    }

    pub(super) fn exec_merge_pr(&self, id: TaskId, pr_url: String) {
        let tx = self.msg_tx.clone();
        let runner = self.runner.clone();

        tokio::task::spawn_blocking(move || match dispatch::merge_pr(&pr_url, &*runner) {
            Ok(()) => {
                let _ = tx.send(Message::PrMerged(id));
            }
            Err(e) => {
                let _ = tx.send(Message::MergePrFailed {
                    id,
                    error: e.to_string(),
                });
            }
        });
    }
}
