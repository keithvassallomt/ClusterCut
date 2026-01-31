use crate::state::AppState;
use tauri::{Emitter, Listener, Manager};
use zbus::interface;
use zbus::object_server::SignalContext;

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

    #[zbus(signal)]
    pub async fn state_changed(
        ctxt: &SignalContext<'_>,
        auto_send: bool,
        auto_receive: bool,
    ) -> zbus::Result<()>;
}

pub async fn start_dbus_server(app_handle: tauri::AppHandle) -> zbus::Result<()> {
    let service = ClusterCutDBus::new(app_handle.clone());
    let conn = zbus::connection::Builder::session()?
        .name("com.keithvassallo.clustercut")?
        .serve_at("/org/gnome/Shell/Extensions/ClusterCut", service)?
        .build()
        .await?;

    // Clone for the closure
    let dbus_conn = conn.clone();

    // Listen for internal settings changes
    app_handle.listen("settings-changed", move |event: tauri::Event| {
        if let Ok(payload) = serde_json::from_str::<crate::storage::AppSettings>(event.payload()) {
            let conn = dbus_conn.clone();
            // Emit signal asynchronously
            tauri::async_runtime::spawn(async move {
                let _ = conn
                    .emit_signal(
                        Option::<&str>::None, // destination (broadcast)
                        "/org/gnome/Shell/Extensions/ClusterCut",
                        "com.keithvassallo.clustercut",
                        "StateChanged",
                        &(payload.auto_send, payload.auto_receive),
                    )
                    .await;
            });
        }
    });

    // Keep connection alive
    std::future::pending::<()>().await;
    Ok(())
}
