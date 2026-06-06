//! Commands backing the in-memory diagnostics panel.

use crate::diagnostics::DiagnosticEvent;
use crate::state::AppState;
use tauri::State;

#[tauri::command]
pub(crate) fn get_diagnostic_events(state: State<'_, AppState>) -> Vec<DiagnosticEvent> {
    state.diagnostics.lock().unwrap().iter().cloned().collect()
}

#[tauri::command]
pub(crate) fn clear_diagnostic_events(state: State<'_, AppState>) {
    state.diagnostics.lock().unwrap().clear();
}
