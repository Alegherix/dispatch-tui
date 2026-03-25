// Stub — full implementation in Task 3
use anyhow::Result;
use std::path::Path;

use crate::models::{Task, TaskStatus};

pub struct Database {
    _path: std::path::PathBuf,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Database { _path: path.to_path_buf() })
    }

    pub fn update_status(&self, _id: i64, _status: TaskStatus) -> Result<()> {
        anyhow::bail!("Not yet implemented")
    }

    pub fn list_all(&self) -> Result<Vec<Task>> {
        Ok(vec![])
    }

    pub fn list_by_status(&self, _status: TaskStatus) -> Result<Vec<Task>> {
        Ok(vec![])
    }
}
