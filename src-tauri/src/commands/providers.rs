use agent_core::providers::registry::{ActiveSelection, ProviderConfig, ProviderInput};
use agent_core::providers::Capabilities;
use agent_core::Error;
use serde::Serialize;
use tauri::State;
use ts_rs::TS;

use crate::AppState;

#[tauri::command]
pub fn list_providers(state: State<'_, AppState>) -> Vec<ProviderConfig> {
    state.providers.list()
}

#[tauri::command]
pub fn save_provider(
    state: State<'_, AppState>,
    input: ProviderInput,
) -> Result<ProviderConfig, Error> {
    state.providers.save(input)
}

#[tauri::command]
pub fn delete_provider(state: State<'_, AppState>, id: String) -> Result<(), Error> {
    state.providers.delete(&id)
}

#[tauri::command]
pub fn get_active_selection(state: State<'_, AppState>) -> Option<ActiveSelection> {
    state.providers.active()
}

#[tauri::command]
pub fn set_active_selection(
    state: State<'_, AppState>,
    selection: ActiveSelection,
) -> Result<(), Error> {
    state.providers.set_active(selection)
}

/// Effective capabilities for a provider+model pair (image support is model-level).
#[tauri::command]
pub fn get_model_capabilities(
    state: State<'_, AppState>,
    provider_id: String,
    model: String,
) -> Result<Capabilities, Error> {
    state.providers.capabilities_for(&provider_id, &model)
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ProviderTestResult {
    pub ok: bool,
    pub message: String,
    pub models: Vec<String>,
    /// Subset of `models` the server reported as vision-capable (may be empty).
    pub vision_models: Vec<String>,
}

/// Reaches the provider's model listing endpoint. Transport success and model discovery are
/// reported separately so a server without `/models` is still usable with a typed model name.
#[tauri::command]
pub async fn test_provider(
    state: State<'_, AppState>,
    id: String,
) -> Result<ProviderTestResult, Error> {
    state.providers.get(&id)?;
    let vision = || {
        state
            .providers
            .get(&id)
            .map(|p| p.vision_models)
            .unwrap_or_default()
    };
    Ok(match state.providers.discover_models(&id).await {
        Ok(models) if models.is_empty() => ProviderTestResult {
            ok: true,
            message: "Connected, but the server listed no models. Enter a model name manually.".into(),
            models,
            vision_models: Vec::new(),
        },
        Ok(models) => {
            let vision_models = vision();
            let vision_note = if vision_models.is_empty() {
                String::new()
            } else {
                format!(" {} accept image input.", vision_models.len())
            };
            ProviderTestResult {
                ok: true,
                message: format!("Connected. {} model(s) available.{vision_note}", models.len()),
                models,
                vision_models,
            }
        }
        Err(Error::Provider(msg)) if msg.contains("HTTP 404") => ProviderTestResult {
            ok: true,
            message: "Connected, but this server has no model listing endpoint. Enter a model name manually.".into(),
            models: Vec::new(),
            vision_models: Vec::new(),
        },
        Err(e) => ProviderTestResult { ok: false, message: e.to_string(), models: Vec::new(), vision_models: Vec::new() },
    })
}
