use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::State;

use crate::core::app_state::AppState;
use crate::storage::{NewRawEvent, NewSession};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageSmokeResult {
    session_id: i64,
    event_id: i64,
    session_count: i64,
    raw_event_count: i64,
}

#[tauri::command]
pub fn storage_smoke_test(state: State<'_, AppState>) -> Result<StorageSmokeResult, String> {
    let started_at_ms = now_ms();
    let session_id = state
        .storage()
        .insert_session(&NewSession {
            external_id: format!("smoke-{}", started_at_ms),
            label: Some("Storage smoke test".to_string()),
            started_at_ms,
            status: "recording".to_string(),
        })
        .map_err(|error| error.to_string())?;

    let event_id = state
        .storage()
        .insert_raw_event(&NewRawEvent {
            session_id,
            sequence: 0,
            event_type: "storage_smoke_test".to_string(),
            event_json: r#"{"ok":true}"#.to_string(),
            recorded_at_ms: now_ms(),
        })
        .map_err(|error| error.to_string())?;

    Ok(StorageSmokeResult {
        session_id,
        event_id,
        session_count: state
            .storage()
            .session_count()
            .map_err(|error| error.to_string())?,
        raw_event_count: state
            .storage()
            .raw_event_count()
            .map_err(|error| error.to_string())?,
    })
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
