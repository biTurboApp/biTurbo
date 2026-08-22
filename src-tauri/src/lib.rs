//! biTurbo — local-first memory layer for AI coding agents.
//!
//! Architecture
//! ────────────
//! ┌─────────────────────────────────────────────────────────────┐
//! │  Tauri 2 desktop app  (this crate)                          │
//! │   ├── commands::*        — IPC handlers (GUI ↔ backend)     │
//! │   ├── mcp                 — MCP stdio server for AI agents   │
//! │   ├── memory             — CRUD over memory entries         │
//! │   ├── project            — multi-project isolation          │
//! │   ├── index_engine       — turbovec IdMapIndex wrapper      │
//! │   ├── embed              — fastembed (BGE) embeddings       │
//! │   ├── ingest             — tree-sitter project indexing     │
//! │   ├── consolidate        — decay / dedup / merge            │
//! │   └── db                 — SQLite schema + connection pool  │
//! └─────────────────────────────────────────────────────────────┘
//!
//! Data lives in the OS app-data dir (~/Library/Application Support/com.biturbo.app/
//! on macOS, %APPDATA%\com.biturbo.app on Windows, ~/.local/share/com.biturbo.app on
//! Linux). Both the GUI and the MCP server share the same on-disk state.

pub mod application;
pub mod commands;
pub mod consolidate;
pub mod db;
pub mod embed;
pub mod error;
pub mod index_engine;
pub mod ingest;
pub mod io;
pub mod mcp;
pub mod memory;
pub mod operations;
pub mod persistence;
pub mod project;
pub mod recall;
pub mod scheduler;
pub mod smoke;
pub mod state;
pub mod tray;

pub use error::{BiError, BiResult};
pub use state::AppState;

use std::sync::Arc;
use tauri::Manager;
use tracing::info;
use tracing_subscriber::fmt::layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

fn init_logging(data_dir: &std::path::Path) {
    let log_dir = data_dir.join("logs");
    std::fs::create_dir_all(&log_dir).ok();

    let file_appender = tracing_appender::rolling::daily(&log_dir, "biturbo.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    // Leak the guard so the non-blocking writer stays alive for the process lifetime.
    std::mem::forget(_guard);

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "biturbo_lib=info,tauri=info".into());

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            layer()
                .compact()
                .with_target(false)
                .with_writer(std::io::stdout),
        )
        .with(
            layer()
                .compact()
                .with_target(true)
                .with_writer(non_blocking),
        )
        .init();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_autostart::Builder::new().build())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            #[cfg(desktop)]
            app.handle()
                .plugin(tauri_plugin_updater::Builder::new().build())?;
            tray::setup(app)?;

            let data_dir = app.path().app_data_dir().expect("app data dir resolvable");
            std::fs::create_dir_all(&data_dir).ok();

            init_logging(&data_dir);

            info!("biTurbo starting…");

            let mut state = AppState::open(&data_dir).expect("open app state");
            state.app = Some(app.handle().clone());
            let state_arc = Arc::new(state);
            scheduler::spawn(state_arc.clone());
            let _ = operations::resume_pending(state_arc.clone());
            io::resume_watches(&state_arc);
            app.manage((*state_arc).clone());
            info!("biTurbo ready @ {}", data_dir.display());
            Ok(())
        })
        .on_window_event(tray::on_window_event)
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::list_memories,
            commands::get_memory,
            commands::remember,
            commands::forget_memory,
            commands::update_memory,
            commands::search_memories,
            commands::recall_explain,
            commands::submit_recall_feedback,
            commands::list_projects,
            commands::create_project,
            commands::delete_project,
            commands::ensure_project_marker_files,
            commands::get_project,
            commands::ingest_project,
            commands::start_ingest,
            commands::ingest_multiple_projects,
            commands::operation_status,
            commands::list_operations,
            commands::cancel_operation,
            commands::retry_operation,
            commands::get_project_graph,
            commands::list_tags,
            commands::consolidate_now,
            commands::consolidate_status,
            commands::import_folder,
            commands::export_memories,
            commands::set_watch,
            commands::watch_status,
            commands::set_project_embed_model,
            commands::stats,
            commands::list_agents,
            commands::register_agent,
            commands::recent_activity,
            commands::bootstrap,
            commands::resolve_mcp_binary_path,
            commands::install_mcp_config,
            commands::check_for_updates,
            commands::install_update,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
