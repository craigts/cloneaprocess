use serde::Serialize;
use tauri::State;

use crate::core::app_state::AppState;
use crate::core::recorder::RecorderStatus;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecorderStatusResponse {
    active: bool,
    session_external_id: Option<String>,
    session_row_id: Option<i64>,
    event_count: i64,
    frame_count: i64,
    permissions: std::collections::BTreeMap<String, bool>,
    recorder_binary: String,
}

#[tauri::command]
pub fn recorder_status(state: State<'_, AppState>) -> Result<RecorderStatusResponse, String> {
    let status = state
        .recorder()
        .lock()
        .map_err(|_| "recorder mutex poisoned".to_string())?
        .status()
        .map_err(|error| error.to_string())?;

    Ok(map_status(status))
}

#[tauri::command]
pub fn start_recording(state: State<'_, AppState>) -> Result<RecorderStatusResponse, String> {
    let status = state
        .recorder()
        .lock()
        .map_err(|_| "recorder mutex poisoned".to_string())?
        .start_capture()
        .map_err(|error| error.to_string())?;

    Ok(map_status(status))
}

#[tauri::command]
pub fn stop_recording(state: State<'_, AppState>) -> Result<RecorderStatusResponse, String> {
    let status = state
        .recorder()
        .lock()
        .map_err(|_| "recorder mutex poisoned".to_string())?
        .stop_capture()
        .map_err(|error| error.to_string())?;

    Ok(map_status(status))
}

fn map_status(status: RecorderStatus) -> RecorderStatusResponse {
    RecorderStatusResponse {
        active: status.active,
        session_external_id: status.session_external_id,
        session_row_id: status.session_row_id,
        event_count: status.event_count,
        frame_count: status.frame_count,
        permissions: status.permissions,
        recorder_binary: status.recorder_binary,
    }
}
