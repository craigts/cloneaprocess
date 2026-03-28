mod commands;
mod core;
mod storage;
mod workflow;

use tauri::Manager;

use core::app_state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let state = AppState::bootstrap(&app.handle())?;
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::recorder::recorder_status,
            commands::storage::list_session_events,
            commands::storage::list_sessions,
            commands::storage::storage_smoke_test,
            commands::recorder::start_recording,
            commands::recorder::stop_recording,
            commands::system::system_status,
            commands::workflow::compile_workflow_preview
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
