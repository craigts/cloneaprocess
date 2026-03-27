# Devcontainer Notes

This environment is intended for remote editing and lightweight verification from GitHub Codespaces or VS Code remote containers.

## What works well

- TypeScript and React editing in `apps/desktop`
- Rust editing in `apps/desktop/src-tauri`
- npm workspace install and frontend builds
- Rust unit tests that do not depend on macOS-only APIs

## What does not work in Codespaces

- macOS Accessibility APIs
- ScreenCaptureKit
- native Swift automation services that target macOS runtime behavior
- full end-to-end Tauri desktop execution for the macOS product

Use Codespaces for planning, editing, schema work, UI work, Rust storage/compiler work, and Git operations. Use a Mac for recorder, runner, permissions, and replay verification.
