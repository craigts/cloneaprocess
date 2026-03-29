use serde::Serialize;
use tauri::State;

use crate::core::app_state::AppState;
use crate::workflow;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDraftResponse {
    workflow_json: String,
    step_count: usize,
}

#[tauri::command]
pub fn compile_workflow_preview(
    state: State<'_, AppState>,
    session_id: i64,
) -> Result<WorkflowDraftResponse, String> {
    let events = state
        .storage()
        .list_raw_events_for_session(session_id, 500)
        .map_err(|error| error.to_string())?;
    let session = state
        .storage()
        .list_sessions(100)
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|row| row.id == session_id)
        .ok_or_else(|| format!("session {} not found", session_id))?;

    let draft = workflow::compile_workflow(session_id, session.label.unwrap_or(session.external_id), &events)?;

    Ok(WorkflowDraftResponse {
        workflow_json: serde_json::to_string_pretty(&draft.workflow).map_err(|error| error.to_string())?,
        step_count: draft.step_count,
    })
}
