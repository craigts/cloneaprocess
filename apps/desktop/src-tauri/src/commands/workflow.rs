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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowExecutionResponse {
    run_row_id: i64,
    run_external_id: String,
    workflow_id: String,
    workflow_name: String,
    status: String,
    step_count: usize,
    completed_step_count: usize,
    failed_step_index: Option<usize>,
    last_error: Option<String>,
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
        .get_session(session_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("session {} not found", session_id))?;

    let draft = workflow::compile_workflow(
        session_id,
        session.label.unwrap_or(session.external_id),
        &events,
    )?;

    Ok(WorkflowDraftResponse {
        workflow_json: serde_json::to_string_pretty(&draft.workflow)
            .map_err(|error| error.to_string())?,
        step_count: draft.step_count,
    })
}

#[tauri::command]
pub fn execute_session_workflow(
    state: State<'_, AppState>,
    session_id: i64,
) -> Result<WorkflowExecutionResponse, String> {
    let events = state
        .storage()
        .list_raw_events_for_session(session_id, 500)
        .map_err(|error| error.to_string())?;
    let session = state
        .storage()
        .get_session(session_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("session {} not found", session_id))?;
    let draft = workflow::compile_workflow(
        session_id,
        session.label.unwrap_or(session.external_id),
        &events,
    )?;
    let summary = workflow::execute_workflow(
        state.storage(),
        state.runner_binary(),
        &draft.workflow,
        Some(session_id),
    )?;

    Ok(WorkflowExecutionResponse {
        run_row_id: summary.run_row_id,
        run_external_id: summary.run_external_id,
        workflow_id: summary.workflow_id,
        workflow_name: summary.workflow_name,
        status: summary.status,
        step_count: summary.step_count,
        completed_step_count: summary.completed_step_count,
        failed_step_index: summary.failed_step_index,
        last_error: summary.last_error,
    })
}

#[tauri::command]
pub fn execute_workflow_json(
    state: State<'_, AppState>,
    workflow_json: String,
    source_session_id: Option<i64>,
) -> Result<WorkflowExecutionResponse, String> {
    let workflow: serde_json::Value = serde_json::from_str(&workflow_json)
        .map_err(|error| format!("invalid workflow JSON: {error}"))?;
    let summary = workflow::execute_workflow(
        state.storage(),
        state.runner_binary(),
        &workflow,
        source_session_id,
    )?;

    Ok(WorkflowExecutionResponse {
        run_row_id: summary.run_row_id,
        run_external_id: summary.run_external_id,
        workflow_id: summary.workflow_id,
        workflow_name: summary.workflow_name,
        status: summary.status,
        step_count: summary.step_count,
        completed_step_count: summary.completed_step_count,
        failed_step_index: summary.failed_step_index,
        last_error: summary.last_error,
    })
}

#[tauri::command]
pub fn approve_workflow_run(
    state: State<'_, AppState>,
    workflow_run_id: i64,
) -> Result<WorkflowExecutionResponse, String> {
    let summary =
        workflow::approve_workflow_run(state.storage(), state.runner_binary(), workflow_run_id)?;

    Ok(WorkflowExecutionResponse {
        run_row_id: summary.run_row_id,
        run_external_id: summary.run_external_id,
        workflow_id: summary.workflow_id,
        workflow_name: summary.workflow_name,
        status: summary.status,
        step_count: summary.step_count,
        completed_step_count: summary.completed_step_count,
        failed_step_index: summary.failed_step_index,
        last_error: summary.last_error,
    })
}

#[tauri::command]
pub fn reject_workflow_run(
    state: State<'_, AppState>,
    workflow_run_id: i64,
) -> Result<WorkflowExecutionResponse, String> {
    let summary = workflow::reject_workflow_run(state.storage(), workflow_run_id)?;

    Ok(WorkflowExecutionResponse {
        run_row_id: summary.run_row_id,
        run_external_id: summary.run_external_id,
        workflow_id: summary.workflow_id,
        workflow_name: summary.workflow_name,
        status: summary.status,
        step_count: summary.step_count,
        completed_step_count: summary.completed_step_count,
        failed_step_index: summary.failed_step_index,
        last_error: summary.last_error,
    })
}
