use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::core::runner::{RunnerBridge, RunnerStepExecutor, RunnerStepRequest};
use crate::storage::{RawEventRecord, SessionRecord, Storage};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_MODEL: &str = "claude-sonnet-4-20250514";
const ANTHROPIC_API_VERSION: &str = "2023-06-01";
const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(5);
const ACTION_TIMEOUT: Duration = Duration::from_secs(6);
const DEFAULT_MAX_STEPS: u32 = 50;
const ACTION_SETTLE_MS: u64 = 300;
const MAX_RECORDING_KEYFRAMES: usize = 4;

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
    let mut conversation: Vec<Value> = Vec::new();
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;

    // Build initial messages
    let system_prompt = build_system_prompt();
    let first_user_message = build_first_user_message(
        &session,
        &recording_summary,
        &recording_keyframes,
    );

    loop {
        // Check cancellation
        if config.cancel_token.load(Ordering::Relaxed) {
            emit(AgentEvent::Cancelled { step_number });
            return Ok(());
        }

        // Check step limit
        if step_number >= max_steps {
            emit(AgentEvent::Failed {
                step_number,
                error: format!("reached maximum step limit ({max_steps})"),
            });
            return Ok(());
        }

        // 1. Take screenshot
        let screenshot = runner.take_screenshot(SCREENSHOT_TIMEOUT)
            .map_err(|e| format!("screenshot failed: {e}"))?;

        emit(AgentEvent::Screenshot {
            step_number,
            base64: screenshot.base64.clone(),
            width: screenshot.width,
            height: screenshot.height,
        });

        // 2. Build the user turn with the screenshot
        let user_turn = if step_number == 0 {
            // First turn: include recording context + first screenshot
            let mut content = first_user_message.clone();
            content.push(json!({"type": "text", "text": "\nHere is the current state of the screen:"}));
            content.push(json!({
                "type": "image",
                "source": {"type": "base64", "media_type": "image/jpeg", "data": screenshot.base64}
            }));
            content.push(json!({"type": "text", "text": "What should I do first?"}));
            json!({"role": "user", "content": content})
        } else {
            // Subsequent turns: just screenshot + result of previous action
            let prev_result_text = conversation.last()
                .and_then(|m| m.get("content"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();

            json!({"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": screenshot.base64}},
                {"type": "text", "text": if prev_result_text.is_empty() {
                    "Here is the current screen. What should I do next?".to_string()
                } else {
                    format!("{prev_result_text}\n\nHere is the current screen. What should I do next?")
                }}
            ]})
        };

        // Add user turn to conversation (replace previous user message content to manage context)
        conversation.push(user_turn);

        // 3. Call Claude
        emit(AgentEvent::Thinking { step_number });

        let api_result = runtime.block_on(call_claude_with_tools(
            &config.api_key,
            &system_prompt,
            &conversation,
        )).map_err(|e| format!("Claude API error: {e}"))?;

        total_input_tokens += api_result.input_tokens;
        total_output_tokens += api_result.output_tokens;

        // Add assistant response to conversation
        conversation.push(json!({"role": "assistant", "content": api_result.content.clone()}));

        // 4. Process response
        let tool_use = api_result.content.iter()
            .filter_map(|block| {
                if block.get("type").and_then(Value::as_str) == Some("tool_use") {
                    Some(block.clone())
                } else {
                    None
                }
            })
            .next();

        let Some(tool_call) = tool_use else {
            // No tool call — check if Claude said "done" in text
            let text = api_result.content.iter()
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            emit(AgentEvent::Completed {
                step_number,
                summary: text,
                total_input_tokens,
                total_output_tokens,
            });
            return Ok(());
        };

        let tool_name = tool_call.get("name").and_then(Value::as_str).unwrap_or("");
        let tool_id = tool_call.get("id").and_then(Value::as_str).unwrap_or("");
        let tool_input = tool_call.get("input").cloned().unwrap_or_else(|| json!({}));
        let description = tool_input.get("description").and_then(Value::as_str).unwrap_or(tool_name).to_string();

        // Handle "done" tool
        if tool_name == "done" {
            let summary = tool_input.get("summary").and_then(Value::as_str).unwrap_or("Task completed.").to_string();
            // Add tool result to conversation
            conversation.push(json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": tool_id, "content": "Task marked as complete."}
            ]}));
            emit(AgentEvent::Completed {
                step_number,
                summary,
                total_input_tokens,
                total_output_tokens,
            });
            return Ok(());
        }

        emit(AgentEvent::Action {
            step_number,
            tool: tool_name.to_string(),
            description: description.clone(),
            params: tool_input.clone(),
        });

        // 5. Execute the action
        let action_result = execute_tool_action(&mut runner, tool_name, &tool_input);

        match &action_result {
            Ok(_) => {
                emit(AgentEvent::ActionResult { step_number, success: true, error: None });
                // Add tool result to conversation
                conversation.push(json!({"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": tool_id, "content": format!("Action '{description}' succeeded.")}
                ]}));
            }
            Err(err) => {
                emit(AgentEvent::ActionResult { step_number, success: false, error: Some(err.clone()) });
                // Add error to conversation so Claude can adapt
                conversation.push(json!({"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": tool_id, "is_error": true, "content": format!("Action failed: {err}")}
                ]}));
            }
        }

        // 6. Brief pause for UI to settle
        std::thread::sleep(Duration::from_millis(ACTION_SETTLE_MS));
        step_number += 1;
    }
}

// ---- Tool execution ----

fn execute_tool_action(runner: &mut RunnerBridge, tool_name: &str, input: &Value) -> Result<(), String> {
    let step_json = match tool_name {
        "click_at" => json!({
            "kind": "clickAt",
            "x": input.get("x").and_then(Value::as_f64).unwrap_or(0.0),
            "y": input.get("y").and_then(Value::as_f64).unwrap_or(0.0),
        }),
        "right_click_at" => json!({
            "kind": "rightClickAt",
            "x": input.get("x").and_then(Value::as_f64).unwrap_or(0.0),
            "y": input.get("y").and_then(Value::as_f64).unwrap_or(0.0),
        }),
        "type_text" => {
            let text = input.get("text").and_then(Value::as_str).unwrap_or("");
            json!({ "kind": "setText", "value": {"kind": "literal", "value": text} })
        }
        "press_key" => json!({
            "kind": "keyPress",
            "key": input.get("key").and_then(Value::as_str).unwrap_or(""),
            "modifiers": input.get("modifiers").cloned().unwrap_or_else(|| json!([])),
        }),
        "focus_app" => json!({
            "kind": "focusWindow",
            "bundleId": input.get("bundle_id").and_then(Value::as_str).unwrap_or(""),
        }),
        "wait" => {
            let secs = input.get("seconds").and_then(Value::as_f64).unwrap_or(1.0);
            json!({ "kind": "delay", "ms": (secs * 1000.0) as u64 })
        }
        _ => return Err(format!("unknown tool: {tool_name}")),
    };

    let request = RunnerStepRequest {
        workflow_id: "agent".to_string(),
        outer_run_id: "agent_run".to_string(),
        step_index: 0,
        attempt: 1,
        operation_label: tool_name.to_string(),
        step: step_json,
    };

    runner.execute_step(&request, ACTION_TIMEOUT)
        .map(|_: crate::core::runner::RunnerStepResult| ())
        .map_err(|e: crate::core::runner::RunnerError| e.to_string())
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
                // Don't list every keystroke, just note text entry
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
    r#"You are a macOS desktop automation agent. You can see the user's screen and perform actions to complete a task.

You have been shown a recording of the user performing this task manually. Use it as a guide for what to do, but adapt based on what you actually see on screen.

RULES:
- Perform exactly ONE action per turn using the provided tools
- Always look at the screenshot carefully before acting
- Click precisely on UI elements you can see — use the x,y coordinates from the screenshot
- If an action doesn't produce the expected result, try a different approach
- When the task is complete, call the "done" tool
- For web page content (inside browsers), prefer click_at with coordinates over AX selectors
- After navigating to a new page, use wait(2-3 seconds) to let it load before clicking
- For context menus, use right_click_at, then wait(0.5), then click_at on the menu item you see
- Common keyboard shortcuts: Cmd+T (new tab), Cmd+L (address bar), Cmd+N (new window), Cmd+W (close tab)"#
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

    // Add keyframe images from the recording
    if !keyframes.is_empty() {
        content.push(json!({"type": "text", "text": "\n## Key moments from my recording:"}));
        for (b64, caption) in keyframes {
            content.push(json!({
                "type": "image",
                "source": {"type": "base64", "media_type": "image/jpeg", "data": b64}
            }));
            content.push(json!({"type": "text", "text": caption}));
        }
    }

    content
}

// ---- Claude API with tools ----

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "click_at",
            "description": "Click at screen coordinates (x, y). Use this for clicking UI elements you can see in the screenshot.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "x": {"type": "number", "description": "X coordinate on screen"},
                    "y": {"type": "number", "description": "Y coordinate on screen"},
                    "description": {"type": "string", "description": "What you're clicking on"}
                },
                "required": ["x", "y", "description"]
            }
        }),
        json!({
            "name": "right_click_at",
            "description": "Right-click at screen coordinates to open a context menu.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "x": {"type": "number", "description": "X coordinate on screen"},
                    "y": {"type": "number", "description": "Y coordinate on screen"},
                    "description": {"type": "string", "description": "What you're right-clicking on"}
                },
                "required": ["x", "y", "description"]
            }
        }),
        json!({
            "name": "type_text",
            "description": "Type text into the currently focused input field. Make sure to click on the field first.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "Text to type"},
                    "description": {"type": "string", "description": "What field you're typing into"}
                },
                "required": ["text", "description"]
            }
        }),
        json!({
            "name": "press_key",
            "description": "Press a keyboard key, optionally with modifiers. Keys: a-z, 0-9, return, tab, space, delete, escape, up, down, left, right. Modifiers: cmd, shift, alt, ctrl.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "key": {"type": "string", "description": "Key name"},
                    "modifiers": {"type": "array", "items": {"type": "string"}, "description": "Modifier keys (cmd, shift, alt, ctrl)"},
                    "description": {"type": "string", "description": "What this keystroke does"}
                },
                "required": ["key", "description"]
            }
        }),
        json!({
            "name": "focus_app",
            "description": "Open/focus a macOS application by bundle ID. Common: com.google.Chrome, com.apple.Safari, com.apple.finder, com.apple.Terminal.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "bundle_id": {"type": "string", "description": "macOS bundle identifier"},
                    "description": {"type": "string", "description": "Which app to open"}
                },
                "required": ["bundle_id", "description"]
            }
        }),
        json!({
            "name": "wait",
            "description": "Wait for a specified number of seconds. Use after navigation or actions that cause UI changes.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "seconds": {"type": "number", "description": "Seconds to wait"},
                    "reason": {"type": "string", "description": "Why waiting"}
                },
                "required": ["seconds", "reason"]
            }
        }),
        json!({
            "name": "done",
            "description": "Call this when the task is complete.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "summary": {"type": "string", "description": "Brief summary of what was accomplished"}
                },
                "required": ["summary"]
            }
        }),
    ]
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

async fn call_claude_with_tools(
    api_key: &str,
    system_prompt: &str,
    conversation: &[Value],
) -> Result<ClaudeResult, String> {
    let client = reqwest::Client::new();

    let body = json!({
        "model": ANTHROPIC_MODEL,
        "max_tokens": 1024,
        "system": system_prompt,
        "tools": tool_definitions(),
        "messages": conversation,
    });

    let response = client
        .post(ANTHROPIC_API_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_API_VERSION)
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
