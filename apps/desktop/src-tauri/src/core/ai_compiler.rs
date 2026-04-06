use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::storage::{RawEventRecord, SessionRecord, Storage};
use crate::workflow::{compile_workflow, WorkflowDraft};

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_MODEL: &str = "claude-sonnet-4-20250514";
const ANTHROPIC_API_VERSION: &str = "2023-06-01";
const MAX_EVENTS_IN_PROMPT: usize = 150;
const MAX_AX_SNAPSHOTS_IN_PROMPT: usize = 40;
const MAX_KEYFRAMES_IN_PROMPT: usize = 8;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiCompileResult {
    pub workflow_json: String,
    pub step_count: usize,
    pub model: String,
    pub prompt_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

pub fn ai_compile_workflow(
    storage: &Storage,
    session_id: i64,
) -> Result<AiCompileResult, String> {
    let api_key = resolve_api_key(storage)?;
    let session = storage
        .get_session(session_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("session {} not found", session_id))?;
    let events = storage
        .list_raw_events_for_session(session_id, MAX_EVENTS_IN_PROMPT as i64)
        .map_err(|e| e.to_string())?;

    let v1_draft = compile_workflow(
        session_id,
        session.label.clone().unwrap_or_else(|| "Untitled".to_string()),
        &events,
    )?;

    let prompt = build_prompt(&session, &events, &v1_draft);
    let keyframes = load_keyframe_images(&events);

    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| format!("failed to create async runtime: {e}"))?;

    runtime.block_on(call_claude_api(&api_key, &prompt, &keyframes))
}

fn load_keyframe_images(events: &[RawEventRecord]) -> Vec<(String, String)> {
    use std::fs;

    let mut images = Vec::new();
    for event in events {
        if event.event_type != "screen_frame" { continue; }
        if images.len() >= MAX_KEYFRAMES_IN_PROMPT { break; }

        let envelope: Value = match serde_json::from_str(&event.event_json) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let path = match envelope.get("payload").and_then(|p| p.get("path")).and_then(Value::as_str) {
            Some(p) => p,
            None => continue,
        };

        if let Ok(bytes) = fs::read(path) {
            use std::io::Write;
            let mut b64 = Vec::new();
            {
                let mut encoder = Base64Encoder::new(&mut b64);
                let _ = encoder.write_all(&bytes);
                let _ = encoder.finish();
            }
            if let Ok(b64_str) = String::from_utf8(b64) {
                let caption = format!("Keyframe at event seq {} ({}ms)", event.sequence, event.recorded_at_ms);
                images.push((b64_str, caption));
            }
        }
    }
    images
}

struct Base64Encoder<W: std::io::Write> {
    writer: W,
    buf: [u8; 3],
    buf_len: usize,
}

const B64_CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

impl<W: std::io::Write> Base64Encoder<W> {
    fn new(writer: W) -> Self { Self { writer, buf: [0; 3], buf_len: 0 } }

    fn finish(mut self) -> std::io::Result<()> {
        if self.buf_len > 0 {
            let mut block = [0u8; 3];
            block[..self.buf_len].copy_from_slice(&self.buf[..self.buf_len]);
            let mut out = [b'='; 4];
            out[0] = B64_CHARS[(block[0] >> 2) as usize];
            out[1] = B64_CHARS[((block[0] & 0x03) << 4 | block[1] >> 4) as usize];
            if self.buf_len > 1 {
                out[2] = B64_CHARS[((block[1] & 0x0f) << 2 | block[2] >> 6) as usize];
            }
            self.writer.write_all(&out)?;
        }
        Ok(())
    }
}

impl<W: std::io::Write> std::io::Write for Base64Encoder<W> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        let mut consumed = 0;
        for &byte in data {
            self.buf[self.buf_len] = byte;
            self.buf_len += 1;
            consumed += 1;
            if self.buf_len == 3 {
                let b = self.buf;
                let out = [
                    B64_CHARS[(b[0] >> 2) as usize],
                    B64_CHARS[((b[0] & 0x03) << 4 | b[1] >> 4) as usize],
                    B64_CHARS[((b[1] & 0x0f) << 2 | b[2] >> 6) as usize],
                    B64_CHARS[(b[2] & 0x3f) as usize],
                ];
                self.writer.write_all(&out)?;
                self.buf_len = 0;
            }
        }
        Ok(consumed)
    }

    fn flush(&mut self) -> std::io::Result<()> { self.writer.flush() }
}

fn resolve_api_key(storage: &Storage) -> Result<String, String> {
    // Check app_settings first, then environment variable
    if let Ok(Some(key)) = storage.get_app_setting("anthropic_api_key") {
        if !key.trim().is_empty() {
            return Ok(key.trim().to_string());
        }
    }

    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.trim().is_empty() {
            return Ok(key.trim().to_string());
        }
    }

    Err("No Anthropic API key configured. Set ANTHROPIC_API_KEY or save one in the app settings.".to_string())
}

fn build_prompt(
    session: &SessionRecord,
    events: &[RawEventRecord],
    v1_draft: &WorkflowDraft,
) -> String {
    let description = session
        .description
        .as_deref()
        .unwrap_or("(no description provided)");

    let ax_snapshots: Vec<Value> = events
        .iter()
        .filter(|e| e.event_type == "ax_snapshot")
        .take(MAX_AX_SNAPSHOTS_IN_PROMPT)
        .filter_map(|e| serde_json::from_str::<Value>(&e.event_json).ok())
        .filter_map(|envelope| {
            let payload = envelope.get("payload").cloned()?;
            Some(payload)
        })
        .collect();

    let app_transitions: Vec<Value> = events
        .iter()
        .filter(|e| e.event_type == "frontmost_app_changed")
        .filter_map(|e| serde_json::from_str::<Value>(&e.event_json).ok())
        .filter_map(|envelope| envelope.get("payload").cloned())
        .collect();

    let v1_workflow_json =
        serde_json::to_string_pretty(&v1_draft.workflow).unwrap_or_default();

    format!(
        r#"You are an expert at converting recorded macOS user interactions into clean, reliable automation workflows.

## User's description of what they recorded

{description}

## App transitions during the recording

{app_transitions_json}

## AX snapshots captured during the recording

These are the accessibility elements that were under the cursor at click time:

{ax_snapshots_json}

## V1 compiler output (mechanical, may contain noise)

This is what a rule-based compiler produced from the raw events. It is mechanical — it includes every click and keystroke literally. Use it as a starting point but improve it:

```json
{v1_workflow_json}
```

## macOS automation knowledge

Important platform behaviors to account for:

- **Dock clicks MUST become focusWindow**: The runner CANNOT interact with Dock items or Dock menus via AX. ANY click on a dock item (AXDockItem, com.apple.dock) MUST be converted to a `focusWindow` step. NEVER emit any step targeting `com.apple.dock`, `com.apple.dock.helper`, or `AXDockItem`. This is critical — dock and dock.helper steps will always fail.
- **App launching**: When you see a dock click followed by an app transition, the intent is to open/focus that app. Use `focusWindow` with the app's bundle ID.
- **Chrome profile selection**: If the recording shows a dock right-click to select a Chrome profile, ignore those dock.helper steps entirely. Instead, just use `focusWindow` for Chrome — the profile that's already active will be used.
- **URL navigation**: If the user's description mentions navigating to a URL, use `click` on the address bar (AXTextField/AXWebArea with role AXTextField in the browser), then `setText` with the URL, then a key press step if available. If the recording doesn't capture the exact address bar click, still include the navigation steps.
- **waitFor reliability**: Only use `waitFor` for elements that you are confident will appear. Avoid waiting for transient UI like context menus or tooltips unless the recording clearly shows a right-click interaction.
- **ONLY use selectors from the recording**: NEVER invent or guess AX selectors. Every click and waitFor selector MUST come from the AX snapshots or the V1 compiler output provided above. If the recording doesn't include a snapshot for a particular UI element (like an address bar), do NOT add a step targeting it. Different apps use different AX roles and the only reliable source is what was actually observed.
- **Bundle IDs**: Common ones — Safari: `com.apple.Safari`, Chrome: `com.google.Chrome`, Firefox: `org.mozilla.firefox`, Finder: `com.apple.finder`, Terminal: `com.apple.Terminal`.
- **Prefer keyboard shortcuts over clicking UI elements** when possible. Keyboard shortcuts are more reliable than AX element clicks. Use `keyPress` steps with `key` and `modifiers`. Common patterns:
  - New window: `{{"kind":"keyPress","key":"n","modifiers":["cmd"]}}`
  - New tab: `{{"kind":"keyPress","key":"t","modifiers":["cmd"]}}`
  - Focus address bar: `{{"kind":"keyPress","key":"l","modifiers":["cmd"]}}`
  - Close window/tab: `{{"kind":"keyPress","key":"w","modifiers":["cmd"]}}`
  - Select all: `{{"kind":"keyPress","key":"a","modifiers":["cmd"]}}`
  - Press Enter/Return: `{{"kind":"keyPress","key":"return","modifiers":[]}}`
  - Available keys: a-z, 0-9, return, tab, space, delete, escape, left, right, up, down, f1-f5
  - Available modifiers: cmd, shift, alt, ctrl
- **Add delays after navigation**: After pressing Enter to navigate to a URL, add a `delay` step to let the page load before interacting with page elements. Use `{{"kind":"delay","ms":2000,"description":"Wait for page to load"}}`. Use 2000ms for typical pages, 3000-4000ms for heavy pages. Always add a delay between navigating and clicking page content.
- **Right-click for context menus**: When the user's description mentions right-clicking, or the recording shows a context menu action (like "Open links"), use `rightClickAt` with x,y coordinates to open the context menu, then add a short `delay` (500ms) before clicking the menu item.
- **Use clickAt/rightClickAt for web page content**: AX selectors for elements inside web pages (Chrome, Safari, etc.) are unreliable — they often use generic roles like AXGroup with no title. For clicking on web page content, PREFER `clickAt` or `rightClickAt` with x,y screen coordinates from the recording's AX snapshots or spatial data. Format: `{{"kind":"clickAt","x":825,"y":647,"description":"Click PR links cell"}}` or `{{"kind":"rightClickAt","x":825,"y":647,"description":"Right-click on PR links"}}`.
- **Screenshots are attached**: Keyframe screenshots from the recording are included above. Use them to understand the visual layout and verify that coordinate-based clicks will target the right area. The screenshots show exactly what the user saw during recording.

## Your task

Produce a refined automation workflow JSON that:

1. **Filters noise** — remove accidental clicks, window repositioning, unrelated app switches, and redundant steps
2. **Infers intent** — understand what the user was actually trying to accomplish based on their description
3. **Parameterizes inputs** — extract values like emails, names, URLs into the `inputs` array so the workflow is reusable
4. **Uses clear step names** — add a human-readable `description` field to each step
5. **Preserves the schema** — output must use the same step kinds: `focusWindow`, `click`, `rightClick`, `clickAt`, `rightClickAt`, `setText`, `waitFor`, `keyPress`, `delay`
6. **Be conservative with waitFor** — only wait for elements when a prior action would cause them to appear. Do not wait for elements from context menus or transient popups unless explicitly shown in the recording.
8. **Do NOT add verify blocks to focusWindow** — the focusWindow step uses NSWorkspace.activate() which is reliable. Adding a verify/windowVisible check requires extra AX permissions and frequently times out. Emit focusWindow steps WITHOUT a verify array.
7. **Never invent selectors** — every `selector` in a `click`, `setText`, or `waitFor` step must come verbatim from the AX snapshots or V1 compiler output above. If the recording doesn't include a snapshot for an element, omit that step entirely rather than guessing. Guessed selectors will fail at runtime.

## Output format

Return ONLY a JSON object with this structure (no markdown, no explanation):

```
{{
  "id": "wf_ai_session_<session_id>",
  "name": "<descriptive workflow name>",
  "inputs": [
    {{ "name": "<param_name>", "type": "string", "description": "<what this input is>" }}
  ],
  "steps": [
    {{
      "kind": "focusWindow" | "click" | "rightClick" | "clickAt" | "rightClickAt" | "setText" | "waitFor" | "keyPress" | "delay",
      "description": "<human-readable step description>",
      ... (kind-specific fields matching the v1 schema)
    }}
  ]
}}
```

For `setText` steps, the `value` field MUST be `{{"kind":"literal","value":"the actual text"}}`. Do NOT use kind "input" — always use kind "literal" with the concrete value.
For `keyPress` steps, use `{{"key":"<key_name>","modifiers":["cmd"]}}` (modifiers is an array, can be empty).

Return ONLY the JSON object."#,
        description = description,
        app_transitions_json = serde_json::to_string_pretty(&app_transitions).unwrap_or_default(),
        ax_snapshots_json = serde_json::to_string_pretty(&ax_snapshots).unwrap_or_default(),
        v1_workflow_json = v1_workflow_json,
    )
}

pub fn ai_refine_workflow(
    storage: &Storage,
    workflow_json: &str,
    run_id: i64,
    source_session_id: Option<i64>,
    session_description: Option<&str>,
    user_hint: Option<&str>,
) -> Result<AiCompileResult, String> {
    let api_key = resolve_api_key(storage)?;

    let run_logs = storage
        .list_workflow_run_logs(run_id, 100)
        .map_err(|e| e.to_string())?;

    let logs_text: Vec<String> = run_logs
        .iter()
        .map(|log| {
            let payload: Value = serde_json::from_str(&log.payload_json).unwrap_or(json!({}));
            format!(
                "{}{} — {}",
                log.event_type,
                log.step_index.map(|i| format!(" (step {})", i)).unwrap_or_default(),
                serde_json::to_string_pretty(&payload).unwrap_or_default(),
            )
        })
        .collect();

    // Load original recording context if we have a source session
    let (ax_snapshots_json, keyframes) = if let Some(sid) = source_session_id {
        let events = storage
            .list_raw_events_for_session(sid, MAX_EVENTS_IN_PROMPT as i64)
            .unwrap_or_default();

        let snapshots: Vec<Value> = events.iter()
            .filter(|e| e.event_type == "ax_snapshot")
            .take(MAX_AX_SNAPSHOTS_IN_PROMPT)
            .filter_map(|e| serde_json::from_str::<Value>(&e.event_json).ok())
            .filter_map(|env| env.get("payload").cloned())
            .collect();

        let kf = load_keyframe_images(&events);
        (serde_json::to_string_pretty(&snapshots).unwrap_or_default(), kf)
    } else {
        ("[]".to_string(), Vec::new())
    };

    let prompt = build_refinement_prompt(
        workflow_json,
        &logs_text.join("\n\n"),
        session_description.unwrap_or("(no description)"),
        &ax_snapshots_json,
        user_hint,
    );

    let runtime = tokio::runtime::Runtime::new()
        .map_err(|e| format!("failed to create async runtime: {e}"))?;

    runtime.block_on(call_claude_api(&api_key, &prompt, &keyframes))
}

fn build_refinement_prompt(
    workflow_json: &str,
    run_logs: &str,
    description: &str,
    ax_snapshots_json: &str,
    user_hint: Option<&str>,
) -> String {
    let hint_section = match user_hint {
        Some(hint) if !hint.trim().is_empty() => format!(
            "\n## IMPORTANT: User's hint about what went wrong\n\nThe user says: \"{}\"\n\nThis is the most important piece of information. Follow this guidance precisely when fixing the workflow.\n",
            hint.trim()
        ),
        _ => String::new(),
    };

    format!(
        r#"You are an expert at debugging and fixing macOS automation workflows.

## User's description of what they want to automate

{description}
{hint_section}
## AX snapshots from the original recording

These are the accessibility elements captured during the user's recording. Use them to understand the UI structure, including menu items, submenus, and element hierarchy:

{ax_snapshots_json}

## The workflow that failed

```json
{workflow_json}
```

## Run logs showing what happened

{run_logs}

## Your task

The workflow above was executed and failed. Analyze the run logs AND the original AX snapshots to understand what went wrong, then produce a FIXED version of the workflow.

Common fixes:
- If a selector didn't match, try a different AX role (e.g., `AXMenuItem` instead of `AXStaticText` for context menu items)
- If a step timed out, add a `delay` before it to let the UI settle
- If a click didn't have the intended effect, try using `keyPress` with a keyboard shortcut instead
- If a right-click context menu item wasn't found, the menu may use `AXMenuItem` role, not `AXStaticText`
- If steps happened too fast, add `delay` steps between them

Available step kinds: `focusWindow`, `click`, `rightClick`, `clickAt`, `rightClickAt`, `setText`, `waitFor`, `keyPress`, `delay`
- `keyPress` example: `{{"kind":"keyPress","key":"n","modifiers":["cmd"]}}`
- `delay` example: `{{"kind":"delay","ms":1000}}`
- `setText` value must be: `{{"kind":"literal","value":"text"}}`
- `clickAt` example: `{{"kind":"clickAt","x":825,"y":647}}` — clicks at screen coordinates, bypasses AX
- `rightClickAt` example: `{{"kind":"rightClickAt","x":825,"y":647}}` — right-clicks at coordinates
- If a selector-based click/rightClick failed, try switching to clickAt/rightClickAt using coordinates from the spatial fallback data in the original workflow
- Keys: a-z, 0-9, return, tab, space, delete, escape, left, right, up, down
- Modifiers: cmd, shift, alt, ctrl

IMPORTANT: Do NOT use selectors targeting `com.apple.dock`, `com.apple.dock.helper`, or `AXDockItem` — these always fail. Use `focusWindow` instead.

Return ONLY the fixed JSON workflow object, no explanation."#,
        description = description,
        workflow_json = workflow_json,
        run_logs = run_logs,
    )
}

#[derive(Deserialize)]
struct ClaudeResponse {
    content: Vec<ContentBlock>,
    model: String,
    usage: Option<UsageInfo>,
}

#[derive(Deserialize)]
struct ContentBlock {
    text: Option<String>,
}

#[derive(Deserialize)]
struct UsageInfo {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

async fn call_claude_api(api_key: &str, prompt: &str, images: &[(String, String)]) -> Result<AiCompileResult, String> {
    let client = reqwest::Client::new();

    let mut content_blocks: Vec<Value> = Vec::new();

    // Add keyframe images first so the AI sees the screenshots before the text
    for (b64_data, caption) in images {
        content_blocks.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": "image/jpeg",
                "data": b64_data,
            }
        }));
        content_blocks.push(json!({
            "type": "text",
            "text": caption,
        }));
    }

    // Add the main text prompt
    content_blocks.push(json!({
        "type": "text",
        "text": prompt,
    }));

    let body = json!({
        "model": ANTHROPIC_MODEL,
        "max_tokens": 4096,
        "messages": [
            {
                "role": "user",
                "content": content_blocks,
            }
        ],
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
        let error_body = response.text().await.unwrap_or_default();
        return Err(format!("Claude API error ({}): {}", status, error_body));
    }

    let claude_response: ClaudeResponse = response
        .json()
        .await
        .map_err(|e| format!("failed to parse API response: {e}"))?;

    let raw_text = claude_response
        .content
        .iter()
        .filter_map(|block| block.text.as_deref())
        .collect::<Vec<_>>()
        .join("");

    let workflow_json = extract_json_from_response(&raw_text)?;
    let workflow: Value = serde_json::from_str(&workflow_json)
        .map_err(|e| format!("AI returned invalid JSON: {e}"))?;

    let step_count = workflow
        .get("steps")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);

    if step_count == 0 {
        return Err("AI returned a workflow with no steps".to_string());
    }

    let prompt_tokens = claude_response
        .usage
        .as_ref()
        .and_then(|u| u.input_tokens);
    let output_tokens = claude_response
        .usage
        .as_ref()
        .and_then(|u| u.output_tokens);

    Ok(AiCompileResult {
        workflow_json: serde_json::to_string_pretty(&workflow)
            .unwrap_or_else(|_| workflow_json),
        step_count,
        model: claude_response.model,
        prompt_tokens,
        output_tokens,
    })
}

fn extract_json_from_response(text: &str) -> Result<String, String> {
    let trimmed = text.trim();

    // If it starts with {, try parsing directly
    if trimmed.starts_with('{') {
        return Ok(trimmed.to_string());
    }

    // Try to extract from markdown code block
    if let Some(start) = trimmed.find("```json") {
        let after_fence = &trimmed[start + 7..];
        if let Some(end) = after_fence.find("```") {
            return Ok(after_fence[..end].trim().to_string());
        }
    }

    if let Some(start) = trimmed.find("```") {
        let after_fence = &trimmed[start + 3..];
        // Skip optional language tag on the same line
        let content_start = after_fence.find('\n').unwrap_or(0);
        let after_tag = &after_fence[content_start..];
        if let Some(end) = after_tag.find("```") {
            return Ok(after_tag[..end].trim().to_string());
        }
    }

    // Last resort: find first { and last }
    if let (Some(start), Some(end)) = (trimmed.find('{'), trimmed.rfind('}')) {
        if end > start {
            return Ok(trimmed[start..=end].to_string());
        }
    }

    Err("Could not extract JSON from AI response".to_string())
}

#[cfg(test)]
mod tests {
    use super::extract_json_from_response;

    #[test]
    fn extracts_raw_json() {
        let input = r#"{"id": "wf_1", "steps": []}"#;
        assert!(extract_json_from_response(input).is_ok());
    }

    #[test]
    fn extracts_json_from_code_block() {
        let input = "Here is the workflow:\n```json\n{\"id\": \"wf_1\", \"steps\": []}\n```\n";
        let result = extract_json_from_response(input).unwrap();
        assert!(result.contains("wf_1"));
    }

    #[test]
    fn extracts_json_with_surrounding_text() {
        let input = "Sure! {\"id\": \"wf_1\", \"steps\": []} Hope this helps!";
        let result = extract_json_from_response(input).unwrap();
        assert!(result.starts_with('{'));
        assert!(result.ends_with('}'));
    }
}
