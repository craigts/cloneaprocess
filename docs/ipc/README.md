# IPC Contracts

This document defines the IPC contracts between the Rust core (Tauri) and the macOS native services.

## Overview

Two XPC services are used:

- Recorder service: capture + semantic enrichment
- Runner service: deterministic replay

Transport:

- Swift XPC service implemented with NSXPC
- Objective-C/C shim provides a C ABI for Rust
- Payloads are JSON by default (CBOR can replace JSON later without changing the C ABI)

## Protocols and Methods

### Recorder Service

Protocol name: `RecorderServiceXPC`

Methods:

- `ping(reply)`
- `getPermissions(reply)`
- `beginCapture(config, reply)`
- `endCapture(sessionId, reply)`
- `subscribeEvents(eventSink, reply)`
- `unsubscribeEvents(reply)`

### Runner Service

Protocol name: `RunnerServiceXPC`

Methods:

- `ping(reply)`
- `runWorkflow(request, reply)`
- `abortRun(runId, reply)`
- `subscribeEvents(eventSink, reply)`
- `unsubscribeEvents(reply)`

### Event Sink

Protocol name: `EventSinkXPC`

Methods:

- `onEvent(event)`

The Rust side provides an EventSink implementation via the Objective-C/C shim. Events are delivered on a background queue and must be handled quickly.

## Envelope and Error Model

All payloads are JSON dictionaries. Each request, reply, and event includes an envelope with a version number.

### Request Envelope

```json
{
  "v": 1,
  "id": "req_123",
  "ts": 1710000000000,
  "type": "begin_capture",
  "payload": { ... }
}
```

### Reply Envelope

```json
{
  "v": 1,
  "id": "req_123",
  "ok": true,
  "error": null,
  "payload": { ... }
}
```

Error object:

```json
{
  "code": "PERMISSION_DENIED",
  "message": "Screen Recording permission not granted",
  "retryable": false
}
```

### Event Envelope

```json
{
  "v": 1,
  "id": "evt_456",
  "ts": 1710000000000,
  "type": "ax_snapshot",
  "payload": { ... }
}
```

## Recorder Payloads

### beginCapture

```json
{
  "session_label": "User signup flow",
  "idle_fps": 2,
  "burst_fps": 10,
  "burst_ms": 1200,
  "include_ax_snapshots": true,
  "redaction": {
    "redact_secure_fields": true,
    "block_clipboard": true,
    "blocked_bundle_ids": ["com.1password.1password", "com.apple.keychainaccess"]
  }
}
```

Reply:

```json
{
  "session_id": "sess_abc"
}
```

### endCapture

Reply:

```json
{
  "session_id": "sess_abc",
  "event_count": 1452,
  "frame_count": 38,
  "started_at": 1710000000000,
  "ended_at": 1710000005000,
  "errors": []
}
```

### Recorder Events

Event types and payloads (not exhaustive):

- `mouse_down`

```json
{ "x": 842, "y": 613, "button": "left" }
```

- `mouse_up`

```json
{ "x": 842, "y": 613, "button": "left" }
```

- `key_down`

```json
{ "key_code": 12, "modifiers": ["cmd"] }
```

- `frontmost_app_changed`

```json
{ "bundle_id": "com.apple.Safari", "pid": 123 }
```

- `screen_frame`

```json
{ "frame_id": "frm_001", "path": "recordings/sess_abc/frames/frm_001.jpg" }
```

- `ax_snapshot`

```json
{
  "snapshot_id": "ax_001",
  "bundle_id": "com.apple.Safari",
  "role": "AXButton",
  "title": "Sign In",
  "path": [3, 1, 4]
}
```

## Runner Payloads

### runWorkflow

```json
{
  "workflow_id": "wf_001",
  "inputs": {
    "email": "user@example.com",
    "name": "Alex"
  },
  "strictness": "ax_first",
  "timeout_ms": 120000
}
```

Reply:

```json
{
  "run_id": "run_001"
}
```

### Runner Events

- `run_started`

```json
{ "run_id": "run_001", "workflow_id": "wf_001" }
```

- `step_started`

```json
{ "run_id": "run_001", "step_index": 3, "kind": "click" }
```

- `step_finished`

```json
{ "run_id": "run_001", "step_index": 3, "ok": true }
```

- `run_failed`

```json
{ "run_id": "run_001", "error": { "code": "ELEMENT_NOT_FOUND", "message": "Selector not found", "retryable": true } }
```

- `run_completed`

```json
{ "run_id": "run_001", "ok": true, "duration_ms": 34210 }
```

## Notes

- Payloads should be treated as opaque by the transport layer.
- The Rust core persists events in arrival order using `ts` for ordering across service restarts.
- Keyframes are stored on disk; events contain only paths and identifiers.
