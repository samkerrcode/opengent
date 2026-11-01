//! Tauri host: owns the long-lived application state and exposes a small, validated command
//! surface to the webview. All privileged work (network, secrets, files) happens here or in
//! `agent-core`, never in the frontend.

mod commands;
mod scheduler;
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
            let state = AppState::initialize(app.handle())?;
            tracing::info!(
                product = agent_core::config::product().display_name,
                data_root = %state.data_root.path().display(),
                secret_backend = state.secrets.backend(),
                "application state initialized"
            );
            // Bring up enabled MCP servers in the background; status flows via mcp-changed.
            state.mcp.autoconnect();
            app.manage(state);
            // Agent schedules: catch up on runs missed while closed/asleep, then keep ticking.
            scheduler::spawn(app.handle().clone());
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
            commands::list_chats,
            commands::search_chats,
            commands::create_chat,
            commands::get_chat,
            commands::rename_chat,
            commands::delete_chat,
            commands::pick_attachments,
            commands::attachment_data_url,
            commands::get_model_capabilities,
            commands::list_mcp_servers,
            commands::save_mcp_server,
            commands::delete_mcp_server,
            commands::set_mcp_server_enabled,
            commands::reconnect_mcp_server,
            commands::set_mcp_tool_policy,
            commands::set_mcp_default_policy,
            commands::get_tool_settings,
            commands::set_tool_settings,
            commands::resolve_tool_approval,
            commands::list_agents,
            commands::save_agent,
            commands::delete_agent,
            commands::list_schedules,
            commands::save_schedule,
            commands::set_schedule_enabled,
            commands::delete_schedule,
            commands::run_schedule_now,
        ])
        .build(tauri::generate_context!())
        .expect("error while building the desktop application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // Stop every MCP server (and its process tree) before teardown.
                if let Some(state) = app.try_state::<AppState>() {
                    state.mcp.shutdown();
                }
            }
        });
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
