/// Clipboard backend using tauri-plugin-clipboard.
/// Used on macOS, Windows, and X11 on Linux.
use super::common::{
    self, ClipboardContent, IGNORED_CONTENT,
};
use crate::state::AppState;
use crate::transport::Transport;
use std::time::Duration;
use std::{thread, sync::mpsc};
use tauri::{AppHandle, Manager};
use tauri_plugin_clipboard::Clipboard;

fn read_clipboard(app: &AppHandle) -> ClipboardContent {
    let clip = app.state::<Clipboard>();

    match clip.read_files() {
        Ok(files) => {
            if !files.is_empty() {
                return ClipboardContent::Files(files);
            }
        }
        Err(_) => {}
    }

    match clip.read_text() {
        Ok(text) => {
            if !text.is_empty() {
                return ClipboardContent::Text(text);
            }
        }
        Err(_) => {}
    }

    ClipboardContent::None
}

fn write_text(app: &AppHandle, text: String) -> Result<(), String> {
    app.state::<Clipboard>()
        .write_text(text)
        .map_err(|e| e.to_string())
}

fn write_files(app: &AppHandle, files: Vec<String>) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let paths: Vec<String> = files
            .into_iter()
            .filter_map(|p| {
                let path = std::path::Path::new(&p);
                if path.is_absolute() {
                    Some(p)
                } else {
                    std::env::current_dir()
                        .ok()
                        .map(|c| c.join(path).to_string_lossy().to_string())
                }
            })
            .collect();

        if paths.is_empty() {
            return Err("No valid paths".to_string());
        }

        app.state::<Clipboard>()
            .write_files_uris(paths)
            .map_err(|e| e.to_string())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let uris: Vec<String> = files
            .into_iter()
            .filter_map(|p| {
                let path = std::path::Path::new(&p);
                let abs_path = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    match std::env::current_dir().ok() {
                        Some(c) => c.join(path),
                        None => return None,
                    }
                };

                url::Url::from_file_path(abs_path)
                    .ok()
                    .map(|u| u.to_string())
            })
            .collect();

        if uris.is_empty() {
            return Err("No valid file paths convertible to URIs".to_string());
        }

        app.state::<Clipboard>()
            .write_files_uris(uris)
            .map_err(|e| e.to_string())
    }
}

pub fn set_clipboard(app: &AppHandle, text: String) {
    common::set_clipboard_with_ignore(app, text, write_text);
}

pub fn set_clipboard_paths(app: &AppHandle, paths: Vec<String>) {
    common::set_clipboard_paths_with_ignore(app, paths, write_files);
}

/// Read clipboard text directly (for manual send shortcut).
pub fn read_text(app: &AppHandle) -> Result<String, String> {
    app.state::<Clipboard>()
        .read_text()
        .map_err(|e| e.to_string())
}

/// Write clipboard text directly (for manual receive shortcut).
pub fn write_text_direct(app: &AppHandle, text: String) -> Result<(), String> {
    write_text(app, text)
}

pub fn start_monitor(app_handle: AppHandle, state: AppState, transport: Transport) {
    let app_handle_worker = app_handle.clone();

    let (cmd_tx, cmd_rx) = mpsc::channel::<()>();
    let (res_tx, res_rx) = mpsc::channel::<ClipboardContent>();

    // Worker Thread
    thread::spawn(move || {
        while cmd_rx.recv().is_ok() {
            let content = read_clipboard(&app_handle_worker);
            if res_tx.send(content).is_err() {
                break;
            }
        }
    });

    // Monitor Thread
    thread::spawn(move || {
        let mut last_content = ClipboardContent::None;

        loop {
            if state.is_shutdown() {
                tracing::info!("Clipboard monitor received shutdown signal, exiting.");
                break;
            }

            if cmd_tx.send(()).is_err() {
                tracing::error!("Clipboard worker thread died.");
                break;
            }

            let current_content = match res_rx.recv_timeout(Duration::from_millis(500)) {
                Ok(c) => c,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    tracing::warn!(
                        "Clipboard read timed out (possible deadlock/lock). Skipping cycle."
                    );
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    tracing::error!("Clipboard worker disconnected.");
                    break;
                }
            };

            if common::should_process_content(&current_content, &last_content) {
                last_content = current_content.clone();
                common::process_clipboard_change(
                    current_content,
                    &app_handle,
                    &state,
                    &transport,
                );
            } else if current_content != ClipboardContent::None {
                // Update last_content for ignored/echo matches
                let ignored = IGNORED_CONTENT.lock().unwrap();
                if *ignored == ClipboardContent::None && current_content != last_content {
                    // Content didn't change from an ignored echo perspective
                } else if matches!(&*ignored, ClipboardContent::Text(t) if matches!(&current_content, ClipboardContent::Text(c) if c == t))
                    || matches!(&*ignored, ClipboardContent::Files(f) if matches!(&current_content, ClipboardContent::Files(c) if c == f))
                {
                    last_content = current_content;
                }
            }

            thread::sleep(Duration::from_millis(500));
        }
    });
}
