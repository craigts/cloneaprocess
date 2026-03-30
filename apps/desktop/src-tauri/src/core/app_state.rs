use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};

use crate::core::helper_paths::{NativeHelper, resolve_helper_binary};
use crate::core::recorder::{RecorderCoordinator, RecorderTransportConfig};
use crate::core::retention::run_retention_cleanup;
use crate::storage::{Storage, StorageError};

pub struct AppState {
    started_at_ms: u64,
    recordings_root: PathBuf,
    storage: Storage,
    recorder: Mutex<RecorderCoordinator>,
    runner_binary: PathBuf,
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
        let _ = run_retention_cleanup(&storage, &recordings_root);
        let recorder_transport = resolve_recorder_transport(app);
        let runner_binary = resolve_helper_binary(app, NativeHelper::Runner);

        Ok(Self {
            started_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0),
            recordings_root,
            recorder: Mutex::new(RecorderCoordinator::new(storage.clone(), recorder_transport)),
            storage,
            runner_binary,
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

    pub fn runner_binary(&self) -> &Path {
        self.runner_binary.as_path()
    }
}

fn resolve_recorder_transport(app: &AppHandle) -> RecorderTransportConfig {
    match std::env::var("CLONEAPROCESS_RECORDER_TRANSPORT")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("xpc_bundle_service") => RecorderTransportConfig::xpc_bundle_service(
            std::env::var("CLONEAPROCESS_RECORDER_XPC_SERVICE")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "com.cloneaprocess.recorder".to_string()),
        ),
        Some("xpc") | Some("xpc_service") => RecorderTransportConfig::xpc_service(
            std::env::var("CLONEAPROCESS_RECORDER_XPC_SERVICE")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "com.cloneaprocess.recorder".to_string()),
        ),
        _ if bundled_recorder_service_exists() => RecorderTransportConfig::xpc_bundle_service(
            std::env::var("CLONEAPROCESS_RECORDER_XPC_SERVICE")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| "com.cloneaprocess.recorder".to_string()),
        ),
        _ => RecorderTransportConfig::subprocess_bridge(resolve_helper_binary(app, NativeHelper::Recorder)),
    }
}

fn bundled_recorder_service_exists() -> bool {
    let service_root = match std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .and_then(|macos_dir| macos_dir.parent().map(Path::to_path_buf))
        .map(|contents_dir| contents_dir.join("XPCServices").join("RecorderService.xpc"))
    {
        Some(path) => path,
        None => return false,
    };

    service_root.join("Contents").join("Info.plist").exists()
        && service_root.join("Contents").join("MacOS").join("RecorderService").exists()
}
