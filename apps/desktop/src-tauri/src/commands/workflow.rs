use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;

use crate::core::app_state::AppState;

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

    let mut steps: Vec<Value> = Vec::new();
    for event in events {
        if event.event_type != "ax_snapshot" {
            continue;
        }

        let envelope: Value = serde_json::from_str(&event.event_json).map_err(|error| error.to_string())?;
        let payload = envelope
            .get("payload")
            .and_then(Value::as_object)
            .ok_or_else(|| "ax_snapshot payload missing".to_string())?;

        let selector = payload
            .get("selector")
            .cloned()
            .unwrap_or_else(|| json!({}));

        steps.push(json!({
            "kind": "click",
            "selector": selector,
        }));
    }

    let workflow = json!({
        "id": format!("wf_session_{}", session_id),
        "name": session.label.unwrap_or(session.external_id),
        "inputs": [],
        "steps": steps,
    });

    Ok(WorkflowDraftResponse {
        workflow_json: serde_json::to_string_pretty(&workflow).map_err(|error| error.to_string())?,
        step_count: workflow
            .get("steps")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or(0),
    })
}
