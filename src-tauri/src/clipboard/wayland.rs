/// Clipboard backend using wl-clipboard-rs (wlr-data-control protocol).
/// Used on Wayland with compositors that support wlr-data-control (KDE, Sway, Hyprland).
///
/// Uses polling with get_contents (not subprocess spawning), so no flickering.
use super::common::{self, ClipboardContent};
use crate::state::AppState;
use crate::transport::Transport;
use std::io::Read;
use std::thread;
use std::time::Duration;
use tauri::AppHandle;
use wl_clipboard_rs::copy::{MimeSource, MimeType as CopyMimeType, Options as CopyOptions, Source};
use wl_clipboard_rs::paste::{
    get_contents, ClipboardType, Error as PasteError, MimeType as PasteMimeType, Seat,
};

/// Check if wlr-data-control is available by attempting a paste.
pub fn is_available() -> bool {
    match get_contents(ClipboardType::Regular, Seat::Unspecified, PasteMimeType::Text) {
        Ok(_) => true,
        Err(PasteError::NoSeats) | Err(PasteError::NoMimeType) => true,
        Err(PasteError::MissingProtocol { .. }) => false,
        Err(_) => true,
    }
}

fn read_clipboard_text() -> Option<String> {
    match get_contents(ClipboardType::Regular, Seat::Unspecified, PasteMimeType::Text) {
        Ok((mut pipe, _mime)) => {
            let mut text = String::new();
            if pipe.read_to_string(&mut text).is_ok() && !text.is_empty() {
                Some(text)
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

fn read_clipboard_files() -> Option<Vec<String>> {
    match get_contents(
        ClipboardType::Regular,
        Seat::Unspecified,
        PasteMimeType::Specific("text/uri-list"),
    ) {
        Ok((mut pipe, _mime)) => {
            let mut data = String::new();
            if pipe.read_to_string(&mut data).is_ok() && !data.is_empty() {
                let uris: Vec<String> = data
                    .lines()
                    .filter(|l| !l.starts_with('#') && !l.is_empty())
                    .map(|l| l.trim().to_string())
                    .collect();
                if !uris.is_empty() {
                    Some(uris)
                } else {
                    None
                }
            } else {
                None
            }
        }
        Err(_) => None,
    }
}

fn read_clipboard() -> ClipboardContent {
    if let Some(files) = read_clipboard_files() {
        return ClipboardContent::Files(files);
    }
    if let Some(text) = read_clipboard_text() {
        return ClipboardContent::Text(text);
    }
    ClipboardContent::None
}

fn write_text(_app: &AppHandle, text: String) -> Result<(), String> {
    let opts = CopyOptions::new();
    opts.copy(Source::Bytes(text.into_bytes().into()), CopyMimeType::Text)
        .map_err(|e| format!("wl-clipboard-rs copy failed: {}", e))
}

fn write_files(_app: &AppHandle, files: Vec<String>) -> Result<(), String> {
    let uris: Vec<String> = files
        .into_iter()
        .filter_map(|p| {
            let path = std::path::Path::new(&p);
            let abs_path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir().ok()?.join(path)
            };
            url::Url::from_file_path(abs_path)
                .ok()
                .map(|u| u.to_string())
        })
        .collect();

    if uris.is_empty() {
        return Err("No valid file paths".to_string());
    }

    // Advertise both text/uri-list and x-special/gnome-copied-files so GTK file
    // managers (Nautilus) and others recognise a file paste rather than a text
    // paste. The `copy` field in x-special/gnome-copied-files distinguishes a
    // copy from a cut — we always use "copy".
    let uri_list = format!("{}\n", uris.join("\n"));
    let gnome_copied = format!("copy\n{}", uris.join("\n"));

    let sources = vec![
        MimeSource {
            source: Source::Bytes(uri_list.into_bytes().into()),
            mime_type: CopyMimeType::Specific("text/uri-list".to_string()),
        },
        MimeSource {
            source: Source::Bytes(gnome_copied.into_bytes().into()),
            mime_type: CopyMimeType::Specific("x-special/gnome-copied-files".to_string()),
        },
    ];

    let opts = CopyOptions::new();
    opts.copy_multi(sources)
        .map_err(|e| format!("wl-clipboard-rs copy files failed: {}", e))
}

pub fn set_clipboard(app: &AppHandle, text: String) {
    common::set_clipboard_with_ignore(app, text, write_text);
}

pub fn set_clipboard_paths(app: &AppHandle, paths: Vec<String>) {
    common::set_clipboard_paths_with_ignore(app, paths, write_files);
}

pub fn read_text(_app: &AppHandle) -> Result<String, String> {
    read_clipboard_text().ok_or_else(|| "No text in clipboard".to_string())
}

pub fn write_text_direct(app: &AppHandle, text: String) -> Result<(), String> {
    write_text(app, text)
}

pub fn start_monitor(app_handle: AppHandle, state: AppState, transport: Transport) {
    thread::spawn(move || {
        tracing::info!("Starting Wayland clipboard monitor (wlr-data-control polling)");

        let mut last_content = ClipboardContent::None;

        loop {
            if state.is_shutdown() {
                tracing::info!("Wayland clipboard monitor shutting down.");
                break;
            }

            let current_content = read_clipboard();

            if common::should_process_content(&current_content, &last_content) {
                last_content = current_content.clone();
                common::process_clipboard_change(
                    current_content,
                    &app_handle,
                    &state,
                    &transport,
                );
            }

            thread::sleep(Duration::from_millis(500));
        }
    });
}
