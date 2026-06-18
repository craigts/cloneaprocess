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
// Thinking depth / token spend. `high` favours reliability over latency for autonomous control.
const EFFORT: &str = "high";
const MAX_RESPONSE_TOKENS: u32 = 8192;

const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(5);
const ACTION_TIMEOUT: Duration = Duration::from_secs(6);
const DEFAULT_MAX_STEPS: u32 = 50;
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
    pub session_id: i64,
    pub max_steps: u32,
    pub api_key: String,
    pub cancel_token: Arc<AtomicBool>,
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
    let session = storage.get_session(config.session_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("session {} not found", config.session_id))?;

    let events = storage
        .list_raw_events_for_session(config.session_id, 200)
        .map_err(|e| e.to_string())?;

    let recording_summary = build_recording_summary(&session, &events);
    let recording_keyframes = load_recording_keyframes(&events);

    let mut runner = RunnerBridge::spawn(runner_binary)
        .map_err(|e| format!("failed to start runner: {e}"))?;

    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| format!("failed to create async runtime: {e}"))?;

    let max_steps = if config.max_steps == 0 { DEFAULT_MAX_STEPS } else { config.max_steps };
    let mut step_number: u32 = 0;
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;

    // Take an initial screenshot. Its (logical-point) dimensions define the computer tool's
    // display size so the model's coordinates land 1:1 on our clicks.
    let initial = runner.take_screenshot(SCREENSHOT_TIMEOUT)
        .map_err(|e| format!("initial screenshot failed: {e}"))?;
    eprintln!(
        "[agent] display {}x{} logical points (backing scale {:.1}); clicks map 1:1",
        initial.width, initial.height, initial.scale
    );
    emit(AgentEvent::Screenshot {
        step_number,
        base64: initial.base64.clone(),
        width: initial.width,
        height: initial.height,
    });

    let tools = tool_definitions(initial.width, initial.height);
    let system_prompt = build_system_prompt();

    // Seed the conversation with the task, the recording walkthrough, and the live screen.
    let mut first_content = build_first_user_message(&session, &recording_summary, &recording_keyframes);
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
            emit(AgentEvent::Completed {
                step_number,
                summary: if text.trim().is_empty() { "Task completed.".to_string() } else { text },
                total_input_tokens,
                total_output_tokens,
            });
            return Ok(());
        }

        let mut tool_results: Vec<Value> = Vec::new();

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

            let exec = execute_computer_action(&mut runner, &action, &input);

            // Let the UI settle, then capture the result so the model sees the consequence.
            std::thread::sleep(Duration::from_millis(ACTION_SETTLE_MS));
            let shot = runner.take_screenshot(SCREENSHOT_TIMEOUT).ok();
            if let Some(s) = &shot {
                emit(AgentEvent::Screenshot {
                    step_number,
                    base64: s.base64.clone(),
                    width: s.width,
                    height: s.height,
                });
            }

            let mut content: Vec<Value> = Vec::new();
            let is_error = exec.is_err();
            match &exec {
                Ok(_) => {
                    emit(AgentEvent::ActionResult { step_number, success: true, error: None });
                }
                Err(err) => {
                    emit(AgentEvent::ActionResult { step_number, success: false, error: Some(err.clone()) });
                    content.push(json!({"type": "text", "text": format!("Action failed: {err}")}));
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

            let mut result = json!({
                "type": "tool_result",
                "tool_use_id": tool_id,
                "content": content,
            });
            if is_error {
                result["is_error"] = json!(true);
            }
            tool_results.push(result);
        }

        conversation.push(json!({"role": "user", "content": tool_results}));
        step_number += 1;
    }
}

// ---- Tool execution ----

fn execute_computer_action(runner: &mut RunnerBridge, action: &str, input: &Value) -> Result<(), String> {
    // `screenshot` and `cursor_position` need no runner step — the screenshot we take after every
    // action is the feedback the model is asking for.
    if action == "screenshot" || action == "cursor_position" {
        return Ok(());
    }

    let step_json = match action {
        "left_click" => {
            let (x, y) = coordinate(input, "coordinate")?;
            json!({"kind": "clickAt", "x": x, "y": y, "button": "left", "clickCount": 1, "modifiers": modifiers_from_text(input)})
        }
        "right_click" => {
            let (x, y) = coordinate(input, "coordinate")?;
            json!({"kind": "clickAt", "x": x, "y": y, "button": "right", "clickCount": 1, "modifiers": modifiers_from_text(input)})
        }
        "middle_click" => {
            let (x, y) = coordinate(input, "coordinate")?;
            json!({"kind": "clickAt", "x": x, "y": y, "button": "middle", "clickCount": 1, "modifiers": modifiers_from_text(input)})
        }
        "double_click" => {
            let (x, y) = coordinate(input, "coordinate")?;
            json!({"kind": "clickAt", "x": x, "y": y, "button": "left", "clickCount": 2, "modifiers": modifiers_from_text(input)})
        }
        "triple_click" => {
            let (x, y) = coordinate(input, "coordinate")?;
            json!({"kind": "clickAt", "x": x, "y": y, "button": "left", "clickCount": 3, "modifiers": modifiers_from_text(input)})
        }
        "mouse_move" => {
            let (x, y) = coordinate(input, "coordinate")?;
            json!({"kind": "moveMouse", "x": x, "y": y})
        }
        "left_click_drag" => {
            let (fx, fy) = coordinate(input, "start_coordinate")?;
            let (tx, ty) = coordinate(input, "coordinate")?;
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
            let (x, y) = coordinate(input, "coordinate")?;
            let direction = input.get("scroll_direction").and_then(Value::as_str).unwrap_or("down");
            let amount = input.get("scroll_amount").and_then(Value::as_i64).unwrap_or(3);
            json!({"kind": "scroll", "x": x, "y": y, "direction": direction, "amount": amount, "modifiers": modifiers_from_text(input)})
        }
        "wait" => {
            let secs = input.get("duration").and_then(Value::as_f64).unwrap_or(1.0);
            json!({"kind": "delay", "ms": (secs * 1000.0) as u64})
        }
        other => return Err(format!("unsupported computer action: {other}")),
    };

    let request = RunnerStepRequest {
        workflow_id: "agent".to_string(),
        outer_run_id: "agent_run".to_string(),
        step_index: 0,
        attempt: 1,
        operation_label: action.to_string(),
        step: step_json,
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
    let mut steps = Vec::new();
    let mut last_app: Option<String> = None;

    for event in events {
        let envelope: Value = match serde_json::from_str(&event.event_json) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let payload = envelope.get("payload").cloned().unwrap_or_else(|| json!({}));

        match event.event_type.as_str() {
            "frontmost_app_changed" => {
                let name = payload.get("name").or(payload.get("bundleId"))
                    .and_then(Value::as_str).unwrap_or("unknown app");
                if last_app.as_deref() != Some(name) {
                    steps.push(format!("Switched to {name}"));
                    last_app = Some(name.to_string());
                }
            }
            "mouse_down" => {
                steps.push("Clicked".to_string());
            }
            "ax_snapshot" => {
                let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
                let title = payload.get("title").and_then(Value::as_str).unwrap_or("");
                if !title.is_empty() {
                    steps.push(format!("Interacted with {role} \"{title}\""));
                }
            }
            "key_down" => {
                if steps.last().map(|s| s.starts_with("Typed")).unwrap_or(false) {
                    // Already noted typing
                } else {
                    steps.push("Typed text".to_string());
                }
            }
            _ => {}
        }
    }

    // Deduplicate consecutive "Clicked" entries
    let mut deduped = Vec::new();
    for step in &steps {
        if step == "Clicked" && deduped.last().map(|s: &String| s == "Clicked").unwrap_or(false) {
            continue;
        }
        deduped.push(step.clone());
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

You have been shown a recording of the user performing this task manually. Use it as a guide for the goal and the rough sequence of steps, but always adapt to what you actually see on screen — windows, positions, and content may differ from the recording.

RULES:
- Take ONE action at a time, then look at the resulting screenshot before deciding the next action.
- Coordinates are in the screenshot's pixel space; click precisely on the element you can see.
- After navigating or opening something, use the `wait` action to let the UI load before acting.
- Prefer keyboard shortcuts where they are reliable (e.g. cmd+t new tab, cmd+l address bar, cmd+c/cmd+v).
- To type into a field, click it first, then use the `type` action.
- If an action doesn't produce the expected result, try a different approach rather than repeating it.
- When the task is fully complete, stop and briefly state what you accomplished (do not call the tool again)."#
        .to_string()
}

fn build_first_user_message(
    session: &SessionRecord,
    recording_summary: &str,
    keyframes: &[(String, String)],
) -> Vec<Value> {
    let mut content: Vec<Value> = Vec::new();

    let description = session.description.as_deref().unwrap_or("(no description provided)");
    content.push(json!({"type": "text", "text": format!(
        "## Task\n\n{description}\n\n## How I did it when I recorded it\n\n{recording_summary}"
    )}));

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
