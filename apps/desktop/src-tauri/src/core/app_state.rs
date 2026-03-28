use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};

use crate::core::recorder::RecorderCoordinator;
use crate::storage::{Storage, StorageError};

pub struct AppState {
    started_at_ms: u64,
    recordings_root: PathBuf,
    storage: Storage,
    recorder: Mutex<RecorderCoordinator>,
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
        let recorder_binary = resolve_recorder_binary(app);

        Ok(Self {
            started_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0),
            recordings_root,
            recorder: Mutex::new(RecorderCoordinator::new(storage.clone(), recorder_binary)),
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

    pub fn recorder(&self) -> &Mutex<RecorderCoordinator> {
        &self.recorder
    }
}

fn resolve_recorder_binary(app: &AppHandle) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| manifest_dir.clone());

    let repo_relative = PathBuf::from("native")
        .join("mac-recorder-service")
        .join(".build")
        .join("debug")
        .join("RecorderService");

    let mut candidates = vec![
        workspace_root.join(&repo_relative),
        manifest_dir.join("..").join("..").join("..").join(&repo_relative),
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(&repo_relative),
    ];

    if let Ok(resource_path) = app.path().resolve(
        "../../native/mac-recorder-service/.build/debug/RecorderService",
        tauri::path::BaseDirectory::Resource,
    ) {
        candidates.push(resource_path);
    }

    candidates
        .into_iter()
        .find(|path| path.exists())
        .unwrap_or_else(|| workspace_root.join(repo_relative))
}
