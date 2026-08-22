//! System tray: show/hide main window, live stats, consolidate, open data
//! folder, quit, and hide-on-close.

use std::time::Duration;

use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, Runtime,
};

use crate::state::AppState;

const TRAY_ID: &str = "main-tray";
const MENU_SHOW: &str = "tray_show";
const MENU_HIDE: &str = "tray_hide";
const MENU_QUIT: &str = "tray_quit";
const MENU_CONSOLIDATE: &str = "tray_consolidate";
const MENU_OPEN_DATA: &str = "tray_open_data";
const MENU_STATS_MEMORIES: &str = "tray_stats_memories";
const MENU_STATS_PROJECTS: &str = "tray_stats_projects";
const MENU_STATS_AGENTS: &str = "tray_stats_agents";

pub fn setup<R: Runtime>(app: &tauri::App<R>) -> tauri::Result<()> {
    // — Stats (disabled info items, updated by background thread) —
    let stat_memories =
        MenuItem::with_id(app, MENU_STATS_MEMORIES, "Memories: —", false, None::<&str>)?;
    let stat_projects =
        MenuItem::with_id(app, MENU_STATS_PROJECTS, "Projects: —", false, None::<&str>)?;
    let stat_agents = MenuItem::with_id(app, MENU_STATS_AGENTS, "Agents: —", false, None::<&str>)?;

    let sep1 = PredefinedMenuItem::separator(app)?;

    // — Window controls —
    let show = MenuItem::with_id(app, MENU_SHOW, "Show biTurbo", true, None::<&str>)?;
    let hide = MenuItem::with_id(app, MENU_HIDE, "Hide", true, None::<&str>)?;

    let sep2 = PredefinedMenuItem::separator(app)?;

    // — Actions —
    let consolidate =
        MenuItem::with_id(app, MENU_CONSOLIDATE, "Consolidate Now", true, None::<&str>)?;
    let open_data = MenuItem::with_id(app, MENU_OPEN_DATA, "Open Data Folder", true, None::<&str>)?;

    let sep3 = PredefinedMenuItem::separator(app)?;

    // — Quit —
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &stat_memories,
            &stat_projects,
            &stat_agents,
            &sep1,
            &show,
            &hide,
            &sep2,
            &consolidate,
            &open_data,
            &sep3,
            &quit,
        ],
    )?;

    let icon = app
        .default_window_icon()
        .cloned()
        .ok_or_else(|| tauri::Error::FailedToReceiveMessage)?;

    let _tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .menu(&menu)
        .tooltip("biTurbo")
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            MENU_SHOW => show_main_window(app),
            MENU_HIDE => hide_main_window(app),
            MENU_CONSOLIDATE => {
                if let Some(state) = app.try_state::<AppState>() {
                    let _ = crate::scheduler::enqueue(state.inner(), None);
                }
            }
            MENU_OPEN_DATA => {
                if let Some(state) = app.try_state::<AppState>() {
                    open_data_folder(&state.data_dir);
                }
            }
            MENU_QUIT => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    // Spawn a background thread that refreshes the stat items and tooltip
    // every 30 seconds. AppState is managed *after* tray::setup returns, so
    // the first iteration(s) will simply skip until the state is available.
    let app_handle = app.handle().clone();
    let stat_mem = stat_memories.clone();
    let stat_proj = stat_projects.clone();
    let stat_ag = stat_agents.clone();
    std::thread::Builder::new()
        .name("biturbo-tray-stats".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(30));

            let (memories, projects, agents) = {
                let Some(state) = app_handle.try_state::<AppState>() else {
                    continue;
                };
                let Ok(conn) = state.db.conn() else {
                    continue;
                };
                let memories: i64 = conn
                    .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
                    .unwrap_or(0);
                let projects: i64 = conn
                    .query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
                    .unwrap_or(0);
                let agents: i64 = conn
                    .query_row("SELECT COUNT(*) FROM agents", [], |r| r.get(0))
                    .unwrap_or(0);
                (memories, projects, agents)
            };

            let _ = stat_mem.set_text(format!("Memories: {memories}"));
            let _ = stat_proj.set_text(format!("Projects: {projects}"));
            let _ = stat_ag.set_text(format!("Agents: {agents}"));

            if let Some(tray) = app_handle.tray_by_id(TRAY_ID) {
                let _ = tray.set_tooltip(Some(format!(
                    "biTurbo — {memories} memories · {projects} projects · {agents} agents"
                )));
            }
        })
        .ok();

    Ok(())
}

pub fn on_window_event<R: Runtime>(window: &tauri::Window<R>, event: &tauri::WindowEvent) {
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        let _ = window.hide();
        api.prevent_close();
    }
}

fn show_main_window<R: Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn hide_main_window<R: Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

fn toggle_main_window<R: Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        match window.is_visible() {
            Ok(true) => {
                let _ = window.hide();
            }
            _ => {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }
    }
}

fn open_data_folder(path: &std::path::Path) {
    let _ = if cfg!(target_os = "windows") {
        std::process::Command::new("explorer").arg(path).spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(path).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(path).spawn()
    };
}
