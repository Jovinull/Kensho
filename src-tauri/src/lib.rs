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

use crate::core::SystemConfig;
use crate::infrastructure::{llm, Database, Notifier};
use crate::services::{AssistantService, ToolRouter};

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
            let router = ToolRouter::with_defaults(db.clone(), notifier.clone());

            // System-prompt composer (live DB context + tool protocol).
            let assistant = AssistantService::new(db.clone());

            // Local inference engine (mock by default; gguf with --features llama)
            // owned exclusively by the actor task, which also owns the rolling
            // conversation history and the tool router.
            let engine = llm::build_engine(&config);
            let llm_handle = actor::spawn(handle.clone(), engine, router, assistant);

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
                app.handle().plugin(
                    ShortcutBuilder::new()
                        .with_shortcut(toggle)?
                        .with_handler(move |app, _shortcut, event| {
                            if event.state == ShortcutState::Pressed {
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

            app.manage(config);
            app.manage(db);
            app.manage(llm_handle);
            app.manage(notifier);

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            tauri_commands::ask_assistant,
            tauri_commands::create_task,
            tauri_commands::list_tasks,
            tauri_commands::send_notification,
            tauri_commands::set_always_on_top,
            tauri_commands::app_info,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Kensho application");
}
