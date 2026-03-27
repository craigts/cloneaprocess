# Clone a Process

macOS-first desktop automation tooling built as a Tauri shell, Rust orchestration layer, and native Swift services.

## Workspace

- `apps/desktop`: Tauri app with the React UI and Rust core.
- `native/mac-recorder-service`: Swift package for recorder-side native automation work.
- `native/mac-runner-service`: Swift package for runner-side native automation work.
- `packages/*`: shared TypeScript packages for workflow, trace, and UI contracts.
- `docs/*`: architecture, IPC, and issue tracking notes.

## Remote Development

The repo includes a GitHub Codespaces / devcontainer setup in `.devcontainer/`.

- Use Codespaces for TypeScript, Rust, schema, docs, and Git work.
- Use a Mac for native Swift services, Accessibility, ScreenCaptureKit, and end-to-end automation verification.
