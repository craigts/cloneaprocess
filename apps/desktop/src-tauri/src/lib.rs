mod commands;
mod core;
mod storage;
mod workflow;

use serde_json::json;
use tauri::Manager;

use core::app_state::AppState;
use core::recorder_xpc::{RecorderXpcClient, RecorderXpcTransportKind};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if let Some(exit_code) = maybe_run_cli_probe() {
        std::process::exit(exit_code);
    }

    tauri::Builder::default()
        .setup(|app| {
            let state = AppState::bootstrap(&app.handle())?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::ai::ai_compile_workflow,
            commands::ai::get_ai_api_key,
            commands::ai::set_ai_api_key,
            commands::clipboard::copy_to_clipboard,
            commands::permissions::check_permissions,
            commands::permissions::open_system_settings_pane,
            commands::permissions::restart_recorder_service,
            commands::recorder::recorder_status,
            commands::storage::load_keyframe_bytes,
            commands::storage::list_session_events,
            commands::storage::list_sessions,
            commands::storage::get_retention_policy,
            commands::storage::list_workflow_run_logs,
            commands::storage::list_workflow_runs,
            commands::storage::update_session_description,
            commands::storage::storage_smoke_test,
            commands::storage::run_retention_cleanup_now,
            commands::storage::update_retention_policy,
            commands::recorder::start_recording,
            commands::recorder::stop_recording,
            commands::system::system_status,
            commands::workflow::compile_workflow_preview,
            commands::workflow::execute_session_workflow,
            commands::workflow::execute_workflow_json,
            commands::workflow::approve_workflow_run,
            commands::workflow::reject_workflow_run
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn maybe_run_cli_probe() -> Option<i32> {
    if !std::env::args().any(|arg| arg == "--probe-recorder-xpc") {
        return None;
    }

    let transport_kind = match std::env::var("CLONEAPROCESS_RECORDER_TRANSPORT")
        .ok()
        .as_deref()
        .map(str::trim)
    {
        Some("xpc_bundle_service") => RecorderXpcTransportKind::BundledService,
        _ => RecorderXpcTransportKind::MachService,
    };
    let service_name = std::env::var("CLONEAPROCESS_RECORDER_XPC_SERVICE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| match transport_kind {
            RecorderXpcTransportKind::BundledService => "com.cloneaprocess.recorder".to_string(),
            RecorderXpcTransportKind::MachService => "com.cloneaprocess.recorder.dev".to_string(),
        });

    let result = (|| -> Result<serde_json::Value, String> {
        let client = RecorderXpcClient::connect(&service_name, transport_kind)
            .map_err(|error| error.to_string())?;
        let protocol = serde_json::from_str::<serde_json::Value>(
            &client
                .ping(std::time::Duration::from_secs(3))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let permissions = serde_json::from_str::<serde_json::Value>(
            &client
                .get_permissions(std::time::Duration::from_secs(3))
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;

        Ok(json!({
            "ok": true,
            "service": service_name,
            "transport": match transport_kind {
                RecorderXpcTransportKind::BundledService => "xpc_bundled_service",
                RecorderXpcTransportKind::MachService => "xpc_mach_service",
            },
            "protocol": protocol,
            "permissions": permissions,
        }))
    })();

    match result {
        Ok(payload) => {
            println!("{}", payload);
            Some(0)
        }
        Err(error) => {
            println!(
                "{}",
                json!({
                    "ok": false,
                    "service": service_name,
                    "transport": match transport_kind {
                        RecorderXpcTransportKind::BundledService => "xpc_bundled_service",
                        RecorderXpcTransportKind::MachService => "xpc_mach_service",
                    },
                    "error": error,
                })
            );
            Some(1)
        }
    }
}
