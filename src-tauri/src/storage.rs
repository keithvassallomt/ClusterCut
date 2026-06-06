use crate::peer::Peer;
use names::Generator;
use rand::Rng;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tauri::{path::BaseDirectory, AppHandle, Manager};

/// Restrict a file to owner-only access. On Unix sets mode 0600. On Windows
/// this is intentionally a no-op: `%APPDATA%\<app>` is already ACL-restricted to
/// the user, SYSTEM, and Administrators by default, so other standard users
/// cannot read it. Best-effort — logs on failure, never panics.
fn set_owner_only(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
            tracing::warn!("Failed to set 0600 on {}: {}", path.display(), e);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path; // Windows: AppData is already per-user ACL-protected.
    }
}

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

/// Load the cluster-name version counter. Missing/invalid file → 0 (pre-issue
/// default; an upgraded install starts unversioned and converges by origin).
pub fn load_network_name_version(app: &AppHandle) -> u64 {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("network_name_version", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(_) => return 0,
    };
    if let Ok(s) = fs::read_to_string(&path) {
        if let Ok(v) = s.trim().parse::<u64>() {
            return v;
        }
    }
    0
}

pub fn save_network_name_version(app: &AppHandle, version: u64) {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("network_name_version", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(_) => return,
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, version.to_string());
}

/// Load the device_id that set the current cluster name (tie-breaker). Missing
/// file → empty string; callers seed it with the local device_id at startup so
/// an unversioned install has a well-formed origin.
pub fn load_network_name_origin(app: &AppHandle) -> String {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("network_name_origin", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(_) => return String::new(),
    };
    if let Ok(s) = fs::read_to_string(&path) {
        let trimmed = s.trim().to_string();
        if !trimmed.is_empty() {
            return trimmed;
        }
    }
    String::new()
}

pub fn save_network_name_origin(app: &AppHandle, origin: &str) {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("network_name_origin", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(_) => return,
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, origin);
}

pub fn load_cluster_id(app: &AppHandle) -> Option<String> {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("cluster_id", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Failed to resolve cluster id path: {}", e);
            return None;
        }
    };

    if !path.exists() {
        return None;
    }

    match fs::read_to_string(&path) {
        Ok(s) => {
            let trimmed = s.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                tracing::debug!("Loaded cluster_id from disk.");
                Some(trimmed)
            }
        }
        Err(e) => {
            tracing::warn!("Failed to read cluster_id file: {}", e);
            None
        }
    }
}

pub fn save_cluster_id(app: &AppHandle, id: &str) {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("cluster_id", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to resolve cluster_id path for saving: {}", e);
            return;
        }
    };

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    if let Err(e) = fs::write(path, id) {
        tracing::error!("Failed to write cluster_id file: {}", e);
    } else {
        tracing::debug!("Saved cluster_id to disk.");
    }
}

/// Delete the legacy `cluster_key.bin` file from earlier versions. v0.3+
/// no longer treats the cluster key as a secret (mTLS replaces its role);
/// the file is wiped on first boot of the new build to avoid leaving a
/// stale 32-byte secret on disk.
pub fn wipe_legacy_cluster_key(app: &AppHandle) {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("cluster_key.bin", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(_) => return,
    };
    if path.exists() {
        match fs::remove_file(&path) {
            Ok(_) => tracing::info!("Wiped legacy cluster_key.bin from {:?}", path),
            Err(e) => tracing::warn!("Failed to wipe legacy cluster_key.bin: {}", e),
        }
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

pub fn load_device_cert(app: &AppHandle) -> Option<(Vec<u8>, Vec<u8>)> {
    let path_resolver = app.path();
    let cert_path = path_resolver
        .resolve("device_cert.der", BaseDirectory::AppConfig)
        .ok()?;
    let key_path = path_resolver
        .resolve("device_key.der", BaseDirectory::AppConfig)
        .ok()?;

    if !cert_path.exists() || !key_path.exists() {
        return None;
    }

    match (fs::read(&cert_path), fs::read(&key_path)) {
        (Ok(cert), Ok(key)) if !cert.is_empty() && !key.is_empty() => {
            tracing::debug!("Loaded device cert from disk.");
            Some((cert, key))
        }
        _ => None,
    }
}

pub fn save_device_cert(app: &AppHandle, cert_der: &[u8], key_der: &[u8]) {
    let path_resolver = app.path();
    let cert_path = match path_resolver.resolve("device_cert.der", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Failed to resolve device cert path: {}", e);
            return;
        }
    };
    let key_path = match path_resolver.resolve("device_key.der", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Failed to resolve device key path: {}", e);
            return;
        }
    };

    if let Some(parent) = cert_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    if let Err(e) = fs::write(&cert_path, cert_der) {
        tracing::error!("Failed to write device cert: {}", e);
        return;
    }
    if let Err(e) = fs::write(&key_path, key_der) {
        tracing::error!("Failed to write device key: {}", e);
        return;
    }
    // The private key must not be world-readable (issue: secret file perms).
    set_owner_only(&key_path);
    tracing::debug!("Saved device cert to disk.");
}

/// Re-apply owner-only permissions to the on-disk secret files if they exist.
/// Run once at startup so installs created before this hardening landed get
/// fixed — `device_key.der` in particular is written only at first launch and
/// never rewritten, so the write-path hardening alone would never reach it.
pub fn harden_secret_files(app: &AppHandle) {
    let path_resolver = app.path();
    for name in ["device_key.der", "network_pin"] {
        if let Ok(path) = path_resolver.resolve(name, BaseDirectory::AppConfig) {
            if path.exists() {
                set_owner_only(&path);
            }
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

/// Generate a fresh 6-character lowercase-alphanumeric pairing PIN. No disk I/O.
fn generate_pin() -> String {
    const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    (0..6)
        .map(|_| {
            let idx = rand::thread_rng().gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

pub fn load_network_pin(app: &AppHandle) -> String {
    let path_resolver = app.path();
    let path = match path_resolver.resolve("network_pin", BaseDirectory::AppConfig) {
        Ok(p) => p,
        Err(_) => return String::from("000000"),
    };

    if path.exists() {
        if let Ok(pin) = fs::read_to_string(&path) {
            // Trim defensively. The PIN is fed straight into SPAKE2 as the
            // shared password, so even a single trailing byte (newline,
            // space) on one side and not the other makes the derived AEAD
            // sub-keys diverge and pairing fails on T2 — and the user-facing
            // symptom is a misleading "Pairing session expired"-class error
            // with no hint that whitespace was the cause. A legacy
            // network_pin file written by an older build (or hand-edited)
            // gets healed on the next load without any migration code.
            let trimmed = pin.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }

    // Generate a new PIN and persist it (provisioned-mode path / lazy default).
    let pin = generate_pin();
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
    // Mirror the trim done on load_network_pin — keeps the on-disk file
    // canonical (no trailing whitespace from a pasted Settings input) so
    // even a build without the load-side trim would behave correctly.
    if fs::write(&path, pin.trim()).is_ok() {
        // The pairing PIN must not be world-readable (issue: secret file perms).
        set_owner_only(&path);
    }
}
/// Whether this device's pairing PIN should be persisted to disk. Only
/// provisioned mode keeps a stable, user-set PIN across restarts; auto mode is
/// ephemeral (issue 4).
pub(crate) fn pin_should_persist(mode: &str) -> bool {
    mode == "provisioned"
}

/// Establish this device's pairing PIN for the given cluster mode.
///
/// Provisioned mode persists the PIN (a user-set, memorable value must survive
/// restarts), so it reads (and lazily generates + saves) from disk. Auto mode
/// keeps the PIN ephemeral: any on-disk `network_pin` file is deleted and a
/// fresh PIN is generated in memory, never written to disk. The PIN is only
/// needed live during interactive pairing, so a per-launch value is sufficient
/// and avoids storing the secret. See issue 4.
pub fn establish_network_pin(app: &AppHandle, mode: &str) -> String {
    if pin_should_persist(mode) {
        return load_network_pin(app);
    }
    // Auto mode: delete any stored PIN, go ephemeral.
    if let Ok(path) = app.path().resolve("network_pin", BaseDirectory::AppConfig) {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }
    generate_pin()
}

// Helper to reset network state (Self-Destruct/Kick)
pub fn reset_network_state(app: &AppHandle) {
    let path_resolver = app.path();
    // Include the actual filenames used by load/save
    let config_files = [
        "cluster_id",
        "cluster_key.bin", // legacy from v0.2; deleted defensively
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
    #[serde(default)]
    pub ignore_extension_missing: bool,
    #[serde(default)]
    pub flatpak_autostart: bool,
    #[serde(default)]
    pub compress_file_transfers: bool,
    /// Per WIRE-PROTOCOL-0.3.1 §H7: when off, the responder logs only a
    /// generic "pairing failed" line on AEAD-decrypt failures, so a
    /// passive observer can't tell a wrong-PIN attempt apart from any
    /// other framing/decrypt error. Flip on for verbose pairing diagnostics.
    #[serde(default)]
    pub pairing_debug_logs: bool,
    /// User-controlled pause for the SPAKE pairing listener. When `false`,
    /// inbound TCP pairing connections are dropped immediately at the accept
    /// loop, alongside the existing `pairing_locked_out` brute-force defence.
    /// Surfaced in the UI as a header-bar toggle (issue #16).
    #[serde(default = "default_pairing_accept_enabled")]
    pub pairing_accept_enabled: bool,
    /// Issue #18: when off, the Windows firewall rule is NOT auto-created at
    /// startup. Default-on for backward compatibility. Windows-only effect.
    #[serde(default = "default_true")]
    pub configure_firewall: bool,
    /// Issue #18: when off, the device does not advertise itself over mDNS
    /// (browsing/discovery of others stays active). Default-on.
    #[serde(default = "default_true")]
    pub mdns_advertising: bool,
}

fn default_pairing_accept_enabled() -> bool {
    true
}

fn default_true() -> bool {
    true
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
            ignore_extension_missing: false,
            flatpak_autostart: false,
            compress_file_transfers: false,
            pairing_debug_logs: false,
            pairing_accept_enabled: true,
            configure_firewall: true,
            mdns_advertising: true,
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

#[cfg(all(test, unix))]
mod perms_tests {
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn set_owner_only_sets_0600() {
        // Create a world-readable temp file, then harden it.
        let path = std::env::temp_dir().join(format!(
            "clustercut_perms_test_{}",
            std::process::id()
        ));
        std::fs::write(&path, b"secret").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        super::set_owner_only(&path);

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        let _ = std::fs::remove_file(&path);
        assert_eq!(mode & 0o777, 0o600, "expected 0600, got {:o}", mode & 0o777);
    }
}

#[cfg(test)]
mod settings_tests {
    use super::AppSettings;

    #[test]
    fn missing_new_fields_default_to_true() {
        // A settings.json written before issue #18 has neither field.
        // They must deserialize to `true`, not bool's serde default of false.
        let json = r#"{
            "custom_device_name": null,
            "cluster_mode": "auto",
            "auto_send": true,
            "auto_receive": true,
            "notifications": {"device_join": true, "device_leave": true, "data_sent": false, "data_received": false},
            "shortcut_send": null,
            "shortcut_receive": null,
            "enable_file_transfer": true,
            "max_auto_download_size": 52428800,
            "notify_large_files": true
        }"#;
        let s: AppSettings = serde_json::from_str(json).unwrap();
        assert!(s.configure_firewall);
        assert!(s.mdns_advertising);
    }

    #[test]
    fn explicit_false_round_trips() {
        let mut s = AppSettings::default();
        s.configure_firewall = false;
        s.mdns_advertising = false;
        let json = serde_json::to_string(&s).unwrap();
        let back: AppSettings = serde_json::from_str(&json).unwrap();
        assert!(!back.configure_firewall);
        assert!(!back.mdns_advertising);
    }

    #[test]
    fn defaults_are_true() {
        let s = AppSettings::default();
        assert!(s.configure_firewall);
        assert!(s.mdns_advertising);
    }
}

#[cfg(test)]
mod pin_tests {
    use super::{generate_pin, pin_should_persist};

    #[test]
    fn generate_pin_is_six_lowercase_alnum() {
        let pin = generate_pin();
        assert_eq!(pin.len(), 6, "pin was {:?}", pin);
        assert!(
            pin.chars().all(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "unexpected chars in {:?}",
            pin
        );
    }

    #[test]
    fn only_provisioned_persists() {
        assert!(pin_should_persist("provisioned"));
        assert!(!pin_should_persist("auto"));
        assert!(!pin_should_persist("something-else"));
    }
}
