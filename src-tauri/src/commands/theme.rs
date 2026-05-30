//! Theme commands.

use crate::state::AppState;
use tauri::State;

#[tauri::command]
pub(crate) async fn get_theme_override() -> Option<String> {
    std::env::var("CLUSTERCUT_THEME").ok()
}

#[tauri::command]
pub(crate) async fn get_current_theme(state: State<'_, AppState>) -> Result<Option<String>, ()> {
    Ok(state.current_theme.lock().unwrap().clone())
}
