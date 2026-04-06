# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What This Is

Clone a Process is a macOS desktop automation tool that records user interactions (clicks, keystrokes, screen frames, accessibility snapshots) and replays them as deterministic workflows. It is built as a Tauri 2 app with a React frontend, Rust orchestration core, and native Swift XPC services.

## Commands

### Run the full desktop app (Mac only)
```bash
npm run desktop:run
```
This builds both Swift services, registers the recorder XPC launch agent, and starts `tauri dev`.

### Build Swift services independently
```bash
cd native/mac-recorder-service && swift build
cd native/mac-runner-service && swift build
```

### Run Swift tests
```bash
cd native/mac-recorder-service && swift test
cd native/mac-runner-service && swift test
```

### Frontend only (Vite dev server, no Tauri shell)
```bash
npm run dev
```

### Build the full app bundle
```bash
npm run tauri:build
```
This stages native helpers, runs `tauri build --bundles app`, then installs the bundled XPC service.

### Rust checks
```bash
cd apps/desktop/src-tauri && cargo check
cd apps/desktop/src-tauri && cargo build
cd apps/desktop/src-tauri && cargo test
```

### Probe recorder XPC connection (CLI mode)
```bash
cd apps/desktop/src-tauri && cargo run -- --probe-recorder-xpc
```

## Architecture

### Three-layer stack

1. **React UI** (`apps/desktop/src/`) — Single-page Vite+React app. Currently one large `App.tsx` that calls Tauri commands via `@tauri-apps/api`.

2. **Rust core** (`apps/desktop/src-tauri/src/`) — Tauri 2 backend. Organized as:
   - `commands/` — Tauri command handlers (recorder, storage, system, workflow). These are the IPC surface the frontend calls.
   - `core/` — Business logic: `recorder.rs` (session management), `runner.rs` (workflow execution), `recorder_xpc.rs` (FFI bridge to the Obj-C/C XPC shim), `app_state.rs` (bootstrap and shared state), `retention.rs` (data lifecycle), `trace.rs` (event processing), `helper_paths.rs` (locating native binaries).
   - `storage/` — SQLite-backed persistence via `database.rs` and `sqlite.rs`. Schema version tracked in `mod.rs` (`STORAGE_SCHEMA_VERSION`).
   - `workflow/` — Workflow compilation and execution engine (`mod.rs`).

3. **Native Swift services** (`native/`) — Two macOS executables that run as XPC services:
   - `mac-recorder-service` — Screen capture, accessibility snapshots, event taps. Modules: `Capture/`, `Accessibility/`, `EventTap/`, `IPC/`, `Models/`.
   - `mac-runner-service` — Deterministic replay. Modules: `Actions/`, `Selectors/`, `Verification/`, `IPC/`, `Models/`.

### IPC bridge (Rust ↔ Swift)

The Rust core talks to Swift services over XPC. The bridge is:
- `native/xpc_bridge.m` — Obj-C shim compiled by `build.rs` via the `cc` crate, providing a C ABI that Rust calls through FFI.
- Transport modes: **MachService** (dev, via launchd) or **BundledService** (production, embedded in .app bundle).
- Payloads are JSON envelopes with version field `v`. See `docs/ipc/` for protocol specs and versioning strategy.

### Shared TypeScript packages (`packages/`)

- `@cloneaprocess/workflow-schema` — Workflow type definitions shared between frontend and backend.
- `@cloneaprocess/trace-schema` — Trace/event type definitions.
- `@cloneaprocess/ui-components` — Shared UI components.

All are source-only (no build step; `main` points directly at `./src/index.ts`).

### Environment variables

- `CLONEAPROCESS_RECORDER_TRANSPORT` — `xpc_service` (dev, default) or `xpc_bundle_service` (production).
- `CLONEAPROCESS_RECORDER_XPC_SERVICE` — Mach service name (default: `com.cloneaprocess.recorder.dev`).
- `CLONEAPROCESS_RECORDER_HELPER_PATH` — Override path to recorder binary.

### Tauri permissions

New Tauri commands must be added to `capabilities/default.json` with an `allow-<command-name>` permission entry, or the frontend will get a permission error at runtime.

### Dev vs production XPC

- **Dev**: `scripts/bootstrap-recorder-xpc.sh` registers a launchd launch agent pointing at the debug-built binary. Logs go to `/tmp/com.cloneaprocess.recorder.dev.{stdout,stderr}.log`.
- **Production build**: `scripts/install-bundled-xpc-service.sh` embeds the recorder as a `.xpc` bundle inside the `.app`, ad-hoc code-signs both.

### Codespaces / devcontainer

The `.devcontainer/` setup supports TypeScript, Rust, schema, and docs work in Codespaces. Native Swift services and end-to-end automation require a Mac.
