use serde::Serialize;
use std::path::Path;
use tauri::State;

use crate::core::app_state::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionCheck {
    storage_ready: bool,
    recordings_root_ready: bool,
    helper_ready: bool,
    helper_error: Option<String>,
    accessibility: bool,
    screen_recording: bool,
}

#[tauri::command]
pub fn check_permissions(state: State<'_, AppState>) -> Result<PermissionCheck, String> {
    let storage_ready = state
        .storage()
        .status()
        .db_path
        .parent()
        .map(Path::exists)
        .unwrap_or(false);
    let recordings_root_ready = state.recordings_root().exists();

    let recorder_status = state
        .recorder()
        .lock()
        .map_err(|_| "recorder mutex poisoned".to_string())?
        .status()
        .map_err(|error| error.to_string())?;

    Ok(PermissionCheck {
        storage_ready,
        recordings_root_ready,
        helper_ready: recorder_status.transport_ready,
        helper_error: recorder_status.transport_error,
        accessibility: recorder_status
            .permissions
            .get("accessibility")
            .copied()
            .unwrap_or(false),
        screen_recording: recorder_status
            .permissions
            .get("screenRecording")
            .copied()
            .unwrap_or(false),
    })
}

#[tauri::command]
pub fn open_system_settings_pane(pane: &str) -> Result<(), String> {
    let url = match pane {
        "accessibility" => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
        }
        "screen_recording" => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
        }
        _ => return Err(format!("unknown settings pane: {pane}")),
    };

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|error| format!("failed to open System Settings: {error}"))?;
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = url;
        return Err("opening System Settings is only supported on macOS".to_string());
    }

    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartRecorderResult {
    restarted: bool,
    error: Option<String>,
}

#[tauri::command]
pub fn restart_recorder_service() -> Result<RestartRecorderResult, String> {
    #[cfg(target_os = "macos")]
    {
        let uid_output = std::process::Command::new("id")
            .arg("-u")
            .output()
            .map_err(|e| format!("failed to get uid: {e}"))?;
        let uid = String::from_utf8_lossy(&uid_output.stdout).trim().to_string();
        let domain = format!("gui/{uid}");
        let label = std::env::var("CLONEAPROCESS_RECORDER_XPC_SERVICE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "com.cloneaprocess.recorder.dev".to_string());
        let service_target = format!("{domain}/{label}");

        let output = std::process::Command::new("launchctl")
            .args(["kickstart", "-k", &service_target])
            .output()
            .map_err(|e| format!("failed to run launchctl: {e}"))?;

        if output.status.success() {
            Ok(RestartRecorderResult {
                restarted: true,
                error: None,
            })
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Ok(RestartRecorderResult {
                restarted: false,
                error: Some(stderr),
            })
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(RestartRecorderResult {
            restarted: false,
            error: Some("only supported on macOS".to_string()),
        })
    }
}
