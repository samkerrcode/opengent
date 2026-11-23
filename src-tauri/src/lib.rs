//! Tauri host: owns the long-lived application state and exposes a small, validated command
//! surface to the webview. All privileged work (network, secrets, files) happens here or in
//! `agent-core`, never in the frontend.

mod commands;
mod scheduler;
mod state;
mod window;

use tauri::{DragDropEvent, Emitter, Manager, WindowEvent};
use tracing_subscriber::EnvFilter;

pub use state::AppState;
use state::OPEN_SETTINGS_EVENT;

const SETTINGS_MENU_ID: &str = "settings";

/// Adds "Settings…" (⌘,) to the app menu, between About and Services.
#[cfg(target_os = "macos")]
fn install_menu(app: &tauri::App) -> tauri::Result<()> {
    use tauri::menu::{Menu, MenuItem, MenuItemKind, PredefinedMenuItem};
    let menu = Menu::default(app.handle())?;
    if let Some(MenuItemKind::Submenu(app_menu)) = menu.items()?.into_iter().next() {
        let settings = MenuItem::with_id(
            app,
            SETTINGS_MENU_ID,
            "Settings…",
            true,
            Some("CmdOrCtrl+,"),
        )?;
        if app_menu.items()?.len() >= 2 {
            let separator = PredefinedMenuItem::separator(app.handle())?;
            app_menu.insert_items(&[&settings, &separator], 2)?;
        } else {
            app_menu.append_items(&[&settings])?;
        }
    }
    app.set_menu(menu)?;
    Ok(())
}

pub fn run() {
    init_tracing();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            install_menu(app)?;
            let state = AppState::initialize(app.handle())?;
            // Before the window is first shown, so it never paints in the wrong theme.
            window::apply_theme(app.handle(), state.services.settings.get().appearance.theme);
            window::fit_and_show(app);
            tracing::info!(
                product = agent_core::config::product().display_name,
                data_root = %state.data_root.path().display(),
                secret_backend = state.secrets.backend(),
                "application state initialized"
            );
            // Bring up enabled MCP servers in the background; status flows via mcp-changed.
            state.services.mcp.autoconnect();
            app.manage(state);
            // Agent schedules: catch up on runs missed while closed/asleep, then keep ticking.
            scheduler::spawn(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_app_info,
            commands::get_settings,
            commands::update_settings,
            commands::start_run,
            commands::cancel_run,
            commands::probe_secret_store,
            commands::list_providers,
            commands::list_provider_kinds,
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
            commands::mcp_sign_in,
            commands::mcp_cancel_sign_in,
            commands::mcp_sign_out,
            commands::set_mcp_tool_policy,
            commands::set_mcp_default_policy,
            commands::reset_mcp_tool_policies,
            commands::set_mcp_instructions,
            commands::list_tool_permissions,
            commands::set_chat_server_enabled,
            commands::set_chat_tool_policy,
            commands::reset_chat_tool_permissions,
            commands::set_agent_server_access,
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
        .on_menu_event(|app, event| {
            if event.id() == SETTINGS_MENU_ID {
                let _ = app.emit(OPEN_SETTINGS_EVENT, ());
            }
        })
        // Native drag-and-drop: with `dragDropEnabled` on (the default), HTML5 drops never
        // reach the webview — the OS drop lands here instead. Imported in Rust so file
        // paths never come from the webview; the result is emitted for the chat store.
        .on_window_event(|window, event| {
            if let WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) = event {
                if window.label() == "main" {
                    commands::handle_dropped_paths(window.app_handle(), paths);
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building the desktop application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                // Stop every MCP server (and its process tree) before teardown.
                if let Some(state) = app.try_state::<AppState>() {
                    state.services.mcp.shutdown();
                }
            }
        });
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env(agent_core::config::log_env_var())
        .unwrap_or_else(|_| EnvFilter::new("info,agent_core=debug"));
    // Secrets never reach tracing fields; see agent_core::secrets.
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .init();
}
