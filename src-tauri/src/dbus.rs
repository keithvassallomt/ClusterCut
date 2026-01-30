use crate::state::AppState;
use tauri::{Emitter, Manager};
use zbus::interface;

pub struct ClusterCutDBus {
    app_handle: tauri::AppHandle,
}

impl ClusterCutDBus {
    pub fn new(app_handle: tauri::AppHandle) -> Self {
        Self { app_handle }
    }
}

#[interface(name = "com.keithvassallo.clustercut")]
impl ClusterCutDBus {
    async fn toggle_auto_send(&mut self) -> bool {
        let state = self.app_handle.state::<AppState>();
        let mut settings = state.settings.lock().unwrap();
        settings.auto_send = !settings.auto_send;
        crate::storage::save_settings(&self.app_handle, &settings);
        let _ = self.app_handle.emit("settings-changed", settings.clone());

        // Notify Tray if applicable
        #[cfg(desktop)]
        crate::tray::update_tray_menu(&self.app_handle);

        settings.auto_send
    }

    async fn toggle_auto_receive(&mut self) -> bool {
        let state = self.app_handle.state::<AppState>();
        let mut settings = state.settings.lock().unwrap();
        settings.auto_receive = !settings.auto_receive;
        crate::storage::save_settings(&self.app_handle, &settings);
        let _ = self.app_handle.emit("settings-changed", settings.clone());

        #[cfg(desktop)]
        crate::tray::update_tray_menu(&self.app_handle);

        settings.auto_receive
    }

    async fn get_state(&self) -> (bool, bool) {
        let state = self.app_handle.state::<AppState>();
        let settings = state.settings.lock().unwrap();
        (settings.auto_send, settings.auto_receive)
    }

    async fn show_window(&self) {
        if let Some(window) = self.app_handle.get_webview_window("main") {
            let _ = window.show();
            let _ = window.set_focus();
            crate::tray::set_badge(&self.app_handle, false);
        }
    }

    async fn quit(&self) {
        self.app_handle.exit(0);
    }
}

pub async fn start_dbus_server(app_handle: tauri::AppHandle) -> zbus::Result<()> {
    let service = ClusterCutDBus::new(app_handle);
    let _conn = zbus::connection::Builder::session()?
        .name("com.keithvassallo.clustercut")?
        .serve_at("/org/gnome/Shell/Extensions/ClusterCut", service)?
        .build()
        .await?;

    // Keep connection alive
    std::future::pending::<()>().await;
    Ok(())
}
