use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, State};

use crate::core::agent::{self, AgentConfig, AgentEvent};
use crate::core::app_state::AppState;

pub struct AgentState {
    cancel_token: Mutex<Option<Arc<AtomicBool>>>,
}

impl AgentState {
    pub fn new() -> Self {
        Self {
            cancel_token: Mutex::new(None),
        }
    }
}

#[tauri::command]
pub fn start_agent(
    app: AppHandle,
    state: State<'_, AppState>,
    agent_state: State<'_, AgentState>,
    session_id: i64,
    max_steps: Option<u32>,
) -> Result<(), String> {
    // Check if agent is already running
    {
        let guard = agent_state.cancel_token.lock().map_err(|_| "lock poisoned")?;
        if let Some(token) = &*guard {
            if !token.load(Ordering::Relaxed) {
                return Err("An agent is already running. Stop it first.".to_string());
            }
        }
    }

    let api_key = resolve_api_key(&state)?;
    let cancel_token = Arc::new(AtomicBool::new(false));

    // Store cancel token
    {
        let mut guard = agent_state.cancel_token.lock().map_err(|_| "lock poisoned")?;
        *guard = Some(Arc::clone(&cancel_token));
    }

    let storage = state.storage().clone();
    let runner_binary = state.runner_binary().to_path_buf();

    let config = AgentConfig {
        session_id,
        max_steps: max_steps.unwrap_or(50),
        api_key,
        cancel_token,
    };

    // Spawn the agent on a background thread
    std::thread::spawn(move || {
        let result = agent::run_agent(
            &storage,
            &runner_binary,
            config,
            |event: AgentEvent| {
                let _ = app.emit("agent:progress", &event);
            },
        );

        if let Err(err) = result {
            let _ = app.emit("agent:progress", &AgentEvent::Failed {
                step_number: 0,
                error: err,
            });
        }
    });

    Ok(())
}

#[tauri::command]
pub fn stop_agent(agent_state: State<'_, AgentState>) -> Result<(), String> {
    let guard = agent_state.cancel_token.lock().map_err(|_| "lock poisoned")?;
    if let Some(token) = &*guard {
        token.store(true, Ordering::Relaxed);
        Ok(())
    } else {
        Err("No agent is running.".to_string())
    }
}

fn resolve_api_key(state: &State<'_, AppState>) -> Result<String, String> {
    if let Ok(Some(key)) = state.storage().get_app_setting("anthropic_api_key") {
        if !key.trim().is_empty() {
            return Ok(key.trim().to_string());
        }
    }
    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.trim().is_empty() {
            return Ok(key.trim().to_string());
        }
    }
    Err("No Anthropic API key configured.".to_string())
}
