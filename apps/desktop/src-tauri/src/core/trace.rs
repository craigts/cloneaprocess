use serde_json::{json, Map, Value};

pub const RAW_EVENT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct NormalizedRawEvent {
    pub event_type: String,
    pub event_json: String,
    pub recorded_at_ms: u64,
}

pub fn normalize_raw_event(
    event_type_hint: Option<&str>,
    value: &Value,
    recorded_at_ms_fallback: u64,
) -> Result<NormalizedRawEvent, String> {
    let event_type = value
        .get("type")
        .or_else(|| value.get("event_type"))
        .and_then(Value::as_str)
        .or(event_type_hint)
        .ok_or_else(|| "event envelope missing type".to_string())?
        .to_string();

    let recorded_at_ms = value
        .get("recordedAtMs")
        .or_else(|| value.get("recorded_at_ms"))
        .and_then(Value::as_u64)
        .or_else(|| value.get("ts").and_then(Value::as_u64))
        .unwrap_or(recorded_at_ms_fallback);

    let event_id = value
        .get("eventId")
        .or_else(|| value.get("event_id"))
        .and_then(Value::as_str)
        .or_else(|| value.get("id").and_then(Value::as_str))
        .map(ToOwned::to_owned);

    let source_version = value
        .get("sourceVersion")
        .or_else(|| value.get("source_version"))
        .or_else(|| value.get("schemaVersion"))
        .or_else(|| value.get("schema_version"))
        .and_then(Value::as_u64)
        .or_else(|| value.get("v").and_then(Value::as_u64));

    let payload = extract_payload(value);
    let payload = normalize_payload_keys(&payload);

    let mut normalized = Map::new();
    normalized.insert(
        "schemaVersion".to_string(),
        Value::Number(RAW_EVENT_SCHEMA_VERSION.into()),
    );
    if let Some(source_version) = source_version {
        normalized.insert(
            "sourceVersion".to_string(),
            Value::Number(source_version.into()),
        );
    }
    normalized.insert("type".to_string(), Value::String(event_type.clone()));
    if let Some(event_id) = event_id {
        normalized.insert("eventId".to_string(), Value::String(event_id));
    }
    normalized.insert(
        "recordedAtMs".to_string(),
        Value::Number(recorded_at_ms.into()),
    );
    normalized.insert("payload".to_string(), payload);

    let event_json = serde_json::to_string(&Value::Object(normalized))
        .map_err(|error| format!("failed to serialize normalized event: {}", error))?;

    Ok(NormalizedRawEvent {
        event_type,
        event_json,
        recorded_at_ms,
    })
}

fn extract_payload(value: &Value) -> Value {
    if let Some(payload) = value.get("payload") {
        return payload.clone();
    }

    match value {
        Value::Object(object) => {
            let mut payload = object.clone();
            payload.remove("schemaVersion");
            payload.remove("schema_version");
            payload.remove("sourceVersion");
            payload.remove("source_version");
            payload.remove("type");
            payload.remove("event_type");
            payload.remove("eventId");
            payload.remove("event_id");
            payload.remove("id");
            payload.remove("recordedAtMs");
            payload.remove("recorded_at_ms");
            payload.remove("ts");
            payload.remove("v");
            Value::Object(payload)
        }
        _ => json!({}),
    }
}

fn normalize_payload_keys(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut normalized = Map::new();
            for (key, child) in object {
                normalized.insert(to_camel_case(key), normalize_payload_keys(child));
            }
            Value::Object(normalized)
        }
        Value::Array(items) => Value::Array(items.iter().map(normalize_payload_keys).collect()),
        _ => value.clone(),
    }
}

fn to_camel_case(key: &str) -> String {
    if !key.contains('_') {
        return key.to_string();
    }

    let mut normalized = String::with_capacity(key.len());
    let mut uppercase_next = false;
    for ch in key.chars() {
        if ch == '_' {
            uppercase_next = true;
            continue;
        }
        if uppercase_next {
            normalized.extend(ch.to_uppercase());
            uppercase_next = false;
        } else {
            normalized.push(ch);
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::{normalize_raw_event, RAW_EVENT_SCHEMA_VERSION};
    use serde_json::json;

    #[test]
    fn normalizes_bridge_payload_into_canonical_envelope() {
        let normalized = normalize_raw_event(
            None,
            &json!({
                "v": 1,
                "id": "evt_1",
                "ts": 99,
                "type": "ax_snapshot",
                "payload": {
                    "snapshot_id": "ax_1",
                    "bundle_id": "com.apple.TextEdit",
                    "selector": {
                        "target_app": { "bundle_id": "com.apple.TextEdit" },
                        "ax": { "role": "AXButton", "title": "Save" }
                    }
                }
            }),
            0,
        )
        .expect("normalization should succeed");

        let event: serde_json::Value =
            serde_json::from_str(&normalized.event_json).expect("normalized event should parse");
        assert_eq!(normalized.event_type, "ax_snapshot");
        assert_eq!(normalized.recorded_at_ms, 99);
        assert_eq!(event["schemaVersion"], RAW_EVENT_SCHEMA_VERSION);
        assert_eq!(event["sourceVersion"], 1);
        assert_eq!(event["eventId"], "evt_1");
        assert_eq!(event["recordedAtMs"], 99);
        assert_eq!(event["payload"]["snapshotId"], "ax_1");
        assert_eq!(event["payload"]["bundleId"], "com.apple.TextEdit");
        assert_eq!(
            event["payload"]["selector"]["targetApp"]["bundleId"],
            "com.apple.TextEdit"
        );
    }

    #[test]
    fn wraps_flat_payloads_for_non_bridge_events() {
        let normalized =
            normalize_raw_event(Some("storage_smoke_test"), &json!({ "ok": true }), 55)
                .expect("normalization should succeed");

        let event: serde_json::Value =
            serde_json::from_str(&normalized.event_json).expect("normalized event should parse");
        assert_eq!(event["type"], "storage_smoke_test");
        assert_eq!(event["recordedAtMs"], 55);
        assert_eq!(event["payload"]["ok"], true);
    }

    #[test]
    fn normalizes_snake_case_xpc_event_envelope() {
        let normalized = normalize_raw_event(
            None,
            &json!({
                "schema_version": 1,
                "event_id": "evt_xpc_1",
                "event_type": "screen_frame",
                "recorded_at_ms": 77,
                "payload": {
                    "frame_id": "frm_1",
                    "path": "recordings/sess_1/frames/frm_1.jpg",
                }
            }),
            0,
        )
        .expect("normalization should succeed");

        let event: serde_json::Value =
            serde_json::from_str(&normalized.event_json).expect("normalized event should parse");
        assert_eq!(normalized.event_type, "screen_frame");
        assert_eq!(normalized.recorded_at_ms, 77);
        assert_eq!(event["schemaVersion"], RAW_EVENT_SCHEMA_VERSION);
        assert_eq!(event["sourceVersion"], 1);
        assert_eq!(event["eventId"], "evt_xpc_1");
        assert_eq!(event["recordedAtMs"], 77);
        assert_eq!(event["payload"]["frameId"], "frm_1");
    }
}
