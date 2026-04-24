use super::*;

impl TuiRuntime {
    pub(super) fn exec_send_notification(&self, title: &str, body: &str, urgent: bool) {
        let urgency = if urgent { "critical" } else { "normal" };
        if let Err(e) = self
            .runner
            .run("notify-send", &["-u", urgency, title, body])
        {
            tracing::warn!("notify-send failed: {e}");
        }
    }

    pub(super) fn exec_persist_setting(&self, app: &mut App, key: &str, value: bool) {
        if let Err(e) = self.database.set_setting_bool(key, value) {
            app.update(Message::Error(Self::db_error("persisting setting", e)));
        }
    }

    pub(super) fn exec_persist_string_setting(&self, app: &mut App, key: &str, value: &str) {
        if let Err(e) = self.database.set_setting_string(key, value) {
            app.update(Message::Error(Self::db_error("persisting setting", e)));
        }
    }

    pub(super) fn exec_persist_filter_preset(
        &self,
        app: &mut App,
        name: &str,
        repo_paths: &[String],
        mode: &str,
    ) {
        if let Err(e) = self.database.save_filter_preset(name, repo_paths, mode) {
            app.update(Message::Error(Self::db_error("saving filter preset", e)));
        }
    }

    pub(super) fn exec_delete_filter_preset(&self, app: &mut App, name: &str) {
        if let Err(e) = self.database.delete_filter_preset(name) {
            app.update(Message::Error(Self::db_error("deleting filter preset", e)));
        }
    }

    pub(super) fn exec_refresh_usage_from_db(&self, app: &mut App) {
        match self.database.get_all_usage() {
            Ok(usage) => {
                app.update(Message::RefreshUsage(usage));
            }
            Err(e) => {
                app.update(Message::Error(Self::db_error("refreshing usage", e)));
            }
        }
    }

    pub(super) fn exec_open_in_browser(&self, url: String) {
        let runner = self.runner.clone();
        tokio::task::spawn_blocking(move || {
            if let Err(e) = runner.run("xdg-open", &[&url]) {
                tracing::warn!("Failed to open browser: {e}");
            }
        });
    }
}
