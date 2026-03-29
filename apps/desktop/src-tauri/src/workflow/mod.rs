use serde_json::{json, Value};

use crate::storage::RawEventRecord;

pub const WORKFLOW_IR_VERSION: u32 = 1;

const ELEMENT_WAIT_TIMEOUT_MS: u64 = 1500;

#[derive(Clone, Debug)]
pub struct WorkflowDraft {
    pub workflow: Value,
    pub step_count: usize,
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

pub fn compile_workflow(session_id: i64, workflow_name: String, events: &[RawEventRecord]) -> Result<WorkflowDraft, String> {
    let mut context = DraftContext {
        last_bundle_id: None,
        pending_snapshot: None,
        active_text_entry: None,
        steps: Vec::new(),
    };

    for event in events {
        let envelope: Value = serde_json::from_str(&event.event_json)
            .map_err(|error| format!("failed to parse event {}: {}", event.id, error))?;
        let payload = envelope
            .get("payload")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        match event.event_type.as_str() {
            "frontmost_app_changed" => {
                flush_text_entry(&mut context);
                if let Some(bundle_id) = payload.get("bundle_id").and_then(Value::as_str) {
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
                    let key_code = payload.get("key_code").and_then(Value::as_i64).unwrap_or(-1) as i32;
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

    Ok(WorkflowDraft { workflow, step_count })
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
    use super::compile_workflow;
    use crate::storage::RawEventRecord;
    use serde_json::json;

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
        assert_eq!(steps[0], json!({ "kind": "focusWindow", "bundleId": "com.apple.TextEdit" }));
        assert_eq!(steps[1]["kind"], "waitFor");
        assert_eq!(steps[2]["kind"], "click");
        assert_eq!(steps[3]["kind"], "setText");
        assert_eq!(steps[3]["value"]["value"], "hello");
        assert_eq!(steps[4], json!({ "kind": "focusWindow", "bundleId": "com.apple.Safari" }));
    }

    #[test]
    fn ignores_non_editable_key_sequences() {
        let events = vec![
            raw_event(
                1,
                0,
                "ax_snapshot",
                json!({
                    "payload": {
                        "role": "AXButton",
                        "selector": {
                            "target_app": { "bundle_id": "com.apple.TextEdit" },
                            "ax": { "role": "AXButton", "title": "Submit" }
                        }
                    }
                }),
            ),
            raw_event(2, 1, "mouse_down", json!({ "payload": { "x": 100, "y": 100 } })),
            raw_event(3, 2, "key_down", json!({ "payload": { "key_code": 12 } })),
        ];

        let workflow = compile_workflow(7, "No text".to_string(), &events)
            .expect("workflow should compile");
        let steps = workflow
            .workflow
            .get("steps")
            .and_then(|value| value.as_array())
            .expect("steps array should exist");

        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0]["kind"], "waitFor");
        assert_eq!(steps[1]["kind"], "click");
    }

    fn sample_events() -> Vec<RawEventRecord> {
        vec![
            raw_event(
                1,
                0,
                "frontmost_app_changed",
                json!({ "payload": { "bundle_id": "com.apple.TextEdit" } }),
            ),
            raw_event(
                2,
                1,
                "ax_snapshot",
                json!({
                    "payload": {
                        "role": "AXTextField",
                        "selector": {
                            "target_app": { "bundle_id": "com.apple.TextEdit" },
                            "ax": { "role": "AXTextField", "title": "Name" }
                        }
                    }
                }),
            ),
            raw_event(3, 2, "mouse_down", json!({ "payload": { "x": 120, "y": 220 } })),
            raw_event(4, 3, "key_down", json!({ "payload": { "key_code": 4 } })),
            raw_event(5, 4, "key_down", json!({ "payload": { "key_code": 14 } })),
            raw_event(6, 5, "key_down", json!({ "payload": { "key_code": 37 } })),
            raw_event(7, 6, "key_down", json!({ "payload": { "key_code": 37 } })),
            raw_event(8, 7, "key_down", json!({ "payload": { "key_code": 31 } })),
            raw_event(
                9,
                8,
                "frontmost_app_changed",
                json!({ "payload": { "bundle_id": "com.apple.Safari" } }),
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

    use serde_json::Value;
}
