use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::State;

use crate::core::app_state::AppState;
use crate::core::retention::{run_retention_cleanup, RetentionCleanupReport};
use crate::core::trace::normalize_raw_event;
use crate::storage::{
    NewRawEvent, NewSession, RawEventRecord, RetentionPolicy, SessionRecord, WorkflowRunLogRecord,
    WorkflowRunRecord,
};

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
    app_transition_count: i64,
    ax_snapshot_count: i64,
    keyframe_count: i64,
    last_error: Option<String>,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunSummary {
    id: i64,
    external_id: String,
    workflow_id: String,
    workflow_name: String,
    source_session_id: Option<i64>,
    status: String,
    started_at_ms: u64,
    ended_at_ms: Option<u64>,
    step_count: i64,
    completed_step_count: i64,
    failed_step_index: Option<i64>,
    last_error: Option<String>,
    created_at_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunLog {
    id: i64,
    workflow_run_id: i64,
    sequence: i64,
    step_index: Option<i64>,
    event_type: String,
    payload_json: String,
    recorded_at_ms: u64,
    created_at_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionPolicyResponse {
    max_completed_sessions: u32,
    max_session_age_days: u32,
    orphan_grace_hours: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetentionCleanupResponse {
    policy: RetentionPolicyResponse,
    retained_session_count: usize,
    pruned_session_count: usize,
    deleted_keyframe_file_count: usize,
    deleted_session_directory_count: usize,
    deleted_orphan_directory_count: usize,
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

    let normalized_event = normalize_raw_event(
        Some("storage_smoke_test"),
        &serde_json::json!({ "ok": true }),
        now_ms(),
    )
    .map_err(|error| error.to_string())?;

    let event_id = state
        .storage()
        .insert_raw_event(&NewRawEvent {
            session_id,
            sequence: 0,
            event_type: normalized_event.event_type,
            event_json: normalized_event.event_json,
            recorded_at_ms: normalized_event.recorded_at_ms,
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
pub fn list_sessions(
    state: State<'_, AppState>,
    limit: Option<i64>,
) -> Result<Vec<SessionSummary>, String> {
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

#[tauri::command]
pub fn list_workflow_runs(
    state: State<'_, AppState>,
    limit: Option<i64>,
) -> Result<Vec<WorkflowRunSummary>, String> {
    let rows = state
        .storage()
        .list_workflow_runs(limit.unwrap_or(20))
        .map_err(|error| error.to_string())?;

    Ok(rows.into_iter().map(map_workflow_run).collect())
}

#[tauri::command]
pub fn list_workflow_run_logs(
    state: State<'_, AppState>,
    workflow_run_id: i64,
    limit: Option<i64>,
) -> Result<Vec<WorkflowRunLog>, String> {
    let rows = state
        .storage()
        .list_workflow_run_logs(workflow_run_id, limit.unwrap_or(200))
        .map_err(|error| error.to_string())?;

    Ok(rows.into_iter().map(map_workflow_run_log).collect())
}

#[tauri::command]
pub fn get_retention_policy(state: State<'_, AppState>) -> Result<RetentionPolicyResponse, String> {
    state
        .storage()
        .retention_policy()
        .map(map_retention_policy)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn update_retention_policy(
    state: State<'_, AppState>,
    max_completed_sessions: u32,
    max_session_age_days: u32,
    orphan_grace_hours: u32,
) -> Result<RetentionPolicyResponse, String> {
    let policy = RetentionPolicy {
        max_completed_sessions,
        max_session_age_days,
        orphan_grace_hours,
    };
    state
        .storage()
        .update_retention_policy(&policy)
        .map_err(|error| error.to_string())?;
    Ok(map_retention_policy(policy))
}

#[tauri::command]
pub fn run_retention_cleanup_now(
    state: State<'_, AppState>,
) -> Result<RetentionCleanupResponse, String> {
    run_retention_cleanup(state.storage(), state.recordings_root())
        .map(map_retention_cleanup_report)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn load_keyframe_bytes(path: String) -> Result<Vec<u8>, String> {
    fs::read(&path).map_err(|error| format!("failed to read keyframe {}: {}", path, error))
}

fn map_session(row: SessionRecord) -> SessionSummary {
    SessionSummary {
        id: row.id,
        external_id: row.external_id,
        label: row.label,
        started_at_ms: row.started_at_ms,
        ended_at_ms: row.ended_at_ms,
        status: row.status,
        app_transition_count: row.app_transition_count,
        ax_snapshot_count: row.ax_snapshot_count,
        keyframe_count: row.keyframe_count_cached,
        last_error: row.last_error,
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

fn map_workflow_run(row: WorkflowRunRecord) -> WorkflowRunSummary {
    WorkflowRunSummary {
        id: row.id,
        external_id: row.external_id,
        workflow_id: row.workflow_id,
        workflow_name: row.workflow_name,
        source_session_id: row.source_session_id,
        status: row.status,
        started_at_ms: row.started_at_ms,
        ended_at_ms: row.ended_at_ms,
        step_count: row.step_count,
        completed_step_count: row.completed_step_count,
        failed_step_index: row.failed_step_index,
        last_error: row.last_error,
        created_at_ms: row.created_at_ms,
    }
}

fn map_workflow_run_log(row: WorkflowRunLogRecord) -> WorkflowRunLog {
    WorkflowRunLog {
        id: row.id,
        workflow_run_id: row.workflow_run_id,
        sequence: row.sequence,
        step_index: row.step_index,
        event_type: row.event_type,
        payload_json: row.payload_json,
        recorded_at_ms: row.recorded_at_ms,
        created_at_ms: row.created_at_ms,
    }
}

fn map_retention_policy(policy: RetentionPolicy) -> RetentionPolicyResponse {
    RetentionPolicyResponse {
        max_completed_sessions: policy.max_completed_sessions,
        max_session_age_days: policy.max_session_age_days,
        orphan_grace_hours: policy.orphan_grace_hours,
    }
}

fn map_retention_cleanup_report(report: RetentionCleanupReport) -> RetentionCleanupResponse {
    RetentionCleanupResponse {
        policy: map_retention_policy(report.policy),
        retained_session_count: report.retained_session_count,
        pruned_session_count: report.pruned_session_count,
        deleted_keyframe_file_count: report.deleted_keyframe_file_count,
        deleted_session_directory_count: report.deleted_session_directory_count,
        deleted_orphan_directory_count: report.deleted_orphan_directory_count,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
