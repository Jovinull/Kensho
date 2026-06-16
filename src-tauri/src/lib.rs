//! Kensho backend — composition root.
//!
//! Architecture (Clean / DDD-flavored):
//!   core            cross-cutting: errors, config, logging
//!   domain          pure entities + value objects
//!   infrastructure  adapters: sqlite, local llm, os notifications
//!   services        business-logic orchestrators
//!   actor           the persistent LLM worker (Tokio actor)
//!   tauri_commands  IPC transport exposed to the frontend

// Foundation scaffold: several domain/infra APIs are defined ahead of their
// first caller. Allow dead_code crate-wide until features land.
#![allow(dead_code)]

mod actor;
mod core;
mod domain;
mod infrastructure;
mod services;
mod tauri_commands;

use tauri::{Emitter, Manager};

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::core::SystemConfig;
use crate::infrastructure::{llm, Database, Notifier};
use crate::services::approval::PendingApprovals;
use crate::services::clipboard::{self, ClipboardContext};
use crate::services::{AssistantService, ToolRouter};
use crate::tauri_commands::TauriApprovalGate;

/// Build and run the Tauri application.
pub fn run() {
    core::logging::init();
    tracing::info!("starting Kensho");

    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();

            // Resolve a per-user data directory; fall back to CWD if unavailable.
            let data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            let config = SystemConfig::from_data_dir(&data_dir);
            tracing::info!(data_dir = %config.data_dir.display(), "resolved data dir");

            // Persistence (creates db + tables on first run).
            let db = Database::open(&config.database_path)?;

            // Adapters + extensible tool router (DB writes + native notifications).
            let notifier = Notifier::default();
            // Human-in-the-loop approval for dangerous shell commands.
            let pending = PendingApprovals::new();
            let gate = Arc::new(TauriApprovalGate::new(handle.clone(), pending.clone()));
            let router = ToolRouter::with_defaults(db.clone(), notifier.clone(), gate);

            // Clipboard context: snapshotted on hotkey, consumed by next prompt.
            let clipboard = ClipboardContext::new();

            // System-prompt composer (live DB context + clipboard + tool protocol).
            let assistant = AssistantService::new(db.clone(), clipboard.clone());

            // Voice output (no-op unless built with --features tts).
            #[cfg(feature = "tts")]
            let speaker = infrastructure::audio::spawn(&config);
            #[cfg(not(feature = "tts"))]
            let speaker = infrastructure::Speaker::disabled();

            // Shared flag toggled by the tray "Pausar Alertas" item.
            let alerts_paused = Arc::new(AtomicBool::new(false));

            // Local inference engine (mock by default; gguf with --features llama)
            // owned exclusively by the actor task, which also owns the rolling
            // conversation history, the tool router and the proactive heartbeat.
            let engine = llm::build_engine(&config);
            let deps = actor::ActorDeps {
                router,
                assistant,
                notifier: notifier.clone(),
                speaker,
                heartbeat: std::time::Duration::from_secs(config.heartbeat_secs),
                alerts_paused: alerts_paused.clone(),
            };
            let llm_handle = actor::spawn(handle.clone(), engine, deps);

            // Apply the floating-widget always-on-top preference.
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.set_always_on_top(config.always_on_top);
            }

            // Global hotkey (Ctrl+Shift+K): focus Kensho and open the input.
            #[cfg(desktop)]
            {
                use tauri_plugin_global_shortcut::{
                    Builder as ShortcutBuilder, Code, Modifiers, Shortcut, ShortcutState,
                };
                let toggle = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyK);
                let clipboard_hotkey = clipboard.clone();
                app.handle().plugin(
                    ShortcutBuilder::new()
                        .with_shortcut(toggle)?
                        .with_handler(move |app, _shortcut, event| {
                            if event.state == ShortcutState::Pressed {
                                // Snapshot the clipboard so this turn's prompt has it.
                                clipboard_hotkey.set(clipboard::read_system_clipboard());
                                if let Some(win) = app.get_webview_window("main") {
                                    let _ = win.show();
                                    let _ = win.set_focus();
                                    let _ = app.emit("ui://focus-input", ());
                                }
                            }
                        })
                        .build(),
                )?;
                tracing::info!("global shortcut Ctrl+Shift+K registered");
            }

            // System tray (AppIndicator): keeps Kensho out of the dock.
            #[cfg(desktop)]
            {
                use tauri::menu::{MenuBuilder, MenuItemBuilder};
                use tauri::tray::TrayIconBuilder;

                let show = MenuItemBuilder::with_id("show", "Mostrar/Ocultar Kensho").build(app)?;
                let pause = MenuItemBuilder::with_id("pause", "Pausar Alertas").build(app)?;
                let quit = MenuItemBuilder::with_id("quit", "Sair").build(app)?;
                let menu = MenuBuilder::new(app)
                    .item(&show)
                    .item(&pause)
                    .item(&quit)
                    .build()?;

                let paused_for_tray = alerts_paused.clone();
                let mut tray = TrayIconBuilder::new()
                    .menu(&menu)
                    .on_menu_event(move |app, event| match event.id().as_ref() {
                        "show" => {
                            if let Some(win) = app.get_webview_window("main") {
                                if win.is_visible().unwrap_or(false) {
                                    let _ = win.hide();
                                } else {
                                    let _ = win.show();
                                    let _ = win.set_focus();
                                }
                            }
                        }
                        "pause" => {
                            let now = !paused_for_tray.load(Ordering::Relaxed);
                            paused_for_tray.store(now, Ordering::Relaxed);
                            tracing::info!(paused = now, "proactive alerts toggled");
                        }
                        "quit" => app.exit(0),
                        _ => {}
                    });
                if let Some(icon) = app.default_window_icon().cloned() {
                    tray = tray.icon(icon);
                }
                tray.build(app)?;
                tracing::info!("system tray ready");
            }

            app.manage(config);
            app.manage(db);
            app.manage(llm_handle);
            app.manage(notifier);
            app.manage(pending);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            tauri_commands::ask_assistant,
            tauri_commands::create_task,
            tauri_commands::list_tasks,
            tauri_commands::send_notification,
            tauri_commands::set_always_on_top,
            tauri_commands::app_info,
            tauri_commands::approve_action,
        ])
        .on_window_event(|window, event| {
            // Closing the floating widget just hides it; the actor + heartbeat
            // keep running silently in the background (tray "Sair" exits).
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running the Kensho application");
}
