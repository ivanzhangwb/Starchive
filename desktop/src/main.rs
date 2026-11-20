#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use serde_json::Value;
use starchive_core::runtime;
use tauri::{Manager, State};

mod dispatch;

use dispatch::Desktop;

#[tauri::command]
async fn dispatch(
    action: String,
    payload: Value,
    desktop: State<'_, Desktop>,
) -> Result<Value, String> {
    desktop
        .dispatch(&action, payload)
        .await
        .map_err(|error| format!("{error:#}"))
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let path = runtime::database_path()?;
            let desktop = tauri::async_runtime::block_on(Desktop::open(&path))?;
            app.manage(desktop);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![dispatch, agent_send_stream])
        .run(tauri::generate_context!())
        .expect("Could not launch Starfolio");
}

#[tauri::command]
async fn agent_send_stream(
    session_id: dispatch::Id,
    text: String,
    on_delta: tauri::ipc::Channel<String>,
    desktop: State<'_, Desktop>,
) -> Result<Value, String> {
    desktop
        .send_agent(session_id, &text, Some(on_delta))
        .await
        .map_err(|error| format!("{error:#}"))
}
