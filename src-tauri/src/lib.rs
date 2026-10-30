//! Tauri host: owns the long-lived application state and exposes a small, validated command
//! surface to the webview. All privileged work (network, secrets, files) happens here or in
//! `agent-core`, never in the frontend.

mod commands;
mod state;

use tauri::Manager;
use tracing_subscriber::EnvFilter;

pub use state::AppState;

pub fn run() {
    init_tracing();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let state = AppState::initialize()?;
            tracing::info!(
                product = agent_core::config::product().display_name,
                data_root = %state.data_root.path().display(),
                secret_backend = state.secrets.backend(),
                "application state initialized"
            );
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_app_info,
            commands::start_run,
            commands::cancel_run,
            commands::probe_secret_store,
            commands::list_providers,
            commands::save_provider,
            commands::delete_provider,
            commands::get_active_selection,
            commands::set_active_selection,
            commands::test_provider,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the desktop application");
}

fn init_tracing() {
    let slug = agent_core::config::product()
        .slug
        .to_ascii_uppercase()
        .replace('-', "_");
    let filter = EnvFilter::try_from_env(format!("{slug}_LOG"))
        .unwrap_or_else(|_| EnvFilter::new("info,agent_core=debug"));
    // Secrets never reach tracing fields; see agent_core::secrets.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}
