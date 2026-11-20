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
            let desktop =
                tauri::async_runtime::block_on(Desktop::open(&path, runtime::pi_executable()))?;
            app.manage(desktop);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![dispatch])
        .run(tauri::generate_context!())
        .expect("Could not launch Starchive");
}
