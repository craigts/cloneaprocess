use serde::Serialize;
use tauri::State;

use crate::core::app_state::AppState;
use crate::storage::StorageStatus;
use crate::workflow::WORKFLOW_IR_VERSION;

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
    storage_schema_version: u32,
    workflow_ir_version: u32,
    recorder_binary: String,
    recorder_permissions: std::collections::BTreeMap<String, bool>,
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
    let recorder_status = state
        .recorder()
        .lock()
        .map_err(|_| "recorder mutex poisoned".to_string())?
        .status()
        .map_err(|error| error.to_string())?;

    Ok(SystemStatus {
        app_version: env!("CARGO_PKG_VERSION"),
        platform: std::env::consts::OS,
        recordings_root: state.recordings_root().display().to_string(),
        database_path: storage_status.db_path.display().to_string(),
        started_at_ms: state.started_at_ms(),
        session_count,
        raw_event_count,
        storage_schema_version: storage_status.schema_version,
        workflow_ir_version: WORKFLOW_IR_VERSION,
        recorder_binary: recorder_status.recorder_binary,
        recorder_permissions: recorder_status.permissions,
    })
}
