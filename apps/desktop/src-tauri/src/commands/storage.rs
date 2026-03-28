use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::State;

use crate::core::app_state::AppState;
use crate::storage::{NewRawEvent, NewSession, RawEventRecord, SessionRecord};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageSmokeResult {
    session_id: i64,
    event_id: i64,
    session_count: i64,
    raw_event_count: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    id: i64,
    external_id: String,
    label: Option<String>,
    started_at_ms: u64,
    ended_at_ms: Option<u64>,
    status: String,
    created_at_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineEvent {
    id: i64,
    session_id: i64,
    sequence: i64,
    event_type: String,
    event_json: String,
    recorded_at_ms: u64,
    created_at_ms: u64,
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

#[tauri::command]
pub fn list_sessions(state: State<'_, AppState>, limit: Option<i64>) -> Result<Vec<SessionSummary>, String> {
    let rows = state
        .storage()
        .list_sessions(limit.unwrap_or(20))
        .map_err(|error| error.to_string())?;

    Ok(rows.into_iter().map(map_session).collect())
}

#[tauri::command]
pub fn list_session_events(
    state: State<'_, AppState>,
    session_id: i64,
    limit: Option<i64>,
) -> Result<Vec<TimelineEvent>, String> {
    let rows = state
        .storage()
        .list_raw_events_for_session(session_id, limit.unwrap_or(500))
        .map_err(|error| error.to_string())?;

    Ok(rows.into_iter().map(map_event).collect())
}

fn map_session(row: SessionRecord) -> SessionSummary {
    SessionSummary {
        id: row.id,
        external_id: row.external_id,
        label: row.label,
        started_at_ms: row.started_at_ms,
        ended_at_ms: row.ended_at_ms,
        status: row.status,
        created_at_ms: row.created_at_ms,
    }
}

fn map_event(row: RawEventRecord) -> TimelineEvent {
    TimelineEvent {
        id: row.id,
        session_id: row.session_id,
        sequence: row.sequence,
        event_type: row.event_type,
        event_json: row.event_json,
        recorded_at_ms: row.recorded_at_ms,
        created_at_ms: row.created_at_ms,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
