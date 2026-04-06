use serde::Serialize;
use tauri::State;

use crate::core::ai_compiler;
use crate::core::app_state::AppState;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiCompileResponse {
    workflow_json: String,
    step_count: usize,
    model: String,
    prompt_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

#[tauri::command]
pub fn ai_compile_workflow(
    state: State<'_, AppState>,
    session_id: i64,
) -> Result<AiCompileResponse, String> {
    let result = ai_compiler::ai_compile_workflow(state.storage(), session_id)?;

    Ok(AiCompileResponse {
        workflow_json: result.workflow_json,
        step_count: result.step_count,
        model: result.model,
        prompt_tokens: result.prompt_tokens,
        output_tokens: result.output_tokens,
    })
}

#[tauri::command]
pub fn ai_refine_workflow(
    state: State<'_, AppState>,
    workflow_json: String,
    workflow_run_id: i64,
    source_session_id: Option<i64>,
    session_description: Option<String>,
    user_hint: Option<String>,
) -> Result<AiCompileResponse, String> {
    let result = ai_compiler::ai_refine_workflow(
        state.storage(),
        &workflow_json,
        workflow_run_id,
        source_session_id,
        session_description.as_deref(),
        user_hint.as_deref(),
    )?;

    Ok(AiCompileResponse {
        workflow_json: result.workflow_json,
        step_count: result.step_count,
        model: result.model,
        prompt_tokens: result.prompt_tokens,
        output_tokens: result.output_tokens,
    })
}

#[tauri::command]
pub fn get_ai_api_key(state: State<'_, AppState>) -> Result<String, String> {
    let from_settings = state
        .storage()
        .get_app_setting("anthropic_api_key")
        .map_err(|e| e.to_string())?;

    if let Some(key) = from_settings {
        if !key.trim().is_empty() {
            return Ok(mask_api_key(&key));
        }
    }

    if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
        if !key.trim().is_empty() {
            return Ok(mask_api_key(&key));
        }
    }

    Ok(String::new())
}

#[tauri::command]
pub fn set_ai_api_key(
    state: State<'_, AppState>,
    api_key: String,
) -> Result<(), String> {
    state
        .storage()
        .upsert_app_setting("anthropic_api_key", api_key.trim())
        .map_err(|e| e.to_string())
}

fn mask_api_key(key: &str) -> String {
    let key = key.trim();
    if key.len() <= 8 {
        return "*".repeat(key.len());
    }
    format!("{}...{}", &key[..4], &key[key.len() - 4..])
}
