use crate::peer::Peer;
use std::collections::HashMap;
use std::fs;
use tauri::{path::BaseDirectory, AppHandle, Manager};

pub fn load_cluster_key(app: &AppHandle) -> Option<Vec<u8>> {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("cluster_key.bin", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to resolve cluster key path: {}", e);
            return None;
        }
    };

    if !path.exists() {
        return None;
    }

    match fs::read(&path) {
        Ok(key) => {
            if key.len() != 32 {
                eprintln!("Cluster key file has invalid length: {}", key.len());
                return None;
            }
            println!("Loaded Cluster Key from disk.");
            Some(key)
        }
        Err(e) => {
            eprintln!("Failed to read cluster key file: {}", e);
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
        eprintln!("Failed to write cluster key file: {}", e);
    } else {
        println!("Saved Cluster Key to disk.");
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
                println!("Loaded {} known peers from disk.", peers.len());
                peers
            }
            Err(e) => {
                eprintln!("Failed to parse known peers: {}", e);
                HashMap::new()
            }
        },
        Err(e) => {
            eprintln!("Failed to read known peers file: {}", e);
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
            if let Err(e) = fs::write(path, json) {
                eprintln!("Failed to write known peers file: {}", e);
            } else {
                println!("Saved known peers to disk.");
            }
        }
        Err(e) => {
            eprintln!("Failed to serialize known peers: {}", e);
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
            eprintln!("Failed to resolve device_id path: {}", e);
            return;
        }
    };

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let _ = fs::write(path, id);
}
