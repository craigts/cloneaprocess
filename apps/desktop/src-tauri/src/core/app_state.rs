use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};

use crate::storage::{Storage, StorageError};

pub struct AppState {
    started_at_ms: u64,
    recordings_root: PathBuf,
    storage: Storage,
}

impl AppState {
    pub fn bootstrap(app: &AppHandle) -> Result<Self, StorageError> {
        let app_data_dir = app
            .path()
            .app_data_dir()
            .map_err(|error| StorageError::PathResolution(error.to_string()))?;
        let recordings_root = app_data_dir.join("recordings");
        fs::create_dir_all(&recordings_root)
            .map_err(|source| StorageError::io(recordings_root.clone(), source))?;

        let storage = Storage::bootstrap(app_data_dir.join("storage").join("cloneaprocess.sqlite3"))?;

        Ok(Self {
            started_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0),
            recordings_root,
            storage,
        })
    }

    pub fn started_at_ms(&self) -> u64 {
        self.started_at_ms
    }

    pub fn recordings_root(&self) -> &Path {
        self.recordings_root.as_path()
    }

    pub fn storage(&self) -> &Storage {
        &self.storage
    }
}
