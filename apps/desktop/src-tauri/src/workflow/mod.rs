use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::core::runner::{RunnerBridge, RunnerError, RunnerStepExecutor, RunnerStepRequest};
use crate::storage::{
    NewWorkflowRun, NewWorkflowRunLog, RawEventRecord, Storage, WorkflowRunRecord,
};

pub const WORKFLOW_IR_VERSION: u32 = 1;

const ELEMENT_WAIT_TIMEOUT_MS: u64 = 1500;
const DEFAULT_STEP_TIMEOUT_MS: u64 = 4_000;

#[derive(Clone, Debug)]
struct ApprovalRequest {
    category: &'static str,
    keyword: &'static str,
    summary: String,
    detail: String,
}

#[derive(Clone, Copy, Debug)]
enum ApprovalDecision {
    Approved,
}

#[derive(Clone, Debug)]
pub struct WorkflowDraft {
    pub workflow: Value,
    pub step_count: usize,
}

#[derive(Clone, Debug)]
pub struct WorkflowExecutionSummary {
    pub run_row_id: i64,
    pub run_external_id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    pub status: String,
    pub step_count: usize,
    pub completed_step_count: usize,
    pub failed_step_index: Option<usize>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug)]
struct DraftContext {
    last_bundle_id: Option<String>,
    pending_snapshot: Option<SnapshotContext>,
    active_text_entry: Option<TextEntryContext>,
    steps: Vec<Value>,
}

#[derive(Clone, Debug)]
struct SnapshotContext {
    selector: Value,
    role: Option<String>,
}

#[derive(Clone, Debug)]
struct TextEntryContext {
    selector: Value,
    value: String,
}

pub fn compile_workflow(
    session_id: i64,
    workflow_name: String,
    events: &[RawEventRecord],
) -> Result<WorkflowDraft, String> {
    let mut context = DraftContext {
        last_bundle_id: None,
        pending_snapshot: None,
        active_text_entry: None,
        steps: Vec::new(),
    };

    for event in events {
        let envelope: Value = serde_json::from_str(&event.event_json)
            .map_err(|error| format!("failed to parse event {}: {}", event.id, error))?;
        let payload = event_payload(&envelope);

        match event.event_type.as_str() {
            "frontmost_app_changed" => {
                flush_text_entry(&mut context);
                if let Some(bundle_id) = payload.get("bundleId").and_then(Value::as_str) {
                    if context.last_bundle_id.as_deref() != Some(bundle_id) {
                        context.steps.push(json!({
                            "kind": "focusWindow",
                            "bundleId": bundle_id,
                        }));
                        context.last_bundle_id = Some(bundle_id.to_string());
                    }
                }
            }
            "ax_snapshot" => {
                let selector = payload
                    .get("selector")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if selector != json!({}) {
                    context.pending_snapshot = Some(SnapshotContext {
                        selector,
                        role: payload
                            .get("role")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                    });
                }
            }
            "mouse_down" => {
                flush_text_entry(&mut context);
                if let Some(snapshot) = context.pending_snapshot.take() {
                    push_wait_for_element(&mut context.steps, &snapshot.selector);
                    context.steps.push(json!({
                        "kind": "click",
                        "selector": snapshot.selector.clone(),
                    }));

                    if is_editable_role(snapshot.role.as_deref()) {
                        context.active_text_entry = Some(TextEntryContext {
                            selector: snapshot.selector,
                            value: String::new(),
                        });
                    }
                }
            }
            "key_down" => {
                if let Some(text_entry) = context.active_text_entry.as_mut() {
                    let key_code =
                        payload.get("keyCode").and_then(Value::as_i64).unwrap_or(-1) as i32;
                    match translate_key_code(key_code) {
                        Some(KeyInput::Text(fragment)) => text_entry.value.push_str(fragment),
                        Some(KeyInput::Backspace) => {
                            text_entry.value.pop();
                        }
                        Some(KeyInput::Submit) | None => flush_text_entry(&mut context),
                    }
                }
            }
            _ => {}
        }
    }

    flush_text_entry(&mut context);

    let workflow = json!({
        "id": format!("wf_session_{}", session_id),
        "name": workflow_name,
        "inputs": [],
        "steps": context.steps,
    });
    let step_count = workflow
        .get("steps")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);

    Ok(WorkflowDraft {
        workflow,
        step_count,
    })
}

fn event_payload(envelope: &Value) -> serde_json::Map<String, Value> {
    if let Some(payload) = envelope.get("payload").and_then(Value::as_object) {
        return payload.clone();
    }

    serde_json::Map::new()
}

pub fn execute_workflow(
    storage: &Storage,
    runner_binary: &Path,
    workflow: &Value,
    source_session_id: Option<i64>,
) -> Result<WorkflowExecutionSummary, String> {
    let mut runner = RunnerBridge::spawn(runner_binary).map_err(|error| error.to_string())?;
    execute_workflow_with_runner(storage, &mut runner, workflow, source_session_id)
}

fn execute_workflow_with_runner<R: RunnerStepExecutor>(
    storage: &Storage,
    runner: &mut R,
    workflow: &Value,
    source_session_id: Option<i64>,
) -> Result<WorkflowExecutionSummary, String> {
    let workflow_id = workflow
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("wf_unknown")
        .to_string();
    let workflow_name = workflow
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Untitled Workflow")
        .to_string();
    let steps = workflow
        .get("steps")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| "workflow steps must be an array".to_string())?;

    let started_at_ms = now_ms();
    let run_external_id = unique_external_id("run");
    let run_row_id = storage
        .insert_workflow_run(&NewWorkflowRun {
            external_id: run_external_id.clone(),
            workflow_id: workflow_id.clone(),
            workflow_name: workflow_name.clone(),
            source_session_id,
            workflow_json: serde_json::to_string(workflow).map_err(|error| error.to_string())?,
            status: "running".to_string(),
            started_at_ms,
            step_count: steps.len() as i64,
        })
        .map_err(|error| error.to_string())?;
    continue_workflow_execution(
        storage,
        runner,
        ExecutionState {
            run_row_id,
            run_external_id,
            workflow_id,
            workflow_name,
            steps,
            completed_step_count: 0,
            sequence: 0,
            resume_step_index: 0,
            approval_decision: None,
        },
    )
}

pub fn approve_workflow_run(
    storage: &Storage,
    runner_binary: &Path,
    workflow_run_id: i64,
) -> Result<WorkflowExecutionSummary, String> {
    let run = storage
        .get_workflow_run(workflow_run_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("workflow run {} not found", workflow_run_id))?;
    let state = resumable_execution_state(storage, run, ApprovalDecision::Approved)?;
    let mut runner = RunnerBridge::spawn(runner_binary).map_err(|error| error.to_string())?;
    continue_workflow_execution(storage, &mut runner, state)
}

pub fn reject_workflow_run(
    storage: &Storage,
    workflow_run_id: i64,
) -> Result<WorkflowExecutionSummary, String> {
    let run = storage
        .get_workflow_run(workflow_run_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("workflow run {} not found", workflow_run_id))?;
    if run.status != "awaiting_approval" {
        return Err(format!(
            "workflow run {} is not awaiting approval",
            workflow_run_id
        ));
    }
    let step_index = run
        .failed_step_index
        .ok_or_else(|| "awaiting approval run is missing failed step index".to_string())?;
    let mut sequence = storage
        .next_workflow_run_log_sequence(workflow_run_id)
        .map_err(|error| error.to_string())?;
    let rejection_message = format!("approval rejected for risky step {}", step_index);
    append_run_log(
        storage,
        workflow_run_id,
        &mut sequence,
        Some(step_index),
        "approval_rejected",
        json!({
            "step_index": step_index,
            "message": rejection_message,
        }),
    )?;
    append_run_log(
        storage,
        workflow_run_id,
        &mut sequence,
        None,
        "run_rejected",
        json!({
            "failed_step_index": step_index,
            "message": rejection_message,
        }),
    )?;
    storage
        .update_workflow_run_state(
            workflow_run_id,
            "rejected",
            Some(now_ms()),
            run.completed_step_count,
            Some(step_index),
            Some(rejection_message.as_str()),
        )
        .map_err(|error| error.to_string())?;

    Ok(WorkflowExecutionSummary {
        run_row_id: run.id,
        run_external_id: run.external_id,
        workflow_id: run.workflow_id,
        workflow_name: run.workflow_name,
        status: "rejected".to_string(),
        step_count: run.step_count as usize,
        completed_step_count: run.completed_step_count as usize,
        failed_step_index: Some(step_index as usize),
        last_error: Some(rejection_message),
    })
}

#[derive(Clone, Debug)]
struct ExecutionState {
    run_row_id: i64,
    run_external_id: String,
    workflow_id: String,
    workflow_name: String,
    steps: Vec<Value>,
    completed_step_count: usize,
    sequence: i64,
    resume_step_index: usize,
    approval_decision: Option<ApprovalDecision>,
}

fn resumable_execution_state(
    storage: &Storage,
    run: WorkflowRunRecord,
    approval_decision: ApprovalDecision,
) -> Result<ExecutionState, String> {
    if run.status != "awaiting_approval" {
        return Err(format!("workflow run {} is not awaiting approval", run.id));
    }
    let resume_step_index = run
        .failed_step_index
        .ok_or_else(|| "awaiting approval run is missing failed step index".to_string())?
        as usize;
    let workflow: Value = serde_json::from_str(&run.workflow_json)
        .map_err(|error| format!("invalid workflow json: {}", error))?;
    let steps = workflow
        .get("steps")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| "workflow steps must be an array".to_string())?;

    Ok(ExecutionState {
        run_row_id: run.id,
        run_external_id: run.external_id,
        workflow_id: run.workflow_id,
        workflow_name: run.workflow_name,
        steps,
        completed_step_count: run.completed_step_count as usize,
        sequence: storage
            .next_workflow_run_log_sequence(run.id)
            .map_err(|error| error.to_string())?,
        resume_step_index,
        approval_decision: Some(approval_decision),
    })
}

fn continue_workflow_execution<R: RunnerStepExecutor>(
    storage: &Storage,
    runner: &mut R,
    mut state: ExecutionState,
) -> Result<WorkflowExecutionSummary, String> {
    if let Some(ApprovalDecision::Approved) = state.approval_decision {
        append_run_log(
            storage,
            state.run_row_id,
            &mut state.sequence,
            Some(state.resume_step_index as i64),
            "approval_approved",
            json!({
                "step_index": state.resume_step_index,
                "message": format!("approval granted for step {}", state.resume_step_index),
            }),
        )?;
        storage
            .update_workflow_run_state(
                state.run_row_id,
                "running",
                None,
                state.completed_step_count as i64,
                None,
                None,
            )
            .map_err(|error| error.to_string())?;
    }

    for step_index in state.resume_step_index..state.steps.len() {
        let step = &state.steps[step_index];
        if let Some(request) = approval_request_for_step(step) {
            let is_preapproved =
                matches!(state.approval_decision, Some(ApprovalDecision::Approved))
                    && step_index == state.resume_step_index;

            if !is_preapproved {
                append_run_log(
                    storage,
                    state.run_row_id,
                    &mut state.sequence,
                    Some(step_index as i64),
                    "approval_requested",
                    json!({
                        "step_index": step_index,
                        "category": request.category,
                        "keyword": request.keyword,
                        "summary": request.summary,
                        "detail": request.detail,
                        "step": step,
                    }),
                )?;
                storage
                    .update_workflow_run_state(
                        state.run_row_id,
                        "awaiting_approval",
                        None,
                        state.completed_step_count as i64,
                        Some(step_index as i64),
                        Some(request.summary.as_str()),
                    )
                    .map_err(|error| error.to_string())?;

                return Ok(WorkflowExecutionSummary {
                    run_row_id: state.run_row_id,
                    run_external_id: state.run_external_id,
                    workflow_id: state.workflow_id,
                    workflow_name: state.workflow_name,
                    status: "awaiting_approval".to_string(),
                    step_count: state.steps.len(),
                    completed_step_count: state.completed_step_count,
                    failed_step_index: Some(step_index),
                    last_error: Some(request.summary),
                });
            }
        }

        let kind = step_kind(step)?.to_string();
        let max_attempts = step_retry_attempts(step);
        let timeout = Duration::from_millis(step_timeout_ms(step));
        let mut last_step_error = None;

        for attempt in 1..=max_attempts {
            append_run_log(
                storage,
                state.run_row_id,
                &mut state.sequence,
                Some(step_index as i64),
                "step_attempt_started",
                json!({
                    "kind": kind,
                    "attempt": attempt,
                    "timeout_ms": timeout.as_millis(),
                    "step": step,
                }),
            )?;

            let outcome = if kind == "waitFor" {
                execute_wait_for_step(step)
            } else {
                runner
                    .execute_step(
                        &RunnerStepRequest {
                            workflow_id: state.workflow_id.clone(),
                            outer_run_id: state.run_external_id.clone(),
                            step_index,
                            attempt,
                            step: step.clone(),
                        },
                        timeout,
                    )
                    .map(|value| value.result)
            };

            match outcome {
                Ok(result) => {
                    state.completed_step_count += 1;
                    append_run_log(
                        storage,
                        state.run_row_id,
                        &mut state.sequence,
                        Some(step_index as i64),
                        "step_finished",
                        json!({
                            "kind": kind,
                            "attempt": attempt,
                            "ok": true,
                            "result": result,
                        }),
                    )?;
                    last_step_error = None;
                    break;
                }
                Err(error) => {
                    let retryable = error.is_retryable();
                    let error_payload = runner_error_payload(&error);
                    append_run_log(
                        storage,
                        state.run_row_id,
                        &mut state.sequence,
                        Some(step_index as i64),
                        "step_finished",
                        json!({
                            "kind": kind,
                            "attempt": attempt,
                            "ok": false,
                            "error": error_payload,
                        }),
                    )?;
                    last_step_error = Some(error.to_string());

                    if attempt < max_attempts && retryable {
                        append_run_log(
                            storage,
                            state.run_row_id,
                            &mut state.sequence,
                            Some(step_index as i64),
                            "step_retry_scheduled",
                            json!({
                                "kind": kind,
                                "attempt": attempt,
                                "next_attempt": attempt + 1,
                                "retryable": true,
                            }),
                        )?;
                        continue;
                    }
                    break;
                }
            }
        }

        if let Some(error) = last_step_error.take() {
            append_run_log(
                storage,
                state.run_row_id,
                &mut state.sequence,
                None,
                "run_failed",
                json!({
                    "failed_step_index": step_index,
                    "message": error,
                }),
            )?;
            storage
                .update_workflow_run_state(
                    state.run_row_id,
                    "failed",
                    Some(now_ms()),
                    state.completed_step_count as i64,
                    Some(step_index as i64),
                    Some(error.as_str()),
                )
                .map_err(|error| error.to_string())?;

            return Ok(WorkflowExecutionSummary {
                run_row_id: state.run_row_id,
                run_external_id: state.run_external_id,
                workflow_id: state.workflow_id,
                workflow_name: state.workflow_name,
                status: "failed".to_string(),
                step_count: state.steps.len(),
                completed_step_count: state.completed_step_count,
                failed_step_index: Some(step_index),
                last_error: Some(error),
            });
        }
    }

    append_run_log(
        storage,
        state.run_row_id,
        &mut state.sequence,
        None,
        "run_completed",
        json!({
            "completed_step_count": state.completed_step_count,
            "step_count": state.steps.len(),
        }),
    )?;
    storage
        .update_workflow_run_state(
            state.run_row_id,
            "completed",
            Some(now_ms()),
            state.completed_step_count as i64,
            None,
            None,
        )
        .map_err(|error| error.to_string())?;

    Ok(WorkflowExecutionSummary {
        run_row_id: state.run_row_id,
        run_external_id: state.run_external_id,
        workflow_id: state.workflow_id,
        workflow_name: state.workflow_name,
        status: "completed".to_string(),
        step_count: state.steps.len(),
        completed_step_count: state.completed_step_count,
        failed_step_index: None,
        last_error: None,
    })
}

fn execute_wait_for_step(step: &Value) -> Result<Value, RunnerError> {
    let timeout_ms = step
        .get("timeoutMs")
        .and_then(Value::as_u64)
        .unwrap_or(ELEMENT_WAIT_TIMEOUT_MS);
    Ok(json!({
        "action": "waitFor",
        "status": "skipped",
        "reason": "verification_not_implemented",
        "timeoutMs": timeout_ms,
        "condition": step.get("condition").cloned().unwrap_or_else(|| json!({})),
    }))
}

fn approval_request_for_step(step: &Value) -> Option<ApprovalRequest> {
    match step_kind(step).ok()? {
        "click" => risky_phrase_from_selector(step.get("selector")?).map(|risk| ApprovalRequest {
            category: risk.0,
            keyword: risk.1,
            summary: format!("approval required before click on {}", risk.2),
            detail: format!("The click target contains the risky keyword '{}'.", risk.1),
        }),
        "selectMenu" => {
            let path = step
                .get("path")
                .and_then(Value::as_array)?
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            let joined = path.join(" > ");
            risky_phrase(&joined).map(|risk| ApprovalRequest {
                category: risk.0,
                keyword: risk.1,
                summary: format!("approval required before menu action {}", joined),
                detail: format!(
                    "The menu path '{}' contains the risky keyword '{}'.",
                    joined, risk.1
                ),
            })
        }
        _ => None,
    }
}

fn risky_phrase_from_selector(selector: &Value) -> Option<(&'static str, &'static str, String)> {
    let ax = selector.get("ax")?;
    for field in ["title", "description", "valueHint", "identifier"] {
        let value = ax.get(field).and_then(Value::as_str)?;
        if let Some((category, keyword)) = risky_phrase(value) {
            return Some((
                category,
                keyword,
                format!("selector {} \"{}\"", field, value),
            ));
        }
    }
    None
}

fn risky_phrase(value: &str) -> Option<(&'static str, &'static str)> {
    let normalized = value.to_ascii_lowercase();
    const RISKY_KEYWORDS: [(&str, &[&str]); 4] = [
        ("send", &["send", "publish", "post"]),
        ("delete", &["delete", "remove", "trash", "discard"]),
        ("purchase", &["buy", "purchase", "checkout", "pay", "order"]),
        ("commit", &["transfer", "wire", "confirm", "submit"]),
    ];

    for (category, keywords) in RISKY_KEYWORDS {
        for keyword in keywords {
            if normalized.contains(keyword) {
                return Some((category, keyword));
            }
        }
    }

    None
}

fn append_run_log(
    storage: &Storage,
    workflow_run_id: i64,
    sequence: &mut i64,
    step_index: Option<i64>,
    event_type: &str,
    payload: Value,
) -> Result<(), String> {
    let payload_json = serde_json::to_string(&payload).map_err(|error| error.to_string())?;
    storage
        .append_workflow_run_log(&NewWorkflowRunLog {
            workflow_run_id,
            sequence: *sequence,
            step_index,
            event_type: event_type.to_string(),
            payload_json,
            recorded_at_ms: now_ms(),
        })
        .map_err(|error| error.to_string())?;
    *sequence += 1;
    Ok(())
}

fn step_kind(step: &Value) -> Result<&str, String> {
    step.get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| "workflow step kind is required".to_string())
}

fn step_timeout_ms(step: &Value) -> u64 {
    match step_kind(step).unwrap_or_default() {
        "waitFor" => step
            .get("timeoutMs")
            .and_then(Value::as_u64)
            .unwrap_or(ELEMENT_WAIT_TIMEOUT_MS),
        _ => step
            .get("timeoutMs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_STEP_TIMEOUT_MS),
    }
}

fn step_retry_attempts(step: &Value) -> u32 {
    step.get("retry")
        .and_then(|value| value.get("maxAttempts"))
        .and_then(Value::as_u64)
        .map(|value| value.clamp(1, 5) as u32)
        .unwrap_or(1)
}

fn runner_error_payload(error: &RunnerError) -> Value {
    match error {
        RunnerError::Remote {
            code,
            message,
            retryable,
        } => json!({
            "code": code,
            "message": message,
            "retryable": retryable,
        }),
        RunnerError::Timeout {
            operation,
            stderr_tail,
        } => json!({
            "code": "TIMEOUT",
            "message": format!("{} timed out", operation),
            "stderr_tail": stderr_tail,
            "retryable": true,
        }),
        _ => json!({
            "code": "EXECUTOR_ERROR",
            "message": error.to_string(),
            "retryable": error.is_retryable(),
        }),
    }
}

fn unique_external_id(prefix: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{}_{}_{}", prefix, std::process::id(), now)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn push_wait_for_element(steps: &mut Vec<Value>, selector: &Value) {
    steps.push(json!({
        "kind": "waitFor",
        "condition": {
            "kind": "elementPresent",
            "selector": selector,
        },
        "timeoutMs": ELEMENT_WAIT_TIMEOUT_MS,
    }));
}

fn flush_text_entry(context: &mut DraftContext) {
    let Some(text_entry) = context.active_text_entry.take() else {
        return;
    };

    if text_entry.value.is_empty() {
        return;
    }

    context.steps.push(json!({
        "kind": "setText",
        "selector": text_entry.selector,
        "value": {
            "kind": "literal",
            "value": text_entry.value,
        },
    }));
}

fn is_editable_role(role: Option<&str>) -> bool {
    matches!(
        role,
        Some("AXTextField" | "AXTextArea" | "AXSearchField" | "AXComboBox")
    )
}

enum KeyInput {
    Text(&'static str),
    Backspace,
    Submit,
}

fn translate_key_code(key_code: i32) -> Option<KeyInput> {
    match key_code {
        0 => Some(KeyInput::Text("a")),
        1 => Some(KeyInput::Text("s")),
        2 => Some(KeyInput::Text("d")),
        3 => Some(KeyInput::Text("f")),
        4 => Some(KeyInput::Text("h")),
        5 => Some(KeyInput::Text("g")),
        6 => Some(KeyInput::Text("z")),
        7 => Some(KeyInput::Text("x")),
        8 => Some(KeyInput::Text("c")),
        9 => Some(KeyInput::Text("v")),
        11 => Some(KeyInput::Text("b")),
        12 => Some(KeyInput::Text("q")),
        13 => Some(KeyInput::Text("w")),
        14 => Some(KeyInput::Text("e")),
        15 => Some(KeyInput::Text("r")),
        16 => Some(KeyInput::Text("y")),
        17 => Some(KeyInput::Text("t")),
        18 => Some(KeyInput::Text("1")),
        19 => Some(KeyInput::Text("2")),
        20 => Some(KeyInput::Text("3")),
        21 => Some(KeyInput::Text("4")),
        22 => Some(KeyInput::Text("6")),
        23 => Some(KeyInput::Text("5")),
        24 => Some(KeyInput::Text("=")),
        25 => Some(KeyInput::Text("9")),
        26 => Some(KeyInput::Text("7")),
        27 => Some(KeyInput::Text("-")),
        28 => Some(KeyInput::Text("8")),
        29 => Some(KeyInput::Text("0")),
        30 => Some(KeyInput::Text("]")),
        31 => Some(KeyInput::Text("o")),
        32 => Some(KeyInput::Text("u")),
        33 => Some(KeyInput::Text("[")),
        34 => Some(KeyInput::Text("i")),
        35 => Some(KeyInput::Text("p")),
        37 => Some(KeyInput::Text("l")),
        38 => Some(KeyInput::Text("j")),
        39 => Some(KeyInput::Text("'")),
        40 => Some(KeyInput::Text("k")),
        41 => Some(KeyInput::Text(";")),
        42 => Some(KeyInput::Text("\\")),
        43 => Some(KeyInput::Text(",")),
        44 => Some(KeyInput::Text("/")),
        45 => Some(KeyInput::Text("n")),
        46 => Some(KeyInput::Text("m")),
        47 => Some(KeyInput::Text(".")),
        49 => Some(KeyInput::Text(" ")),
        51 => Some(KeyInput::Backspace),
        36 | 48 => Some(KeyInput::Submit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::{
        compile_workflow, continue_workflow_execution, execute_workflow_with_runner,
        reject_workflow_run, resumable_execution_state, ApprovalDecision,
    };
    use crate::core::runner::{
        RunnerError, RunnerStepExecutor, RunnerStepRequest, RunnerStepResult,
    };
    use crate::storage::{RawEventRecord, Storage};
    use serde_json::{json, Value};

    struct MockRunner {
        outcomes: VecDeque<Result<Value, RunnerError>>,
        seen_kinds: Vec<String>,
    }

    impl MockRunner {
        fn new(outcomes: Vec<Result<Value, RunnerError>>) -> Self {
            Self {
                outcomes: outcomes.into(),
                seen_kinds: Vec::new(),
            }
        }
    }

    impl RunnerStepExecutor for MockRunner {
        fn execute_step(
            &mut self,
            request: &RunnerStepRequest,
            _timeout: Duration,
        ) -> Result<RunnerStepResult, RunnerError> {
            self.seen_kinds.push(
                request
                    .step
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
            );

            match self.outcomes.pop_front().unwrap_or_else(|| Ok(json!({}))) {
                Ok(result) => Ok(RunnerStepResult { result }),
                Err(error) => Err(error),
            }
        }
    }

    #[test]
    fn compiles_focus_click_and_text_steps() {
        let workflow = compile_workflow(42, "Signup".to_string(), &sample_events())
            .expect("workflow should compile");

        let steps = workflow
            .workflow
            .get("steps")
            .and_then(|value| value.as_array())
            .expect("steps array should exist");

        assert_eq!(workflow.step_count, 5);
        assert_eq!(
            steps[0],
            json!({ "kind": "focusWindow", "bundleId": "com.apple.TextEdit" })
        );
        assert_eq!(steps[1]["kind"], "waitFor");
        assert_eq!(steps[2]["kind"], "click");
        assert_eq!(steps[3]["kind"], "setText");
        assert_eq!(steps[3]["value"]["value"], "hello");
        assert_eq!(
            steps[4],
            json!({ "kind": "focusWindow", "bundleId": "com.apple.Safari" })
        );
    }

    #[test]
    fn ignores_non_editable_key_sequences() {
        let events = vec![
            raw_event(
                1,
                0,
                "ax_snapshot",
                json!({
                    "schemaVersion": 1,
                    "payload": {
                        "role": "AXButton",
                        "selector": {
                            "targetApp": { "bundleId": "com.apple.TextEdit" },
                            "ax": { "role": "AXButton", "title": "Submit" }
                        }
                    }
                }),
            ),
            raw_event(
                2,
                1,
                "mouse_down",
                json!({ "schemaVersion": 1, "payload": { "x": 100, "y": 100 } }),
            ),
            raw_event(
                3,
                2,
                "key_down",
                json!({ "schemaVersion": 1, "payload": { "keyCode": 12 } }),
            ),
        ];

        let workflow =
            compile_workflow(7, "No text".to_string(), &events).expect("workflow should compile");
        let steps = workflow
            .workflow
            .get("steps")
            .and_then(|value| value.as_array())
            .expect("steps array should exist");

        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["kind"], "waitFor");
        assert_eq!(steps[1]["kind"], "click");
    }

    #[test]
    fn executes_workflow_and_persists_run_logs() {
        let root = unique_test_dir();
        let storage =
            Storage::bootstrap(root.join("storage.sqlite3")).expect("storage should bootstrap");
        let workflow = json!({
            "id": "wf_exec",
            "name": "Executor Smoke",
            "steps": [
                { "kind": "focusWindow", "bundleId": "com.apple.TextEdit" },
                { "kind": "waitFor", "condition": { "kind": "elementPresent", "selector": { "ax": { "role": "AXButton" } } }, "timeoutMs": 250 },
                { "kind": "click", "selector": { "ax": { "role": "AXButton", "title": "Save" } } }
            ]
        });
        let mut runner = MockRunner::new(vec![
            Ok(json!({ "action": "focusWindow" })),
            Ok(json!({ "action": "click" })),
        ]);

        let summary = execute_workflow_with_runner(&storage, &mut runner, &workflow, None)
            .expect("workflow should execute");

        assert_eq!(summary.status, "completed");
        assert_eq!(summary.completed_step_count, 3);
        assert_eq!(runner.seen_kinds, vec!["focusWindow", "click"]);

        let run = storage
            .get_workflow_run(summary.run_row_id)
            .expect("workflow run should load")
            .expect("workflow run should exist");
        assert_eq!(run.status, "completed");
        assert_eq!(run.completed_step_count, 3);

        let logs = storage
            .list_workflow_run_logs(summary.run_row_id, 20)
            .expect("logs should load");
        assert!(logs
            .iter()
            .any(|log| log.event_type == "step_attempt_started"));
        assert!(logs.iter().any(|log| log.event_type == "step_finished"));
        assert!(logs.iter().any(|log| log.event_type == "run_completed"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn retries_retryable_step_and_records_failure_metadata() {
        let root = unique_test_dir();
        let storage =
            Storage::bootstrap(root.join("storage.sqlite3")).expect("storage should bootstrap");
        let workflow = json!({
            "id": "wf_retry",
            "name": "Retry Workflow",
            "steps": [
                {
                    "kind": "click",
                    "selector": { "ax": { "role": "AXButton", "title": "Save" } },
                    "retry": { "maxAttempts": 2 }
                },
                {
                    "kind": "setText",
                    "selector": { "ax": { "role": "AXTextField", "title": "Name" } },
                    "value": { "kind": "literal", "value": "Alice" }
                }
            ]
        });
        let mut runner = MockRunner::new(vec![
            Err(RunnerError::Timeout {
                operation: "step execution",
                stderr_tail: String::new(),
            }),
            Ok(json!({ "action": "click" })),
            Err(RunnerError::Remote {
                code: "EXECUTION_FAILED".to_string(),
                message: "setText failed".to_string(),
                retryable: false,
            }),
        ]);

        let summary = execute_workflow_with_runner(&storage, &mut runner, &workflow, None)
            .expect("workflow execution should return summary");

        assert_eq!(summary.status, "failed");
        assert_eq!(summary.completed_step_count, 1);
        assert_eq!(summary.failed_step_index, Some(1));
        assert!(summary
            .last_error
            .as_deref()
            .expect("last error should exist")
            .contains("setText failed"));

        let logs = storage
            .list_workflow_run_logs(summary.run_row_id, 20)
            .expect("logs should load");
        assert!(logs
            .iter()
            .any(|log| log.event_type == "step_retry_scheduled"));
        assert!(logs.iter().any(|log| log.event_type == "run_failed"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn risky_step_pauses_for_approval_and_can_resume() {
        let root = unique_test_dir();
        let storage =
            Storage::bootstrap(root.join("storage.sqlite3")).expect("storage should bootstrap");
        let workflow = json!({
            "id": "wf_approval",
            "name": "Approval Workflow",
            "steps": [
                { "kind": "click", "selector": { "ax": { "role": "AXButton", "title": "Delete Account" } } }
            ]
        });
        let mut initial_runner = MockRunner::new(vec![Ok(json!({ "action": "click" }))]);

        let initial_summary =
            execute_workflow_with_runner(&storage, &mut initial_runner, &workflow, None)
                .expect("workflow should return approval summary");

        assert_eq!(initial_summary.status, "awaiting_approval");
        assert_eq!(initial_summary.completed_step_count, 0);
        assert!(
            initial_runner.seen_kinds.is_empty(),
            "risky step should not execute before approval"
        );

        let run = storage
            .get_workflow_run(initial_summary.run_row_id)
            .expect("workflow run should load")
            .expect("workflow run should exist");
        assert_eq!(run.status, "awaiting_approval");

        let state = resumable_execution_state(&storage, run, ApprovalDecision::Approved)
            .expect("run should be resumable");
        let mut resumed_runner = MockRunner::new(vec![Ok(json!({ "action": "click" }))]);
        let resumed_summary = continue_workflow_execution(&storage, &mut resumed_runner, state)
            .expect("approved run should continue");

        assert_eq!(resumed_summary.status, "completed");
        assert_eq!(resumed_summary.completed_step_count, 1);
        assert_eq!(resumed_runner.seen_kinds, vec!["click"]);

        let logs = storage
            .list_workflow_run_logs(initial_summary.run_row_id, 20)
            .expect("logs should load");
        assert!(logs
            .iter()
            .any(|log| log.event_type == "approval_requested"));
        assert!(logs.iter().any(|log| log.event_type == "approval_approved"));
        assert!(logs.iter().any(|log| log.event_type == "run_completed"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn risky_step_can_be_rejected_without_execution() {
        let root = unique_test_dir();
        let storage =
            Storage::bootstrap(root.join("storage.sqlite3")).expect("storage should bootstrap");
        let workflow = json!({
            "id": "wf_reject",
            "name": "Reject Workflow",
            "steps": [
                { "kind": "selectMenu", "path": ["File", "Delete"] }
            ]
        });
        let mut runner = MockRunner::new(vec![Ok(json!({ "action": "selectMenu" }))]);

        let initial_summary = execute_workflow_with_runner(&storage, &mut runner, &workflow, None)
            .expect("workflow should return approval summary");
        assert_eq!(initial_summary.status, "awaiting_approval");
        assert!(
            runner.seen_kinds.is_empty(),
            "risky menu action should not execute before approval"
        );

        let rejected_summary =
            reject_workflow_run(&storage, initial_summary.run_row_id).expect("run should reject");
        assert_eq!(rejected_summary.status, "rejected");
        assert_eq!(rejected_summary.completed_step_count, 0);

        let logs = storage
            .list_workflow_run_logs(initial_summary.run_row_id, 20)
            .expect("logs should load");
        assert!(logs.iter().any(|log| log.event_type == "approval_rejected"));
        assert!(logs.iter().any(|log| log.event_type == "run_rejected"));

        let _ = fs::remove_dir_all(&root);
    }

    fn sample_events() -> Vec<RawEventRecord> {
        vec![
            raw_event(
                1,
                0,
                "frontmost_app_changed",
                json!({ "schemaVersion": 1, "payload": { "bundleId": "com.apple.TextEdit" } }),
            ),
            raw_event(
                2,
                1,
                "ax_snapshot",
                json!({
                    "schemaVersion": 1,
                    "payload": {
                        "role": "AXTextField",
                        "selector": {
                            "targetApp": { "bundleId": "com.apple.TextEdit" },
                            "ax": { "role": "AXTextField", "title": "Name" }
                        }
                    }
                }),
            ),
            raw_event(
                3,
                2,
                "mouse_down",
                json!({ "schemaVersion": 1, "payload": { "x": 120, "y": 220 } }),
            ),
            raw_event(
                4,
                3,
                "key_down",
                json!({ "schemaVersion": 1, "payload": { "keyCode": 4 } }),
            ),
            raw_event(
                5,
                4,
                "key_down",
                json!({ "schemaVersion": 1, "payload": { "keyCode": 14 } }),
            ),
            raw_event(
                6,
                5,
                "key_down",
                json!({ "schemaVersion": 1, "payload": { "keyCode": 37 } }),
            ),
            raw_event(
                7,
                6,
                "key_down",
                json!({ "schemaVersion": 1, "payload": { "keyCode": 37 } }),
            ),
            raw_event(
                8,
                7,
                "key_down",
                json!({ "schemaVersion": 1, "payload": { "keyCode": 31 } }),
            ),
            raw_event(
                9,
                8,
                "frontmost_app_changed",
                json!({ "schemaVersion": 1, "payload": { "bundleId": "com.apple.Safari" } }),
            ),
        ]
    }

    fn raw_event(id: i64, sequence: i64, event_type: &str, event_json: Value) -> RawEventRecord {
        RawEventRecord {
            id,
            session_id: 1,
            sequence,
            event_type: event_type.to_string(),
            event_json: serde_json::to_string(&event_json).expect("event json should serialize"),
            recorded_at_ms: 0,
            created_at_ms: 0,
        }
    }

    fn unique_test_dir() -> std::path::PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("cloneaprocess-workflow-test-{}", timestamp))
    }
}
