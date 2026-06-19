use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::runner::{RunnerBridge, RunnerStepExecutor, RunnerStepRequest};
use crate::storage::{RawEventRecord, SessionRecord, Storage};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_MODEL: &str = "claude-opus-4-8";
const ANTHROPIC_API_VERSION: &str = "2023-06-01";
// Computer-use beta. `computer_20251124` + this header are required for Opus 4.8 / 4.7 / 4.6,
// Sonnet 4.6, and Opus 4.5. On these models the coordinates the model returns are 1:1 with the
// pixels of the image we send (long edge up to 2576px), so no scale-factor conversion is needed —
// the runner already hands us logical-point screenshots that match the click coordinate space.
const ANTHROPIC_BETA: &str = "computer-use-2025-11-24";
const COMPUTER_TOOL_TYPE: &str = "computer_20251124";
// Thinking depth / token spend. `medium` is the sweet spot for GUI automation — enough reasoning
// to act reliably, far faster and cheaper than `high` across a long step loop.
const EFFORT: &str = "medium";
const MAX_RESPONSE_TOKENS: u32 = 8192;

const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(5);
const ACTION_TIMEOUT: Duration = Duration::from_secs(6);
const DEFAULT_MAX_STEPS: u32 = 120;
const ACTION_SETTLE_MS: u64 = 400;
const MAX_RECORDING_KEYFRAMES: usize = 4;
// Keep only the most recent screenshots in the conversation. Older ones are replaced with a text
// placeholder so context (and cost) stays bounded across long runs.
const MAX_CONVERSATION_IMAGES: usize = 3;

// ---- Public types ----

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum AgentEvent {
    #[serde(rename = "screenshot")]
    Screenshot { step_number: u32, base64: String, width: u32, height: u32 },
    #[serde(rename = "thinking")]
    Thinking { step_number: u32 },
    #[serde(rename = "action")]
    Action { step_number: u32, tool: String, description: String, params: Value },
    #[serde(rename = "action_result")]
    ActionResult { step_number: u32, success: bool, error: Option<String> },
    #[serde(rename = "completed")]
    Completed { step_number: u32, summary: String, total_input_tokens: u64, total_output_tokens: u64 },
    #[serde(rename = "failed")]
    Failed { step_number: u32, error: String },
    #[serde(rename = "cancelled")]
    Cancelled { step_number: u32 },
}

pub struct AgentConfig {
    /// A recorded session to use as a demonstration, if any. `None` runs from `task` alone — the
    /// agent explores the live screen with no recording ("you don't need to hit record").
    pub session_id: Option<i64>,
    /// Explicit task description. Required when `session_id` is `None`; otherwise it overrides the
    /// recorded session's description when provided.
    pub task: Option<String>,
    pub max_steps: u32,
    pub api_key: String,
    pub cancel_token: Arc<AtomicBool>,
}

/// How to translate the coordinate space of the screenshot the model is currently looking at into
/// global click coordinates: which display it was captured from (`origin`) and any downscaling
/// (`point_scale`).
#[derive(Clone, Copy)]
struct View {
    origin_x: f64,
    origin_y: f64,
    point_scale: f64,
    width: u32,
    height: u32,
}

impl View {
    fn from_shot(shot: &crate::core::runner::ScreenshotResult) -> Self {
        Self {
            origin_x: shot.origin_x,
            origin_y: shot.origin_y,
            point_scale: shot.point_scale,
            width: shot.width,
            height: shot.height,
        }
    }
}

// ---- Agent loop ----

pub fn run_agent<F>(
    storage: &Storage,
    runner_binary: &Path,
    config: AgentConfig,
    emit: F,
) -> Result<(), String>
where
    F: Fn(AgentEvent) + Send,
{
    // Resolve the task and (optional) recording. With no session, the agent works from the task
    // description alone and discovers the rest by looking at the live screen.
    let (task, recording_summary, recording_keyframes) = match config.session_id {
        Some(sid) => {
            let session = storage.get_session(sid)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("session {sid} not found"))?;
            let events = storage
                .list_raw_events_for_session(sid, 200)
                .map_err(|e| e.to_string())?;
            let task = config.task.clone()
                .or_else(|| session.description.clone())
                .unwrap_or_else(|| "(no task description provided)".to_string());
            let summary = build_recording_summary(&session, &events);
            let keyframes = load_recording_keyframes(&events);
            (task, Some(summary), keyframes)
        }
        None => {
            let task = config.task.clone()
                .filter(|t| !t.trim().is_empty())
                .ok_or_else(|| "a task description is required when running without a recording".to_string())?;
            (task, None, Vec::new())
        }
    };

    let mut runner = RunnerBridge::spawn(runner_binary)
        .map_err(|e| format!("failed to start runner: {e}"))?;

    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| format!("failed to create async runtime: {e}"))?;

    let max_steps = if config.max_steps == 0 { DEFAULT_MAX_STEPS } else { config.max_steps };
    let mut step_number: u32 = 0;
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    // Concrete runner steps actually executed, in order — saved on success as a replayable script.
    let mut captured_steps: Vec<Value> = Vec::new();

    // Take an initial screenshot. Its (logical-point) dimensions define the computer tool's
    // display size so the model's coordinates land 1:1 on our clicks.
    let initial = runner.take_screenshot(SCREENSHOT_TIMEOUT)
        .map_err(|e| format!("initial screenshot failed: {e}"))?;
    eprintln!(
        "[agent] capture {}x{} at origin ({:.0}, {:.0}); backing scale {:.1}, point scale {:.2}",
        initial.width, initial.height, initial.origin_x, initial.origin_y, initial.scale, initial.point_scale
    );
    // The coordinate space of the screenshot the model will next react to.
    let mut view = View::from_shot(&initial);
    emit(AgentEvent::Screenshot {
        step_number,
        base64: initial.base64.clone(),
        width: initial.width,
        height: initial.height,
    });

    let system_prompt = build_system_prompt();

    // Seed the conversation with the task, the recording walkthrough, and the live screen.
    let mut first_content = build_first_user_message(&task, recording_summary.as_deref(), &recording_keyframes);
    first_content.push(json!({"type": "text", "text": "\nHere is the current state of the screen:"}));
    first_content.push(image_block(&initial.base64));
    first_content.push(json!({"type": "text", "text":
        "Complete the task. Take one action at a time and check the result before the next."}));

    let mut conversation: Vec<Value> = vec![json!({"role": "user", "content": first_content})];

    loop {
        if config.cancel_token.load(Ordering::Relaxed) {
            emit(AgentEvent::Cancelled { step_number });
            return Ok(());
        }

        if step_number >= max_steps {
            emit(AgentEvent::Failed {
                step_number,
                error: format!("reached maximum step limit ({max_steps})"),
            });
            return Ok(());
        }

        // Drop stale screenshots before sending — keeps context bounded over long runs.
        prune_images(&mut conversation, MAX_CONVERSATION_IMAGES);

        emit(AgentEvent::Thinking { step_number });

        // Declare the tool's display size to match the screenshot the model is acting on — capture
        // can follow the active window to a differently-sized display between turns.
        let tools = tool_definitions(view.width, view.height);
        let api_result = runtime.block_on(call_claude(
            &config.api_key,
            &system_prompt,
            &tools,
            &conversation,
        )).map_err(|e| format!("Claude API error: {e}"))?;

        total_input_tokens += api_result.input_tokens;
        total_output_tokens += api_result.output_tokens;

        // Preserve the full assistant turn verbatim (including any thinking blocks) so multi-turn
        // tool use stays valid.
        conversation.push(json!({"role": "assistant", "content": api_result.content.clone()}));

        let tool_uses: Vec<&Value> = api_result.content.iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
            .collect();

        if tool_uses.is_empty() {
            // No tool call — the model considers the task done. Use its text as the summary.
            let text = api_result.content.iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            // Save the executed steps as a replayable script so this task can be re-run fast
            // (deterministically, no LLM) next time.
            save_script(storage, config.session_id, &task, &captured_steps);
            emit(AgentEvent::Completed {
                step_number,
                summary: if text.trim().is_empty() { "Task completed.".to_string() } else { text },
                total_input_tokens,
                total_output_tokens,
            });
            return Ok(());
        }

        let mut tool_results: Vec<Value> = Vec::new();
        // All actions in this assistant turn were decided from the same screenshot, so they share
        // its coordinate space. `view` may advance as we capture results for the *next* turn.
        let turn_view = view;

        for tool_call in tool_uses {
            let tool_id = tool_call.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            let input = tool_call.get("input").cloned().unwrap_or_else(|| json!({}));
            let action = input.get("action").and_then(Value::as_str).unwrap_or("").to_string();
            let description = describe_action(&action, &input);

            emit(AgentEvent::Action {
                step_number,
                tool: "computer".to_string(),
                description: description.clone(),
                params: input.clone(),
            });

            // `zoom` is a read-only detail view: capture the requested region at full resolution
            // and return it, without taking a fresh full-desktop screenshot or advancing `view`
            // (the model still clicks against the composite it already has).
            if action == "zoom" {
                let region = input.get("region").and_then(Value::as_array).and_then(|r| {
                    Some((r.first()?.as_f64()?, r.get(1)?.as_f64()?, r.get(2)?.as_f64()?, r.get(3)?.as_f64()?))
                });
                let content = match region {
                    Some((x1, y1, x2, y2)) => {
                        let gx = turn_view.origin_x + x1.min(x2) * turn_view.point_scale;
                        let gy = turn_view.origin_y + y1.min(y2) * turn_view.point_scale;
                        let gw = (x2 - x1).abs() * turn_view.point_scale;
                        let gh = (y2 - y1).abs() * turn_view.point_scale;
                        match runner.zoom_capture(gx, gy, gw, gh, SCREENSHOT_TIMEOUT) {
                            Ok(z) => {
                                emit(AgentEvent::Screenshot {
                                    step_number, base64: z.base64.clone(), width: z.width, height: z.height,
                                });
                                emit(AgentEvent::ActionResult { step_number, success: true, error: None });
                                vec![
                                    json!({"type": "text", "text": "Zoomed view of the requested region:"}),
                                    image_block(&z.base64),
                                ]
                            }
                            Err(e) => {
                                let err = e.to_string();
                                emit(AgentEvent::ActionResult { step_number, success: false, error: Some(err.clone()) });
                                vec![json!({"type": "text", "text": format!("Zoom failed: {err}")})]
                            }
                        }
                    }
                    None => {
                        emit(AgentEvent::ActionResult {
                            step_number, success: false,
                            error: Some("zoom requires region [x1, y1, x2, y2]".to_string()),
                        });
                        vec![json!({"type": "text", "text": "zoom requires a region [x1, y1, x2, y2]"})]
                    }
                };
                tool_results.push(json!({
                    "type": "tool_result",
                    "tool_use_id": tool_id,
                    "content": content,
                }));
                continue;
            }

            let exec = execute_computer_action(&mut runner, &action, &input, &turn_view);

            // Let the UI settle, then capture the result so the model sees the consequence. The
            // capture may be on a different display now (focus moved), so advance `view` to it.
            std::thread::sleep(Duration::from_millis(ACTION_SETTLE_MS));
            let shot = runner.take_screenshot(SCREENSHOT_TIMEOUT).ok();
            if let Some(s) = &shot {
                view = View::from_shot(s);
                emit(AgentEvent::Screenshot {
                    step_number,
                    base64: s.base64.clone(),
                    width: s.width,
                    height: s.height,
                });
            }

            // Report the result as ordinary (non-error) tool content so it can carry the
            // screenshot — the API rejects images when `is_error` is true, and keeping the agent
            // sighted on failures matters more than the error flag. The failure is conveyed in text.
            let mut content: Vec<Value> = Vec::new();
            match &exec {
                Ok(step) => {
                    // Record the concrete step for the replayable script.
                    if let Some(step) = step {
                        captured_steps.push(step.clone());
                    }
                    emit(AgentEvent::ActionResult { step_number, success: true, error: None });
                }
                Err(err) => {
                    emit(AgentEvent::ActionResult { step_number, success: false, error: Some(err.clone()) });
                    content.push(json!({"type": "text", "text":
                        format!("That action could not be performed: {err}. Try a different approach.")}));
                }
            }

            match &shot {
                Some(s) => content.push(image_block(&s.base64)),
                None if content.is_empty() => {
                    content.push(json!({"type": "text", "text":
                        "Action completed, but a screenshot could not be captured."}));
                }
                None => {}
            }

            tool_results.push(json!({
                "type": "tool_result",
                "tool_use_id": tool_id,
                "content": content,
            }));
        }

        conversation.push(json!({"role": "user", "content": tool_results}));
        step_number += 1;
    }
}

// ---- Script capture & replay ----

fn script_key(session_id: Option<i64>) -> String {
    match session_id {
        Some(id) => format!("agent_script_session_{id}"),
        None => "agent_script_task_last".to_string(),
    }
}

fn save_script(storage: &Storage, session_id: Option<i64>, task: &str, steps: &[Value]) {
    if steps.is_empty() {
        return;
    }
    let payload = json!({ "task": task, "steps": steps });
    let _ = storage.upsert_app_setting(&script_key(session_id), &payload.to_string());
}

/// Loads a previously-captured replay script for a session (or the last no-record task).
pub fn load_script(storage: &Storage, session_id: Option<i64>) -> Option<(String, Vec<Value>)> {
    let raw = storage.get_app_setting(&script_key(session_id)).ok().flatten()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    let task = value.get("task").and_then(Value::as_str).unwrap_or("").to_string();
    let steps = value.get("steps").and_then(Value::as_array)?.clone();
    if steps.is_empty() {
        return None;
    }
    Some((task, steps))
}

pub fn has_script(storage: &Storage, session_id: Option<i64>) -> bool {
    load_script(storage, session_id).is_some()
}

/// Replays a captured script deterministically through the runner — no LLM, no per-step thinking.
/// Emits the same progress events as a live run. On a step error it stops and reports that the UI
/// may have changed (the vision fallback is the next phase).
pub fn run_script<F>(
    runner_binary: &Path,
    steps: Vec<Value>,
    cancel_token: Arc<AtomicBool>,
    emit: F,
) -> Result<(), String>
where
    F: Fn(AgentEvent) + Send,
{
    let mut runner = RunnerBridge::spawn(runner_binary)
        .map_err(|e| format!("failed to start runner: {e}"))?;

    let mut step_number: u32 = 0;
    if let Ok(s) = runner.take_screenshot(SCREENSHOT_TIMEOUT) {
        emit(AgentEvent::Screenshot { step_number, base64: s.base64, width: s.width, height: s.height });
    }

    for step in &steps {
        if cancel_token.load(Ordering::Relaxed) {
            emit(AgentEvent::Cancelled { step_number });
            return Ok(());
        }

        let description = describe_step(step);
        emit(AgentEvent::Action {
            step_number,
            tool: "replay".to_string(),
            description: description.clone(),
            params: step.clone(),
        });

        let request = RunnerStepRequest {
            workflow_id: "agent_replay".to_string(),
            outer_run_id: "agent_replay_run".to_string(),
            step_index: step_number as usize,
            attempt: 1,
            operation_label: description,
            step: step.clone(),
        };

        match runner.execute_step(&request, ACTION_TIMEOUT) {
            Ok(_) => emit(AgentEvent::ActionResult { step_number, success: true, error: None }),
            Err(e) => {
                let err = e.to_string();
                emit(AgentEvent::ActionResult { step_number, success: false, error: Some(err.clone()) });
                emit(AgentEvent::Failed {
                    step_number,
                    error: format!("Replay step failed ({err}). The UI may have changed — re-run with AI to refresh the script."),
                });
                return Ok(());
            }
        }

        std::thread::sleep(Duration::from_millis(ACTION_SETTLE_MS));
        if let Ok(s) = runner.take_screenshot(SCREENSHOT_TIMEOUT) {
            emit(AgentEvent::Screenshot { step_number, base64: s.base64, width: s.width, height: s.height });
        }
        step_number += 1;
    }

    emit(AgentEvent::Completed {
        step_number,
        summary: format!("Replayed {} steps deterministically (no AI).", steps.len()),
        total_input_tokens: 0,
        total_output_tokens: 0,
    });
    Ok(())
}

/// Human-readable label for a captured runner step (for the replay progress UI).
fn describe_step(step: &Value) -> String {
    let kind = step.get("kind").and_then(Value::as_str).unwrap_or("step");
    let num = |k: &str| step.get(k).and_then(Value::as_f64).unwrap_or(0.0);
    match kind {
        "clickAt" => format!("click at ({:.0}, {:.0})", num("x"), num("y")),
        "clickElement" => {
            let label = step.get("selector").and_then(|s| s.get("ax"))
                .and_then(|a| a.get("title").or_else(|| a.get("identifier")))
                .and_then(Value::as_str).unwrap_or("element");
            format!("click \"{label}\"")
        }
        "moveMouse" => format!("move to ({:.0}, {:.0})", num("x"), num("y")),
        "typeText" => format!("type \"{}\"", step.get("text").and_then(Value::as_str).unwrap_or("")),
        "keyPress" => format!("press {}", step.get("key").and_then(Value::as_str).unwrap_or("")),
        "holdKey" => format!("hold {}", step.get("key").and_then(Value::as_str).unwrap_or("")),
        "scroll" => format!("scroll {}", step.get("direction").and_then(Value::as_str).unwrap_or("")),
        "drag" => "drag".to_string(),
        "delay" => "wait".to_string(),
        other => other.to_string(),
    }
}

// ---- Tool execution ----

/// Executes one computer-use action. Returns the concrete runner step that was run (with global
/// coordinates already resolved), so the caller can record it into a replayable script. `None`
/// means the action needed no runner step (a screenshot/cursor query).
fn execute_computer_action(
    runner: &mut RunnerBridge,
    action: &str,
    input: &Value,
    view: &View,
) -> Result<Option<Value>, String> {
    // `screenshot` and `cursor_position` need no runner step — the screenshot we take after every
    // action is the feedback the model is asking for.
    if action == "screenshot" || action == "cursor_position" {
        return Ok(None);
    }

    // Map a coordinate from the screenshot the model saw into the global click space, accounting
    // for which display was captured and any downscaling.
    let map = |input: &Value, key: &str| -> Result<(f64, f64), String> {
        let (mx, my) = coordinate(input, key)?;
        Ok((view.origin_x + mx * view.point_scale, view.origin_y + my * view.point_scale))
    };

    // Click family: execute at the vision-chosen point, but capture an AX selector (when the
    // element is specifically identifiable) so the recorded step replays by element — robust to
    // layout shifts — with the coordinate as a fallback.
    if let Some((button, click_count)) = click_button_count(action) {
        let (x, y) = map(input, "coordinate")?;
        let modifiers = modifiers_from_text(input);
        let selector = specific_selector(runner, x, y);
        let exec_step = json!({"kind": "clickAt", "x": x, "y": y, "button": button, "clickCount": click_count, "modifiers": modifiers});
        run_step(runner, action, &exec_step)?;
        let record = match selector {
            Some(sel) => json!({"kind": "clickElement", "selector": sel, "x": x, "y": y, "button": button, "clickCount": click_count, "modifiers": modifiers}),
            None => exec_step,
        };
        return Ok(Some(record));
    }

    let step_json = match action {
        "mouse_move" => {
            let (x, y) = map(input, "coordinate")?;
            json!({"kind": "moveMouse", "x": x, "y": y})
        }
        "left_click_drag" => {
            let (fx, fy) = map(input, "start_coordinate")?;
            let (tx, ty) = map(input, "coordinate")?;
            json!({"kind": "drag", "fromX": fx, "fromY": fy, "toX": tx, "toY": ty})
        }
        "type" => {
            let text = input.get("text").and_then(Value::as_str)
                .ok_or_else(|| "type action requires text".to_string())?;
            json!({"kind": "typeText", "text": text})
        }
        "key" => {
            let combo = input.get("text").and_then(Value::as_str)
                .ok_or_else(|| "key action requires text".to_string())?;
            let (key, modifiers) = parse_key_combo(combo);
            if key.is_empty() {
                return Err(format!("could not parse key combo: {combo}"));
            }
            json!({"kind": "keyPress", "key": key, "modifiers": modifiers})
        }
        "scroll" => {
            let (x, y) = map(input, "coordinate")?;
            let direction = input.get("scroll_direction").and_then(Value::as_str).unwrap_or("down");
            let amount = input.get("scroll_amount").and_then(Value::as_i64).unwrap_or(3);
            json!({"kind": "scroll", "x": x, "y": y, "direction": direction, "amount": amount, "modifiers": modifiers_from_text(input)})
        }
        "hold_key" => {
            let combo = input.get("text").and_then(Value::as_str)
                .ok_or_else(|| "hold_key action requires text".to_string())?;
            let (key, modifiers) = parse_key_combo(combo);
            if key.is_empty() {
                return Err(format!("could not parse key combo: {combo}"));
            }
            let secs = input.get("duration").and_then(Value::as_f64).unwrap_or(1.0);
            json!({"kind": "holdKey", "key": key, "modifiers": modifiers, "durationMs": (secs * 1000.0) as u64})
        }
        "wait" => {
            let secs = input.get("duration").and_then(Value::as_f64).unwrap_or(1.0);
            json!({"kind": "delay", "ms": (secs * 1000.0) as u64})
        }
        other => return Err(format!("unsupported computer action: {other}")),
    };

    run_step(runner, action, &step_json)?;
    Ok(Some(step_json))
}

/// Button + click count for the click-family computer actions; `None` for non-click actions.
fn click_button_count(action: &str) -> Option<(&'static str, u32)> {
    match action {
        "left_click" => Some(("left", 1)),
        "right_click" => Some(("right", 1)),
        "middle_click" => Some(("middle", 1)),
        "double_click" => Some(("left", 2)),
        "triple_click" => Some(("left", 3)),
        _ => None,
    }
}

/// Queries the AX element at a global point and returns a selector — but only when the element is
/// specifically identifiable (has a title or identifier), not a bare container like AXScrollArea.
/// Returns `None` so the caller keeps a plain coordinate click for opaque/unlabeled elements.
fn specific_selector(runner: &mut RunnerBridge, x: f64, y: f64) -> Option<Value> {
    let info = runner.describe_element_at(x, y, ACTION_TIMEOUT).ok()?;
    if info.get("found").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let title = info.get("title").and_then(Value::as_str).filter(|s| !s.is_empty());
    let identifier = info.get("identifier").and_then(Value::as_str).filter(|s| !s.is_empty());
    if title.is_none() && identifier.is_none() {
        return None;
    }
    let mut ax = serde_json::Map::new();
    if let Some(role) = info.get("role").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        ax.insert("role".to_string(), json!(role));
    }
    if let Some(t) = title {
        ax.insert("title".to_string(), json!(t));
    }
    if let Some(id) = identifier {
        ax.insert("identifier".to_string(), json!(id));
    }
    Some(json!({ "ax": Value::Object(ax) }))
}

fn run_step(runner: &mut RunnerBridge, label: &str, step: &Value) -> Result<(), String> {
    let request = RunnerStepRequest {
        workflow_id: "agent".to_string(),
        outer_run_id: "agent_run".to_string(),
        step_index: 0,
        attempt: 1,
        operation_label: label.to_string(),
        step: step.clone(),
    };
    runner.execute_step(&request, ACTION_TIMEOUT)
        .map(|_: crate::core::runner::RunnerStepResult| ())
        .map_err(|e: crate::core::runner::RunnerError| e.to_string())
}

fn coordinate(input: &Value, key: &str) -> Result<(f64, f64), String> {
    let arr = input.get(key).and_then(Value::as_array)
        .ok_or_else(|| format!("action requires {key} [x, y]"))?;
    let x = arr.first().and_then(Value::as_f64)
        .ok_or_else(|| format!("{key} missing x"))?;
    let y = arr.get(1).and_then(Value::as_f64)
        .ok_or_else(|| format!("{key} missing y"))?;
    Ok((x, y))
}

/// On click/scroll actions the `text` field carries held modifier keys (e.g. "shift", "ctrl").
fn modifiers_from_text(input: &Value) -> Vec<String> {
    input.get("text").and_then(Value::as_str)
        .map(|t| t.split('+').map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default()
}

/// Splits a `key` combo like "cmd+shift+t" into ("t", ["cmd", "shift"]). The runner lower-cases
/// and maps key names (return, tab, page_down, arrows, letters, …) and modifier names itself.
fn parse_key_combo(combo: &str) -> (String, Vec<String>) {
    let parts: Vec<&str> = combo.split('+').map(str::trim).filter(|s| !s.is_empty()).collect();
    match parts.split_last() {
        Some((key, modifiers)) => (
            key.to_lowercase(),
            modifiers.iter().map(|m| m.to_lowercase()).collect(),
        ),
        None => (String::new(), Vec::new()),
    }
}

fn describe_action(action: &str, input: &Value) -> String {
    match action {
        "left_click" | "right_click" | "middle_click" | "double_click" | "triple_click" | "mouse_move" => {
            match coordinate(input, "coordinate") {
                Ok((x, y)) => format!("{action} at ({:.0}, {:.0})", x, y),
                Err(_) => action.to_string(),
            }
        }
        "type" => format!("type \"{}\"", input.get("text").and_then(Value::as_str).unwrap_or("")),
        "key" => format!("press {}", input.get("text").and_then(Value::as_str).unwrap_or("")),
        "scroll" => format!("scroll {}", input.get("scroll_direction").and_then(Value::as_str).unwrap_or("")),
        "wait" => "wait".to_string(),
        other => other.to_string(),
    }
}

fn image_block(base64: &str) -> Value {
    json!({
        "type": "image",
        "source": {"type": "base64", "media_type": "image/jpeg", "data": base64}
    })
}

// ---- Image pruning ----

/// Replaces all but the most recent `keep` image blocks (top-level and inside tool_result content)
/// with a short text placeholder, so the conversation doesn't accumulate megabytes of screenshots.
fn prune_images(conversation: &mut [Value], keep: usize) {
    let total = count_images(conversation);
    if total <= keep {
        return;
    }
    let mut to_remove = total - keep;

    for message in conversation.iter_mut() {
        if to_remove == 0 {
            break;
        }
        let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for block in content.iter_mut() {
            if to_remove == 0 {
                break;
            }
            match block.get("type").and_then(Value::as_str) {
                Some("image") => {
                    *block = placeholder();
                    to_remove -= 1;
                }
                Some("tool_result") => {
                    if let Some(inner) = block.get_mut("content").and_then(Value::as_array_mut) {
                        for ib in inner.iter_mut() {
                            if to_remove == 0 {
                                break;
                            }
                            if ib.get("type").and_then(Value::as_str) == Some("image") {
                                *ib = placeholder();
                                to_remove -= 1;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn count_images(conversation: &[Value]) -> usize {
    let mut count = 0;
    for message in conversation {
        let Some(content) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("image") => count += 1,
                Some("tool_result") => {
                    if let Some(inner) = block.get("content").and_then(Value::as_array) {
                        count += inner.iter()
                            .filter(|b| b.get("type").and_then(Value::as_str) == Some("image"))
                            .count();
                    }
                }
                _ => {}
            }
        }
    }
    count
}

fn placeholder() -> Value {
    json!({"type": "text", "text": "[older screenshot omitted]"})
}

// ---- Recording summary ----

fn build_recording_summary(session: &SessionRecord, events: &[RawEventRecord]) -> String {
    let description = session.description.as_deref().unwrap_or("(no description)");
    let mut steps: Vec<String> = Vec::new();
    let mut last_app: Option<String> = None;
    // Accumulates printable keystrokes so we can report the actual typed string (e.g.
    // "/remote-control") instead of a generic "Typed text".
    let mut typed_buffer = String::new();

    let flush_typed = |steps: &mut Vec<String>, buffer: &mut String| {
        let text = buffer.trim();
        if !text.is_empty() {
            steps.push(format!("Typed \"{text}\""));
        }
        buffer.clear();
    };

    for event in events {
        let envelope: Value = match serde_json::from_str(&event.event_json) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let payload = envelope.get("payload").cloned().unwrap_or_else(|| json!({}));

        // Any non-typing event ends the current typed run.
        if event.event_type != "key_down" && event.event_type != "key_up" {
            flush_typed(&mut steps, &mut typed_buffer);
        }

        match event.event_type.as_str() {
            "frontmost_app_changed" => {
                let name = payload.get("name").or(payload.get("bundleId"))
                    .and_then(Value::as_str).unwrap_or("unknown app");
                if last_app.as_deref() != Some(name) {
                    steps.push(format!("Switched to {name}"));
                    last_app = Some(name.to_string());
                }
            }
            "ax_snapshot" => {
                // The recorder captures title/description/identifier/role; use the most specific
                // text available so clicks on named items (a session row, a button) carry intent
                // — not just "Clicked" or a bare "AXScrollArea".
                let role = payload.get("role").and_then(Value::as_str).unwrap_or("element");
                let label = ["title", "description", "identifier"].iter()
                    .find_map(|k| payload.get(*k).and_then(Value::as_str).filter(|s| !s.is_empty()));
                match label {
                    Some(text) => steps.push(format!("Clicked {role} \"{text}\"")),
                    None => steps.push(format!("Clicked {role}")),
                }
            }
            "key_down" => {
                // Normalized events use camelCase (`keyCode`); accept snake_case too for safety.
                let code = payload.get("keyCode").or_else(|| payload.get("key_code"))
                    .and_then(Value::as_i64).unwrap_or(-1);
                match code {
                    36 => { // Return — submits the field; surface it so the agent knows to confirm
                        flush_typed(&mut steps, &mut typed_buffer);
                        steps.push("Pressed Return".to_string());
                    }
                    48 => { // Tab — e.g. accepting a slash-command autocomplete
                        flush_typed(&mut steps, &mut typed_buffer);
                        steps.push("Pressed Tab".to_string());
                    }
                    53 => { // Escape
                        flush_typed(&mut steps, &mut typed_buffer);
                        steps.push("Pressed Escape".to_string());
                    }
                    51 => { typed_buffer.pop(); } // Backspace
                    _ => if let Some(c) = keycode_to_char(code) { typed_buffer.push(c); }
                }
            }
            _ => {}
        }
    }
    flush_typed(&mut steps, &mut typed_buffer);

    // Collapse consecutive identical steps (e.g. repeated bare "Clicked AXScrollArea").
    let mut deduped: Vec<String> = Vec::new();
    for step in steps {
        if deduped.last() == Some(&step) {
            continue;
        }
        deduped.push(step);
    }

    format!(
        "User's description: {description}\n\nRecording steps ({} actions observed):\n{}",
        deduped.len(),
        deduped.iter().enumerate()
            .map(|(i, s)| format!("{}. {s}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Maps a macOS virtual keycode to the printable character it produces on a US layout (unshifted).
/// Used to reconstruct typed text from recorded key events, which capture only the keycode.
/// Returns None for modifiers and non-printable keys (Return/Tab/Escape/Delete are handled by the
/// caller).
fn keycode_to_char(code: i64) -> Option<char> {
    let c = match code {
        0 => 'a', 1 => 's', 2 => 'd', 3 => 'f', 4 => 'h', 5 => 'g', 6 => 'z', 7 => 'x',
        8 => 'c', 9 => 'v', 11 => 'b', 12 => 'q', 13 => 'w', 14 => 'e', 15 => 'r', 16 => 'y',
        17 => 't', 31 => 'o', 32 => 'u', 34 => 'i', 35 => 'p', 37 => 'l', 38 => 'j', 40 => 'k',
        45 => 'n', 46 => 'm',
        18 => '1', 19 => '2', 20 => '3', 21 => '4', 23 => '5', 22 => '6', 26 => '7', 28 => '8',
        25 => '9', 29 => '0',
        27 => '-', 24 => '=', 33 => '[', 30 => ']', 42 => '\\', 41 => ';', 39 => '\'',
        43 => ',', 47 => '.', 44 => '/', 50 => '`',
        49 => ' ',
        _ => return None,
    };
    Some(c)
}

fn load_recording_keyframes(events: &[RawEventRecord]) -> Vec<(String, String)> {
    let frame_events: Vec<_> = events.iter()
        .filter(|e| e.event_type == "screen_frame")
        .collect();

    if frame_events.is_empty() {
        return Vec::new();
    }

    // Sample evenly across the recording
    let step = frame_events.len().max(1) / MAX_RECORDING_KEYFRAMES.min(frame_events.len()).max(1);
    let mut keyframes = Vec::new();

    for (i, event) in frame_events.iter().enumerate() {
        if i % step.max(1) != 0 && keyframes.len() < MAX_RECORDING_KEYFRAMES {
            continue;
        }
        if keyframes.len() >= MAX_RECORDING_KEYFRAMES {
            break;
        }

        let envelope: Value = match serde_json::from_str(&event.event_json) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let path = match envelope.get("payload").and_then(|p| p.get("path")).and_then(Value::as_str) {
            Some(p) => p,
            None => continue,
        };

        if let Ok(bytes) = std::fs::read(path) {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let caption = format!("Recording keyframe {} of {}", i + 1, frame_events.len());
            keyframes.push((b64, caption));
        }
    }

    keyframes
}

// ---- Prompt building ----

fn build_system_prompt() -> String {
    r#"You are a macOS desktop automation agent. You control the computer with the `computer` tool — taking screenshots and issuing mouse and keyboard actions to complete a task.

You may have been shown a recording of the user performing this task manually. If a recording walkthrough is included below, use it as a guide for the goal and the rough sequence of steps — but always adapt to what you actually see on screen (windows, positions, and content may differ). If no recording is included, work the task out from the description and what you see on screen.

RULES:
- Take ONE action at a time, then look at the resulting screenshot before deciding the next action.
- A fresh screenshot is returned automatically after every action — you do NOT need to call the `screenshot` action just to see the result. Only screenshot to re-check the screen when you did not act. Be decisive and avoid redundant moves; every action is a slow round-trip.
- Coordinates are in the screenshot's pixel space; click precisely on the element you can see.
- The screenshot shows ALL of the user's displays composited side by side, so it may be a wide image spanning multiple monitors (black regions are gaps between displays). Scan the whole image — the window you need may be on a different monitor than you expect, not just the left portion.
- Because the image spans all monitors it is downscaled, so small text can be hard to read. When you need to read fine detail to make a decision (e.g. distinguishing enabled vs. disabled or active vs. faded/archived items, small labels), use the `zoom` action on that region first to see it at full resolution, then act.
- After navigating or opening something, use the `wait` action to let the UI load before acting.
- Prefer keyboard shortcuts where they are reliable (e.g. cmd+t new tab, cmd+l address bar, cmd+c/cmd+v).
- To type into a field, click it first, then use the `type` action.
- If an action doesn't produce the expected result, try a different approach rather than repeating it.
- When the task is fully complete, stop and briefly state what you accomplished (do not call the tool again)."#
        .to_string()
}

fn build_first_user_message(
    task: &str,
    recording_summary: Option<&str>,
    keyframes: &[(String, String)],
) -> Vec<Value> {
    let mut content: Vec<Value> = Vec::new();

    let mut intro = format!("## Task\n\n{task}");
    if let Some(summary) = recording_summary {
        intro.push_str(&format!("\n\n## How I did it when I recorded it\n\n{summary}"));
    }
    content.push(json!({"type": "text", "text": intro}));

    if !keyframes.is_empty() {
        content.push(json!({"type": "text", "text": "\n## Key moments from my recording:"}));
        for (b64, caption) in keyframes {
            content.push(image_block(b64));
            content.push(json!({"type": "text", "text": caption}));
        }
    }

    content
}

// ---- Claude API (computer use) ----

fn tool_definitions(display_width: u32, display_height: u32) -> Vec<Value> {
    vec![json!({
        "type": COMPUTER_TOOL_TYPE,
        "name": "computer",
        "display_width_px": display_width,
        "display_height_px": display_height,
        "display_number": 1,
        // Lets the model inspect a region at full resolution to read small/faded text — important
        // since the multi-display composite is downscaled.
        "enable_zoom": true,
    })]
}

#[derive(Deserialize)]
struct ClaudeApiResponse {
    content: Vec<Value>,
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize)]
struct ClaudeUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

struct ClaudeResult {
    content: Vec<Value>,
    input_tokens: u64,
    output_tokens: u64,
}

async fn call_claude(
    api_key: &str,
    system_prompt: &str,
    tools: &[Value],
    conversation: &[Value],
) -> Result<ClaudeResult, String> {
    let client = reqwest::Client::new();

    let body = json!({
        "model": ANTHROPIC_MODEL,
        "max_tokens": MAX_RESPONSE_TOKENS,
        "system": system_prompt,
        "tools": tools,
        "thinking": {"type": "adaptive"},
        "output_config": {"effort": EFFORT},
        "messages": conversation,
    });

    let response = client
        .post(ANTHROPIC_API_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_API_VERSION)
        .header("anthropic-beta", ANTHROPIC_BETA)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("API request failed: {e}"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Claude API error ({status}): {body}"));
    }

    let parsed: ClaudeApiResponse = response.json().await
        .map_err(|e| format!("failed to parse API response: {e}"))?;

    Ok(ClaudeResult {
        content: parsed.content,
        input_tokens: parsed.usage.as_ref().and_then(|u| u.input_tokens).unwrap_or(0),
        output_tokens: parsed.usage.as_ref().and_then(|u| u.output_tokens).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{RawEventRecord, SessionRecord};

    fn session(description: &str) -> SessionRecord {
        SessionRecord {
            id: 1,
            external_id: "sess_test".to_string(),
            label: None,
            description: Some(description.to_string()),
            started_at_ms: 0,
            ended_at_ms: None,
            status: "completed".to_string(),
            app_transition_count: 0,
            ax_snapshot_count: 0,
            keyframe_count_cached: 0,
            last_error: None,
            created_at_ms: 0,
        }
    }

    fn key_event(seq: i64, key_code: i64) -> RawEventRecord {
        RawEventRecord {
            id: seq,
            session_id: 1,
            sequence: seq,
            event_type: "key_down".to_string(),
            event_json: format!(
                r#"{{"type":"key_down","payload":{{"keyCode":{key_code},"x":0,"y":0}}}}"#
            ),
            recorded_at_ms: 0,
            created_at_ms: 0,
        }
    }

    #[test]
    fn reconstructs_typed_text_from_camelcase_keycode() {
        // "/remot" then Tab then Return — the slash-command autocomplete + submit flow.
        let codes = [44, 15, 14, 46, 31, 17, 48, 36];
        let events: Vec<RawEventRecord> = codes.iter().enumerate()
            .map(|(i, &c)| key_event(i as i64, c))
            .collect();

        let summary = build_recording_summary(&session("turn on remote control"), &events);
        assert!(summary.contains("Typed \"/remot\""), "summary was: {summary}");
        assert!(summary.contains("Pressed Tab"), "summary was: {summary}");
        assert!(summary.contains("Pressed Return"), "summary was: {summary}");
    }

    #[test]
    fn keycode_map_covers_command_characters() {
        assert_eq!(keycode_to_char(44), Some('/'));
        assert_eq!(keycode_to_char(27), Some('-'));
        assert_eq!(keycode_to_char(15), Some('r'));
        assert_eq!(keycode_to_char(58), None); // modifier → not printable
    }
}
