use crate::state::AppState;
use tauri::{
    menu::{CheckMenuItem, Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Wry,
};

pub fn create_tray(app: &AppHandle) -> tauri::Result<TrayIcon<Wry>> {
    let toggle_auto_send = CheckMenuItem::with_id(
        app,
        "toggle_auto_send",
        "Auto-Send",
        true,
        false,
        None::<&str>,
    )?;
    let toggle_auto_receive = CheckMenuItem::with_id(
        app,
        "toggle_auto_receive",
        "Auto-Receive",
        true,
        false,
        None::<&str>,
    )?;
    let quit_i = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let show_i = MenuItem::with_id(app, "show", "Show Window", true, None::<&str>)?;

    let menu = Menu::with_items(
        app,
        &[
            &show_i,
            &MenuItem::with_id(app, "sep1", "-", true, None::<&str>)?,
            &toggle_auto_send,
            &toggle_auto_receive,
            &MenuItem::with_id(app, "sep2", "-", true, None::<&str>)?,
            &quit_i,
        ],
    )?;

    // Initial state sync
    let state = app.state::<AppState>();
    let settings = state.settings.lock().unwrap();
    let _ = toggle_auto_send.set_checked(settings.auto_send);
    let _ = toggle_auto_receive.set_checked(settings.auto_receive);

    // Build Tray
    let tray = TrayIconBuilder::with_id("main-tray")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .icon(
            tauri::image::Image::from_bytes(include_bytes!("../icons/tray-icon-white.png"))
                .expect("Failed to load tray icon"),
        )
        .on_menu_event(move |app: &AppHandle, event| {
            let id = event.id.as_ref();
            match id {
                "quit" => app.exit(0),
                "show" => {
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                        set_badge(app, false);
                    }
                }
                "toggle_auto_send" => {
                    let state = app.state::<AppState>();
                    let mut settings = state.settings.lock().unwrap();
                    settings.auto_send = !settings.auto_send;
                    crate::storage::save_settings(app, &settings);
                    let _ = app.emit("settings-changed", settings.clone());
                }
                "toggle_auto_receive" => {
                    let state = app.state::<AppState>();
                    let mut settings = state.settings.lock().unwrap();
                    settings.auto_receive = !settings.auto_receive;
                    crate::storage::save_settings(app, &settings);
                    let _ = app.emit("settings-changed", settings.clone());
                }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray: &TrayIcon<Wry>, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                    set_badge(app, false);
                }
            }
        })
        .build(app)?;

    Ok(tray)
}

pub fn update_tray_menu(_app: &AppHandle) {
    // STUB
}

pub fn set_badge(_app: &AppHandle, _show: bool) {
    // STUB
}
