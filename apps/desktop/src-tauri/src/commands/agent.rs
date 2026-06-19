use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Emitter, State};

use crate::core::agent::{self, AgentConfig, AgentEvent};
use crate::core::app_state::AppState;

pub struct AgentState {
    /// True while a run/replay thread is active. Reset by the worker thread when it exits, so the
    /// next run can start without needing the user to press Stop.
    running: Arc<AtomicBool>,
    cancel_token: Mutex<Option<Arc<AtomicBool>>>,
}

impl AgentState {
    pub fn new() -> Self {
        Self {
            running: Arc::new(AtomicBool::new(false)),
            cancel_token: Mutex::new(None),
        }
    }
}

/// Run the agent against a recorded session (using it as a demonstration).
#[tauri::command]
pub fn start_agent(
    app: AppHandle,
    state: State<'_, AppState>,
    agent_state: State<'_, AgentState>,
    session_id: i64,
    max_steps: Option<u32>,
) -> Result<(), String> {
    spawn_agent(app, state, agent_state, Some(session_id), None, max_steps)
}

/// Run the agent from a natural-language task alone, with no recording — the agent explores the
/// live screen to figure out how. ("You don't need to hit record.")
#[tauri::command]
pub fn start_agent_from_task(
    app: AppHandle,
    state: State<'_, AppState>,
    agent_state: State<'_, AgentState>,
    task: String,
    max_steps: Option<u32>,
) -> Result<(), String> {
    if task.trim().is_empty() {
        return Err("Describe what you want the agent to do.".to_string());
    }
    spawn_agent(app, state, agent_state, None, Some(task), max_steps)
}

fn spawn_agent(
    app: AppHandle,
    state: State<'_, AppState>,
    agent_state: State<'_, AgentState>,
    session_id: Option<i64>,
    task: Option<String>,
    max_steps: Option<u32>,
) -> Result<(), String> {
    if agent_state.running.load(Ordering::Relaxed) {
        return Err("An agent is already running. Stop it first.".to_string());
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
        task,
        max_steps: max_steps.unwrap_or(0), // 0 → agent's DEFAULT_MAX_STEPS
        api_key,
        cancel_token,
    };

    agent_state.running.store(true, Ordering::Relaxed);
    let running = Arc::clone(&agent_state.running);

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
        running.store(false, Ordering::Relaxed);
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

/// Whether a captured replay script exists for this session (or the last no-record task).
#[tauri::command]
pub fn agent_script_exists(state: State<'_, AppState>, session_id: Option<i64>) -> Result<bool, String> {
    Ok(agent::has_script(state.storage(), session_id))
}

/// Replays a previously-captured run deterministically through the runner — fast, no LLM.
#[tauri::command]
pub fn replay_agent_script(
    app: AppHandle,
    state: State<'_, AppState>,
    agent_state: State<'_, AgentState>,
    session_id: Option<i64>,
) -> Result<(), String> {
    if agent_state.running.load(Ordering::Relaxed) {
        return Err("An agent is already running. Stop it first.".to_string());
    }

    let (_task, steps) = agent::load_script(state.storage(), session_id)
        .ok_or_else(|| "No saved script yet — run this with AI once, then you can replay it.".to_string())?;

    let cancel_token = Arc::new(AtomicBool::new(false));
    {
        let mut guard = agent_state.cancel_token.lock().map_err(|_| "lock poisoned")?;
        *guard = Some(Arc::clone(&cancel_token));
    }

    agent_state.running.store(true, Ordering::Relaxed);
    let running = Arc::clone(&agent_state.running);

    let runner_binary = state.runner_binary().to_path_buf();
    std::thread::spawn(move || {
        let result = agent::run_script(&runner_binary, steps, cancel_token, |event: AgentEvent| {
            let _ = app.emit("agent:progress", &event);
        });
        if let Err(err) = result {
            let _ = app.emit("agent:progress", &AgentEvent::Failed { step_number: 0, error: err });
        }
        running.store(false, Ordering::Relaxed);
    });

    Ok(())
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
