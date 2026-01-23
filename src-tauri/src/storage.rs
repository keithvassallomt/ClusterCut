use crate::peer::Peer;
use names::Generator;
use rand::Rng;
use std::collections::HashMap;
use std::fs;
use tauri::{path::BaseDirectory, AppHandle, Manager};

pub fn load_network_name(app: &AppHandle) -> String {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("network_name", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(_) => return String::from("unknown-network"),
    };

    if path.exists() {
        if let Ok(name) = fs::read_to_string(&path) {
            if !name.trim().is_empty() {
                tracing::debug!("Loaded Network Name: {}", name);
                return name;
            }
        }
    }

    // Generate new name if missing
    let mut generator = Generator::default();
    let new_name = generator
        .next()
        .unwrap_or_else(|| "unnamed-network".to_string());

    // Save it
    save_network_name(app, &new_name);
    tracing::info!("Generated new Network Name: {}", new_name);
    new_name
}

pub fn save_network_name(app: &AppHandle, name: &str) {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("network_name", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(_) => return,
    };

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, name);
}

pub fn load_cluster_key(app: &AppHandle) -> Option<Vec<u8>> {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("cluster_key.bin", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Failed to resolve cluster key path: {}", e);
            return None;
        }
    };

    if !path.exists() {
        return None;
    }

    match fs::read(&path) {
        Ok(key) => {
            if key.len() != 32 {
                tracing::error!("Cluster key file has invalid length: {}", key.len());
                return None;
            }
            tracing::debug!("Loaded Cluster Key from disk.");
            Some(key)
        }
        Err(e) => {
            tracing::warn!("Failed to read cluster key file: {}", e);
            None
        }
    }
}

pub fn save_cluster_key(app: &AppHandle, key: &[u8]) {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("cluster_key.bin", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to resolve cluster key path for saving: {}", e);
            return;
        }
    };

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    if let Err(e) = fs::write(path, key) {
        tracing::error!("Failed to write cluster key file: {}", e);
    } else {
        tracing::debug!("Saved Cluster Key to disk.");
    }
}

pub fn load_known_peers(app: &AppHandle) -> HashMap<String, Peer> {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("known_peers.json", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to resolve config path: {}", e);
            return HashMap::new();
        }
    };

    if !path.exists() {
        return HashMap::new();
    }

    match fs::read_to_string(&path) {
        Ok(content) => match serde_json::from_str::<HashMap<String, Peer>>(&content) {
            Ok(peers) => {
                tracing::info!("Loaded {} known peers from disk at {:?}", peers.len(), path);
                peers
            }
            Err(e) => {
                tracing::error!("Failed to parse known peers: {}", e);
                HashMap::new()
            }
        },
        Err(e) => {
            tracing::warn!("Failed to read known peers file: {}", e);
            HashMap::new()
        }
    }
}

pub fn save_known_peers(app: &AppHandle, peers: &HashMap<String, Peer>) {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("known_peers.json", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to resolve config path for saving: {}", e);
            return;
        }
    };

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    match serde_json::to_string_pretty(peers) {
        Ok(json) => {
            if let Err(e) = fs::write(&path, json) {
                tracing::error!("Failed to write known peers file: {}", e);
            } else {
                tracing::debug!("Saved known peers to disk at {:?}", path);
            }
        }
        Err(e) => {
            tracing::error!("Failed to serialize known peers: {}", e);
        }
    }
}

pub fn load_device_id(app: &AppHandle) -> String {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("device_id", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(_) => return String::new(),
    };

    if !path.exists() {
        return String::new();
    }

    fs::read_to_string(path).unwrap_or_default()
}

pub fn save_device_id(app: &AppHandle, id: &str) {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("device_id", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Failed to resolve device_id path: {}", e);
            return;
        }
    };

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let _ = fs::write(path, id);
}

pub fn load_network_pin(app: &AppHandle) -> String {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("network_pin", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(_) => return String::from("000000"),
    };

    if path.exists() {
        if let Ok(pin) = fs::read_to_string(&path) {
            if !pin.trim().is_empty() {
                return pin;
            }
        }
    }

    // Generate new PIN
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let pin: String = (0..6)
        .map(|_| {
            let idx = rand::thread_rng().gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect();

    tracing::info!("Generated New Network PIN: {}", pin);
    save_network_pin(app, &pin);
    pin
}

pub fn save_network_pin(app: &AppHandle, pin: &str) {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("network_pin", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to resolve network_pin path: {}", e);
            return;
        }
    };

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, pin);
}
// Helper to reset network state (Self-Destruct/Kick)
pub fn reset_network_state(app: &AppHandle) {
    let path_resolver = app.path();
    // Include the actual filenames used by load/save
    let config_files = [
        "cluster_key.bin",
        "network_name",
        "network_pin",
        "known_peers.json",
    ];

    for filename in config_files {
        match path_resolver.resolve(filename, BaseDirectory::AppConfig) {
            Ok(path) => {
                if path.exists() {
                    let _ = fs::remove_file(path);
                }
            }
            Err(e) => tracing::error!("Failed to resolve path for {}: {}", filename, e),
        }
    }
}

pub fn regenerate_identity(app: &AppHandle) -> (String, String) {
    let path_resolver = app.path();
    // 1. Delete existing Name/PIN files
    if let Ok(path) = path_resolver.resolve("network_name", BaseDirectory::AppConfig) {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }
    if let Ok(path) = path_resolver.resolve("network_pin", BaseDirectory::AppConfig) {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }

    // 2. Load (which generates new ones if missing)
    let new_name = load_network_name(app);
    let new_pin = load_network_pin(app);

    (new_name, new_pin)
}
// --- Settings Persistance ---

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct NotificationSettings {
    pub device_join: bool,
    pub device_leave: bool,
    pub data_sent: bool,
    pub data_received: bool,
}

impl Default for NotificationSettings {
    fn default() -> Self {
        Self {
            device_join: true,
            device_leave: true,
            data_sent: false,
            data_received: false,
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct AppSettings {
    pub custom_device_name: Option<String>,
    pub cluster_mode: String, // "auto" or "provisioned"
    pub auto_send: bool,
    pub auto_receive: bool,
    pub notifications: NotificationSettings,
    pub shortcut_send: Option<String>,
    pub shortcut_receive: Option<String>,
    pub enable_file_transfer: bool,
    pub max_auto_download_size: u64, // In bytes
    pub notify_large_files: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            custom_device_name: None,
            cluster_mode: "auto".to_string(),
            auto_send: true,
            auto_receive: true,
            notifications: NotificationSettings::default(),
            shortcut_send: Some("CommandOrControl+Alt+C".to_string()),
            shortcut_receive: Some("CommandOrControl+Alt+V".to_string()),
            enable_file_transfer: true,
            max_auto_download_size: 50 * 1024 * 1024, // 50 MB
            notify_large_files: true,
        }
    }
}

pub fn load_settings(app: &AppHandle) -> AppSettings {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("settings.json", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(_) => return AppSettings::default(),
    };

    if !path.exists() {
        return AppSettings::default();
    }

    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => AppSettings::default(),
    }
}

pub fn save_settings(app: &AppHandle, settings: &AppSettings) {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("settings.json", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Failed to resolve settings path: {}", e);
            return;
        }
    };

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = fs::write(path, json);
    }
}
