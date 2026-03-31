use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};

use crate::core::helper_paths::{resolve_helper_binary, NativeHelper};
use crate::core::recorder::{RecorderCoordinator, RecorderTransportConfig};
use crate::core::retention::run_retention_cleanup;
use crate::storage::{Storage, StorageError};

const BUNDLED_RECORDER_XPC_SERVICE_NAME: &str = "com.cloneaprocess.recorder";
const DEV_RECORDER_XPC_SERVICE_NAME: &str = "com.cloneaprocess.recorder.dev";

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

        let storage =
            Storage::bootstrap(app_data_dir.join("storage").join("cloneaprocess.sqlite3"))?;
        let _ = run_retention_cleanup(&storage, &recordings_root);
        let recorder_transport = resolve_recorder_transport(app);
        let runner_binary = resolve_helper_binary(app, NativeHelper::Runner);

        Ok(Self {
            started_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_millis() as u64)
                .unwrap_or(0),
            recordings_root,
            recorder: Mutex::new(RecorderCoordinator::new(
                storage.clone(),
                recorder_transport,
            )),
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
    let helper_binary = resolve_helper_binary(app, NativeHelper::Recorder);
    let configured_transport = std::env::var("CLONEAPROCESS_RECORDER_TRANSPORT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let configured_service_name = std::env::var("CLONEAPROCESS_RECORDER_XPC_SERVICE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let configured_label = std::env::var("CLONEAPROCESS_RECORDER_XPC_LABEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    resolve_recorder_transport_from_config(
        configured_transport.as_deref(),
        configured_service_name.as_deref(),
        bundled_recorder_service_exists(),
        dev_recorder_launch_agent_exists(
            configured_label.as_deref(),
            configured_service_name.as_deref(),
        ),
        helper_binary,
    )
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
        && service_root
            .join("Contents")
            .join("MacOS")
            .join("RecorderService")
            .exists()
}

fn dev_recorder_launch_agent_exists(
    configured_label: Option<&str>,
    configured_service_name: Option<&str>,
) -> bool {
    let Some(home_dir) = std::env::var_os("HOME").map(PathBuf::from) else {
        return false;
    };
    let label = configured_label
        .filter(|value| !value.trim().is_empty())
        .or(configured_service_name.filter(|value| !value.trim().is_empty()))
        .unwrap_or(DEV_RECORDER_XPC_SERVICE_NAME);

    home_dir
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", label))
        .exists()
}

fn resolve_recorder_transport_from_config(
    configured_transport: Option<&str>,
    configured_service_name: Option<&str>,
    bundled_service_exists: bool,
    dev_launch_agent_exists: bool,
    helper_binary: PathBuf,
) -> RecorderTransportConfig {
    match configured_transport.map(str::trim) {
        Some("xpc_bundle_service") => RecorderTransportConfig::xpc_bundle_service(
            configured_service_name
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(BUNDLED_RECORDER_XPC_SERVICE_NAME)
                .to_string(),
        ),
        Some("xpc") | Some("xpc_service") => RecorderTransportConfig::xpc_service(
            configured_service_name
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(DEV_RECORDER_XPC_SERVICE_NAME)
                .to_string(),
        ),
        Some("subprocess_bridge") | Some("bridge") => {
            RecorderTransportConfig::subprocess_bridge(helper_binary)
        }
        _ if bundled_service_exists => RecorderTransportConfig::xpc_bundle_service(
            configured_service_name
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(BUNDLED_RECORDER_XPC_SERVICE_NAME)
                .to_string(),
        ),
        _ if dev_launch_agent_exists => RecorderTransportConfig::xpc_service(
            configured_service_name
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(DEV_RECORDER_XPC_SERVICE_NAME)
                .to_string(),
        ),
        _ => RecorderTransportConfig::subprocess_bridge(helper_binary),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        resolve_recorder_transport_from_config, RecorderTransportConfig,
        BUNDLED_RECORDER_XPC_SERVICE_NAME, DEV_RECORDER_XPC_SERVICE_NAME,
    };
    use std::path::PathBuf;

    fn helper_binary() -> PathBuf {
        PathBuf::from("/tmp/RecorderService")
    }

    #[test]
    fn explicit_bundled_transport_wins() {
        let transport = resolve_recorder_transport_from_config(
            Some("xpc_bundle_service"),
            None,
            false,
            false,
            helper_binary(),
        );

        assert_eq!(
            transport,
            RecorderTransportConfig::xpc_bundle_service(
                BUNDLED_RECORDER_XPC_SERVICE_NAME.to_string()
            )
        );
    }

    #[test]
    fn explicit_mach_transport_uses_dev_service_default() {
        let transport = resolve_recorder_transport_from_config(
            Some("xpc_service"),
            None,
            false,
            false,
            helper_binary(),
        );

        assert_eq!(
            transport,
            RecorderTransportConfig::xpc_service(DEV_RECORDER_XPC_SERVICE_NAME.to_string())
        );
    }

    #[test]
    fn explicit_subprocess_transport_can_override_xpc_defaults() {
        let helper_binary = helper_binary();
        let transport = resolve_recorder_transport_from_config(
            Some("subprocess_bridge"),
            None,
            true,
            true,
            helper_binary.clone(),
        );

        assert_eq!(
            transport,
            RecorderTransportConfig::subprocess_bridge(helper_binary)
        );
    }

    #[test]
    fn bundled_service_is_preferred_when_available() {
        let transport =
            resolve_recorder_transport_from_config(None, None, true, true, helper_binary());

        assert_eq!(
            transport,
            RecorderTransportConfig::xpc_bundle_service(
                BUNDLED_RECORDER_XPC_SERVICE_NAME.to_string()
            )
        );
    }

    #[test]
    fn bootstrapped_dev_launch_agent_prefers_mach_service() {
        let transport =
            resolve_recorder_transport_from_config(None, None, false, true, helper_binary());

        assert_eq!(
            transport,
            RecorderTransportConfig::xpc_service(DEV_RECORDER_XPC_SERVICE_NAME.to_string())
        );
    }

    #[test]
    fn subprocess_bridge_remains_fallback_without_xpc_endpoints() {
        let helper_binary = helper_binary();
        let transport =
            resolve_recorder_transport_from_config(None, None, false, false, helper_binary.clone());

        assert_eq!(
            transport,
            RecorderTransportConfig::subprocess_bridge(helper_binary)
        );
    }

    #[test]
    fn configured_service_name_applies_to_dev_mach_selection() {
        let transport = resolve_recorder_transport_from_config(
            None,
            Some("com.cloneaprocess.recorder.custom"),
            false,
            true,
            helper_binary(),
        );

        assert_eq!(
            transport,
            RecorderTransportConfig::xpc_service("com.cloneaprocess.recorder.custom".to_string())
        );
    }
}
