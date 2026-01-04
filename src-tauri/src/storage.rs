use std::collections::HashMap;
use std::fs;
use tauri::{path::BaseDirectory, AppHandle, Manager};

// Defines the storage format
// Just a map of PeerID -> Key (hex encoded or bytes? Bytes in memory, maybe hex in JSON for readability, but serde handles vector as array)
// serde_json handles Vec<u8> as [u8, u8, ...] usually.

pub fn load_trusted_peers(app: &AppHandle) -> HashMap<String, Vec<u8>> {
    let path_resolver = app.path();
    // Use AppConfig or AppData. AppConfig is better for settings/keys.
    let path = match path_resolver.resolve("trusted_peers.json", BaseDirectory::AppConfig) {
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
        Ok(content) => match serde_json::from_str::<HashMap<String, Vec<u8>>>(&content) {
            Ok(peers) => {
                println!("Loaded {} trusted peers from disk.", peers.len());
                peers
            }
            Err(e) => {
                eprintln!("Failed to parse trusted peers: {}", e);
                HashMap::new()
            }
        },
        Err(e) => {
            eprintln!("Failed to read trusted peers file: {}", e);
            HashMap::new()
        }
    }
}

pub fn save_trusted_peers(app: &AppHandle, peers: &HashMap<String, Vec<u8>>) {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("trusted_peers.json", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to resolve config path for saving: {}", e);
            return;
        }
    };

    // Ensure directory exists
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    match serde_json::to_string_pretty(peers) {
        Ok(json) => {
            if let Err(e) = fs::write(path, json) {
                eprintln!("Failed to write trusted peers file: {}", e);
            } else {
                println!("Saved trusted peers to disk.");
            }
        }
        Err(e) => {
            eprintln!("Failed to serialize trusted peers: {}", e);
        }
    }
}
// ... (existing load/save trusted peers)

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
            eprintln!("Failed to resolve device_id path: {}", e);
            return;
        }
    };

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let _ = fs::write(path, id);
}
