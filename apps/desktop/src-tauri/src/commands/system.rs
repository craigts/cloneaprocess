use serde::Serialize;
use std::path::Path;
use tauri::State;

use crate::core::app_state::AppState;
use crate::core::recorder::RecorderTransportMode;
use crate::storage::StorageStatus;
use crate::workflow::WORKFLOW_IR_VERSION;

const EXPECTED_RECORDER_PROTOCOL_VERSION: u32 = 1;
const MINIMUM_RECORDER_PROTOCOL_VERSION: u32 = 1;
const REQUIRED_RECORDER_CAPABILITIES: &[&str] = &["event_stream", "permissions"];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemStatus {
    app_version: &'static str,
    platform: &'static str,
    recordings_root: String,
    database_path: String,
    started_at_ms: u64,
    session_count: i64,
    raw_event_count: i64,
    keyframe_count: i64,
    storage_schema_version: u32,
    workflow_ir_version: u32,
    recorder_binary: String,
    recorder_transport_mode: RecorderTransportMode,
    recorder_transport_target: String,
    recorder_transport_ready: bool,
    recorder_transport_error: Option<String>,
    recorder_protocol_version: Option<u32>,
    recorder_protocol_min: Option<u32>,
    recorder_protocol_capabilities: Vec<String>,
    recorder_protocol_compatible: bool,
    recorder_permissions: std::collections::BTreeMap<String, bool>,
    storage_ready: bool,
    recordings_root_ready: bool,
    recorder_binary_exists: bool,
    helper_health: &'static str,
}

#[tauri::command]
pub fn system_status(state: State<'_, AppState>) -> Result<SystemStatus, String> {
    let storage_status: StorageStatus = state.storage().status();
    let session_count = state
        .storage()
        .session_count()
        .map_err(|error| error.to_string())?;
    let raw_event_count = state
        .storage()
        .raw_event_count()
        .map_err(|error| error.to_string())?;
    let keyframe_count = state
        .storage()
        .keyframe_count()
        .map_err(|error| error.to_string())?;
    let recorder_status = state
        .recorder()
        .lock()
        .map_err(|_| "recorder mutex poisoned".to_string())?
        .status()
        .map_err(|error| error.to_string())?;
    let recorder_binary_exists = !recorder_status.recorder_binary.is_empty()
        && Path::new(&recorder_status.recorder_binary).exists();
    let storage_ready = storage_status
        .db_path
        .parent()
        .map(Path::exists)
        .unwrap_or(false);
    let recordings_root_ready = state.recordings_root().exists();
    let recorder_protocol_compatible = is_protocol_compatible(
        recorder_status.protocol_version,
        recorder_status.protocol_min,
        &recorder_status.protocol_capabilities,
    );
    let helper_health = match recorder_status.transport_mode {
        RecorderTransportMode::SubprocessBridge => {
            if !recorder_binary_exists {
                "missing_binary"
            } else if recorder_protocol_compatible {
                "ready"
            } else {
                "protocol_mismatch"
            }
        }
        RecorderTransportMode::XpcMachService | RecorderTransportMode::XpcBundledService => {
            if recorder_status.transport_ready {
                "ready"
            } else {
                "transport_unavailable"
            }
        }
    };

    Ok(SystemStatus {
        app_version: env!("CARGO_PKG_VERSION"),
        platform: std::env::consts::OS,
        recordings_root: state.recordings_root().display().to_string(),
        database_path: storage_status.db_path.display().to_string(),
        started_at_ms: state.started_at_ms(),
        session_count,
        raw_event_count,
        keyframe_count,
        storage_schema_version: storage_status.schema_version,
        workflow_ir_version: WORKFLOW_IR_VERSION,
        recorder_binary: recorder_status.recorder_binary,
        recorder_transport_mode: recorder_status.transport_mode,
        recorder_transport_target: recorder_status.transport_target,
        recorder_transport_ready: recorder_status.transport_ready,
        recorder_transport_error: recorder_status.transport_error,
        recorder_protocol_version: recorder_status.protocol_version,
        recorder_protocol_min: recorder_status.protocol_min,
        recorder_protocol_capabilities: recorder_status.protocol_capabilities,
        recorder_protocol_compatible,
        recorder_permissions: recorder_status.permissions,
        storage_ready,
        recordings_root_ready,
        recorder_binary_exists,
        helper_health,
    })
}

fn is_protocol_compatible(
    protocol_version: Option<u32>,
    protocol_min: Option<u32>,
    capabilities: &[String],
) -> bool {
    let Some(protocol_version) = protocol_version else {
        return false;
    };
    let Some(protocol_min) = protocol_min else {
        return false;
    };

    if protocol_version < MINIMUM_RECORDER_PROTOCOL_VERSION {
        return false;
    }
    if protocol_min > EXPECTED_RECORDER_PROTOCOL_VERSION {
        return false;
    }

    REQUIRED_RECORDER_CAPABILITIES
        .iter()
        .all(|required| capabilities.iter().any(|capability| capability == required))
}

#[cfg(test)]
mod tests {
    use super::is_protocol_compatible;

    #[test]
    fn protocol_is_compatible_for_expected_version_and_capabilities() {
        let capabilities = vec![
            "event_stream".to_string(),
            "permissions".to_string(),
            "ax_snapshot".to_string(),
        ];
        assert!(is_protocol_compatible(Some(1), Some(1), &capabilities));
    }

    #[test]
    fn protocol_is_incompatible_when_helper_requires_newer_version() {
        let capabilities = vec!["event_stream".to_string(), "permissions".to_string()];
        assert!(!is_protocol_compatible(Some(2), Some(2), &capabilities));
    }

    #[test]
    fn protocol_is_incompatible_when_required_capability_is_missing() {
        let capabilities = vec!["event_stream".to_string()];
        assert!(!is_protocol_compatible(Some(1), Some(1), &capabilities));
    }
}
