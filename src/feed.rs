use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::db::TaskAndEpicStore;
use crate::mcp::McpEvent;
use crate::models::{EpicId, FeedItem};

const DEFAULT_FEED_INTERVAL: Duration = Duration::from_secs(30);

pub struct FeedRunner {
    db: Arc<dyn TaskAndEpicStore>,
    notify: mpsc::UnboundedSender<McpEvent>,
    last_run: HashMap<EpicId, Instant>,
}

impl FeedRunner {
    pub fn new(db: Arc<dyn TaskAndEpicStore>, notify: mpsc::UnboundedSender<McpEvent>) -> Self {
        Self {
            db,
            notify,
            last_run: HashMap::new(),
        }
    }

    pub async fn tick(&mut self) {
        let epics = match self.db.list_epics() {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!("FeedRunner: failed to list epics: {err:#}");
                return;
            }
        };

        for epic in epics {
            let Some(ref cmd) = epic.feed_command else {
                continue;
            };

            let interval = epic
                .feed_interval_secs
                .map(|s| Duration::from_secs(s as u64))
                .unwrap_or(DEFAULT_FEED_INTERVAL);

            let elapsed = self
                .last_run
                .get(&epic.id)
                .map(|t| t.elapsed())
                .unwrap_or(Duration::MAX);

            if elapsed < interval {
                continue;
            }

            let output = match tokio::process::Command::new("sh")
                .args(["-c", cmd])
                .output()
                .await
            {
                Ok(o) => o,
                Err(err) => {
                    tracing::warn!(
                        epic_id = epic.id.0,
                        epic_title = %epic.title,
                        "FeedRunner: failed to spawn command: {err:#}"
                    );
                    self.last_run.insert(epic.id, Instant::now());
                    continue;
                }
            };

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!(
                    epic_id = epic.id.0,
                    epic_title = %epic.title,
                    "FeedRunner: command exited non-zero: {stderr}"
                );
                self.last_run.insert(epic.id, Instant::now());
                continue;
            }

            let items: Vec<FeedItem> = match serde_json::from_slice::<Vec<FeedItem>>(&output.stdout)
            {
                Ok(i) => i,
                Err(err) => {
                    tracing::warn!(
                        epic_id = epic.id.0,
                        epic_title = %epic.title,
                        "FeedRunner: failed to parse JSON output: {err:#}"
                    );
                    self.last_run.insert(epic.id, Instant::now());
                    continue;
                }
            };

            if let Err(err) = self.db.upsert_feed_tasks(epic.id, &items) {
                tracing::warn!(
                    epic_id = epic.id.0,
                    "FeedRunner: upsert_feed_tasks failed: {err:#}"
                );
            } else {
                let _ = self.notify.send(McpEvent::Refresh);
            }

            self.last_run.insert(epic.id, Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, EpicCrud, EpicPatch};
    use std::sync::Arc;

    fn make_runner(db: Arc<Database>) -> (FeedRunner, mpsc::UnboundedReceiver<McpEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (FeedRunner::new(db, tx), rx)
    }

    #[tokio::test]
    async fn tick_valid_json_upserts_tasks() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let epic = db.create_epic("My Epic", "", "/repo", None).unwrap();
        db.patch_epic(
            epic.id,
            &EpicPatch::new().feed_command(Some(
                r#"echo '[{"external_id":"1","title":"T","description":"D","status":"backlog"}]'"#,
            )),
        )
        .unwrap();

        let (mut runner, _rx) = make_runner(db.clone());
        runner.tick().await;

        let tasks = db.list_tasks_for_epic(epic.id).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "T");
        assert_eq!(tasks[0].external_id.as_deref(), Some("1"));
    }

    #[tokio::test]
    async fn tick_nonzero_exit_no_panic() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let epic = db.create_epic("Err Epic", "", "/repo", None).unwrap();
        db.patch_epic(epic.id, &EpicPatch::new().feed_command(Some("exit 1")))
            .unwrap();

        let (mut runner, _rx) = make_runner(db.clone());
        runner.tick().await; // must not panic

        let tasks = db.list_tasks_for_epic(epic.id).unwrap();
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn tick_malformed_json_no_panic() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let epic = db.create_epic("Bad JSON Epic", "", "/repo", None).unwrap();
        db.patch_epic(
            epic.id,
            &EpicPatch::new().feed_command(Some("echo 'not-json'")),
        )
        .unwrap();

        let (mut runner, _rx) = make_runner(db.clone());
        runner.tick().await; // must not panic

        let tasks = db.list_tasks_for_epic(epic.id).unwrap();
        assert!(tasks.is_empty());
    }

    #[tokio::test]
    async fn tick_interval_not_elapsed_skips_command() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        let epic = db.create_epic("Interval Epic", "", "/repo", None).unwrap();

        // Write a counter to a temp file so we can count how many times the command ran.
        let tmp = std::env::temp_dir().join(format!("feed_test_{}", epic.id.0));
        let cmd = format!(
            r#"echo 0 >> {path}; echo '[{{"external_id":"1","title":"T","description":"","status":"backlog"}}]'"#,
            path = tmp.display()
        );
        db.patch_epic(
            epic.id,
            &EpicPatch::new()
                .feed_command(Some(&cmd))
                .feed_interval_secs(Some(10000)),
        )
        .unwrap();

        let (mut runner, _rx) = make_runner(db.clone());
        // First tick: command runs, counter file gets one line.
        runner.tick().await;
        // Second tick immediately: interval (10000s) not elapsed, command must not run again.
        runner.tick().await;

        let content = std::fs::read_to_string(&tmp).unwrap_or_default();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "command ran {count} times, expected 1",
            count = lines.len()
        );

        let _ = std::fs::remove_file(&tmp);
    }

    #[tokio::test]
    async fn tick_null_feed_command_skipped() {
        let db = Arc::new(Database::open_in_memory().unwrap());
        // Epic with no feed_command (default)
        let epic = db.create_epic("Plain Epic", "", "/repo", None).unwrap();

        let (mut runner, _rx) = make_runner(db.clone());
        runner.tick().await;

        let tasks = db.list_tasks_for_epic(epic.id).unwrap();
        assert!(tasks.is_empty());
    }
}
