# Issue Tracker

This is the initial tracked issue list to start implementation. Status values are updated in place as work lands.

| ID | Title | Status | Description | Dependencies | Acceptance Criteria |
| --- | --- | --- | --- | --- | --- |
| ISSUE-001 | Repo scaffold and build scripts | DONE | Create directory layout and minimal Tauri app under `apps/desktop` plus package stubs under `packages/`. | None | Repo contains `apps/desktop`, `native/*`, and `packages/*` directories with the initial Tauri shell, shared packages, and native service stubs. |
| ISSUE-002 | Rust core skeleton | DONE | Add `src-tauri/src/core` and `src-tauri/src/commands` with placeholder command handlers. | ISSUE-001 | Tauri app can invoke a Rust command and receive a reply. |
| ISSUE-003 | SQLite event store | DONE | Implement `storage` module with tables for sessions, raw events, semantic actions, and keyframes. | ISSUE-002 | SQLite schema exists and can insert a session and a raw event. |
| ISSUE-004 | Recorder service XPC skeleton | DONE | Create `native/mac-recorder-service` with XPC listener, `ping`, and `getPermissions`. | ISSUE-001 | XPC service builds and responds to `ping` and `getPermissions`. |
| ISSUE-005 | Event tap capture | IN_PROGRESS | Implement Quartz event tap to emit key/mouse events via XPC. | ISSUE-004 | Key and mouse events are received by the Rust side as recorder events. |
| ISSUE-006 | ScreenCaptureKit keyframes | OPEN | Capture keyframes on interaction and store them on disk; emit `screen_frame` events with path. | ISSUE-004 | Keyframe files are created and events reference valid paths. |
| ISSUE-007 | AX snapshot on click | OPEN | Capture AX element at cursor and emit snapshot + selector hints. | ISSUE-005 | Clicking a UI element results in an `ax_snapshot` event with role and title. |
| ISSUE-008 | Rust event ingest | OPEN | Implement XPC client in Rust and persist incoming events to SQLite. | ISSUE-003, ISSUE-004 | Events are persisted in arrival order with timestamps. |
| ISSUE-009 | UI timeline | OPEN | Build React timeline for raw events and keyframes with session list. | ISSUE-002, ISSUE-008 | UI renders a session timeline and loads keyframes from disk paths. |
| ISSUE-010 | Semantic action compiler v0 | OPEN | Convert raw events to semantic actions and draft workflow JSON. | ISSUE-008 | A workflow JSON preview appears in the UI for a recorded session. |
