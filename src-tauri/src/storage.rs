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
    tracing::info!("Generated a new network PIN.");
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

/// Whether a device joining a cluster should adopt the PIN it just paired with
/// as its own `network_pin`. Only provisioned clusters share a single PIN: the
/// joiner already typed the responder's (== the cluster's) PIN to complete
/// SPAKE2, so adopting it makes every device converge onto the admin's common
/// PIN. Auto clusters keep per-device ephemeral PINs, so they never adopt.
pub(crate) fn should_adopt_cluster_pin(mode: &str) -> bool {
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

/// Regenerate just the cluster NAME: delete the on-disk name file and return a
/// fresh generated name (`load_network_name` regenerates and persists it). The
/// PIN is handled separately by `establish_network_pin` so its persistence
/// follows the cluster mode (ephemeral in auto — issue 4).
pub fn regenerate_network_name(app: &AppHandle) -> String {
    let path_resolver = app.path();
    if let Ok(path) = path_resolver.resolve("network_name", BaseDirectory::AppConfig) {
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }
    load_network_name(app)
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
    /// Send Clipboard: sends what's already on the clipboard.
    pub shortcut_send: Option<String>,
    /// Receive Only: applies the pending content without pasting.
    pub shortcut_receive: Option<String>,
    /// Copy & Send: presses Copy in the focused app, then sends (issue #21).
    #[serde(default)]
    pub shortcut_copy_send: Option<String>,
    /// Cut & Send: presses Cut in the focused app, then sends.
    #[serde(default)]
    pub shortcut_cut_send: Option<String>,
    /// Receive & Paste: applies the pending content, then presses Paste.
    #[serde(default)]
    pub shortcut_receive_paste: Option<String>,
    /// Keep My Clipboard: Copy & Send, Cut & Send and Receive & Paste put back
    /// what was on the clipboard before.
    #[serde(default)]
    pub keep_local_clipboard: bool,
    /// Bumped by `migrate_shortcuts`; 0 for settings written before issue #21.
    #[serde(default)]
    pub shortcuts_version: u32,
    /// RemoteDesktop portal restore token (KDE): skips the desktop's permission
    /// dialog when the one-step shortcuts press keys. Single use; replaced after
    /// every session. Backend-owned.
    #[serde(default)]
    pub remote_desktop_token: Option<String>,
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
    /// Max bytes of re-callable clipboard content (text + images) the History
    /// content store retains, across RAM + disk tiers. File transfers don't
    /// count. Default 200 MB; oldest entries evict first when exceeded.
    #[serde(default = "default_history_store_max_bytes")]
    pub history_store_max_bytes: u64,
    /// How long History keeps entries (issue #22).
    #[serde(default)]
    pub history_retention: HistoryRetention,
    /// How many entries History keeps (issue #22); the oldest go first.
    #[serde(default = "default_history_max_items")]
    pub history_max_items: u32,
    /// Hyprland only: bind the manual shortcuts live over Hyprland's IPC, since
    /// its shortcut portal never assigns keys. Opt-in; no config files touched.
    #[serde(default)]
    pub hyprland_bind_keys: bool,
}

fn default_pairing_accept_enabled() -> bool {
    true
}

fn default_true() -> bool {
    true
}

fn default_history_store_max_bytes() -> u64 {
    200 * 1024 * 1024
}

fn default_history_max_items() -> u32 {
    50
}

/// How long History keeps an entry. History never outlives ClusterCut: it's
/// held in memory, plus staged files cleared on exit.
#[derive(serde::Serialize, serde::Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HistoryRetention {
    #[serde(rename = "off")]
    Off,
    #[serde(rename = "15m")]
    Minutes15,
    #[serde(rename = "1h")]
    Hour,
    #[serde(rename = "1d")]
    Day,
    #[serde(rename = "1w")]
    Week,
    #[default]
    #[serde(rename = "until_quit")]
    UntilQuit,
}

impl AppSettings {
    /// History's limits: how many entries (0 while it's off) and how old.
    pub fn history_limits(&self) -> (usize, Option<std::time::Duration>) {
        let minutes = |m: u64| Some(std::time::Duration::from_secs(m * 60));
        let max_age = match self.history_retention {
            HistoryRetention::Off => return (0, None),
            HistoryRetention::Minutes15 => minutes(15),
            HistoryRetention::Hour => minutes(60),
            HistoryRetention::Day => minutes(24 * 60),
            HistoryRetention::Week => minutes(7 * 24 * 60),
            HistoryRetention::UntilQuit => None,
        };
        (self.history_max_items.clamp(1, 1000) as usize, max_age)
    }

    pub fn history_off(&self) -> bool {
        self.history_retention == HistoryRetention::Off
    }
}

const DEFAULT_COPY_SEND: &str = "CommandOrControl+Alt+C";
const DEFAULT_CUT_SEND: &str = "CommandOrControl+Alt+X";
const DEFAULT_SEND_CLIPBOARD: &str = "CommandOrControl+Alt+Shift+C";
const DEFAULT_RECEIVE_PASTE: &str = "CommandOrControl+Alt+V";
const DEFAULT_RECEIVE_ONLY: &str = "CommandOrControl+Alt+Shift+V";
const SHORTCUTS_VERSION: u32 = 1;

/// Same keys, whatever the order or case ("Alt+CommandOrControl+c").
fn same_accelerator(a: &str, b: &str) -> bool {
    let parts = |s: &str| {
        let mut parts: Vec<String> = s.split('+').map(|p| p.trim().to_ascii_lowercase()).collect();
        parts.sort();
        parts
    };
    parts(a) == parts(b)
}

/// Issue #21 moved the old Send/Receive defaults (Ctrl+Alt+C/V) to the new
/// Copy & Send and Receive & Paste. Settings still on those defaults move
/// Send Clipboard and Receive Only to +Shift; customised keys keep their
/// meaning, and a new default that would collide with one stays unset.
/// Returns whether anything changed.
fn migrate_shortcuts(s: &mut AppSettings) -> bool {
    if s.shortcuts_version >= SHORTCUTS_VERSION {
        return false;
    }
    let is = |stored: &Option<String>, default: &str| stored.as_deref().is_some_and(|k| same_accelerator(k, default));
    if is(&s.shortcut_send, DEFAULT_COPY_SEND) {
        s.shortcut_send = Some(DEFAULT_SEND_CLIPBOARD.to_string());
    }
    if is(&s.shortcut_receive, DEFAULT_RECEIVE_PASTE) {
        s.shortcut_receive = Some(DEFAULT_RECEIVE_ONLY.to_string());
    }
    let taken = [s.shortcut_send.clone(), s.shortcut_receive.clone()];
    for (field, default) in [
        (&mut s.shortcut_copy_send, DEFAULT_COPY_SEND),
        (&mut s.shortcut_cut_send, DEFAULT_CUT_SEND),
        (&mut s.shortcut_receive_paste, DEFAULT_RECEIVE_PASTE),
    ] {
        if field.is_none() && !taken.iter().any(|t| is(t, default)) {
            *field = Some(default.to_string());
        }
    }
    s.shortcuts_version = SHORTCUTS_VERSION;
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
            shortcut_send: Some(DEFAULT_SEND_CLIPBOARD.to_string()),
            shortcut_receive: Some(DEFAULT_RECEIVE_ONLY.to_string()),
            shortcut_copy_send: Some(DEFAULT_COPY_SEND.to_string()),
            shortcut_cut_send: Some(DEFAULT_CUT_SEND.to_string()),
            shortcut_receive_paste: Some(DEFAULT_RECEIVE_PASTE.to_string()),
            keep_local_clipboard: false,
            shortcuts_version: SHORTCUTS_VERSION,
            remote_desktop_token: None,
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
            history_store_max_bytes: 200 * 1024 * 1024,
            history_retention: HistoryRetention::UntilQuit,
            history_max_items: 50,
            hyprland_bind_keys: false,
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

    let mut settings: AppSettings = match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => return AppSettings::default(),
    };
    // Saved straight away, so a key later set back to an old default isn't moved again.
    if migrate_shortcuts(&mut settings) {
        tracing::info!(
            "Migrated shortcuts for issue #21: send={:?} receive={:?} copy_send={:?} cut_send={:?} receive_paste={:?}",
            settings.shortcut_send,
            settings.shortcut_receive,
            settings.shortcut_copy_send,
            settings.shortcut_cut_send,
            settings.shortcut_receive_paste
        );
        save_settings(app, &settings);
    }
    settings
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

    /// Settings as written before issue #21, with the given Send/Receive keys.
    fn v0(send: Option<&str>, receive: Option<&str>) -> AppSettings {
        let mut s = AppSettings::default();
        s.shortcut_send = send.map(str::to_string);
        s.shortcut_receive = receive.map(str::to_string);
        s.shortcut_copy_send = None;
        s.shortcut_cut_send = None;
        s.shortcut_receive_paste = None;
        s.shortcuts_version = 0;
        s
    }

    fn keys(s: &AppSettings) -> [Option<&str>; 5] {
        [
            s.shortcut_copy_send.as_deref(),
            s.shortcut_cut_send.as_deref(),
            s.shortcut_send.as_deref(),
            s.shortcut_receive_paste.as_deref(),
            s.shortcut_receive.as_deref(),
        ]
    }

    #[test]
    fn migrates_old_defaults_to_the_new_layout() {
        let mut s = v0(Some("CommandOrControl+Alt+C"), Some("CommandOrControl+Alt+V"));
        assert!(super::migrate_shortcuts(&mut s));
        assert_eq!(keys(&s), keys(&AppSettings::default()));
        assert!(!super::migrate_shortcuts(&mut s), "migrates once");
    }

    #[test]
    fn migration_keeps_customised_keys() {
        let mut s = v0(Some("Super+S"), Some("Super+R"));
        super::migrate_shortcuts(&mut s);
        assert_eq!(s.shortcut_send.as_deref(), Some("Super+S"));
        assert_eq!(s.shortcut_receive.as_deref(), Some("Super+R"));
        assert_eq!(s.shortcut_copy_send.as_deref(), Some("CommandOrControl+Alt+C"));
        assert_eq!(s.shortcut_receive_paste.as_deref(), Some("CommandOrControl+Alt+V"));
    }

    #[test]
    fn migration_leaves_colliding_defaults_unset() {
        let mut s = v0(Some("Alt+CommandOrControl+x"), None);
        super::migrate_shortcuts(&mut s);
        assert_eq!(s.shortcut_send.as_deref(), Some("Alt+CommandOrControl+x"));
        assert_eq!(s.shortcut_cut_send, None);
        assert_eq!(s.shortcut_copy_send.as_deref(), Some("CommandOrControl+Alt+C"));
        assert_eq!(s.shortcut_receive, None, "a cleared key stays cleared");
    }

    #[test]
    fn history_settings_default_to_todays_behaviour() {
        let json = serde_json::to_value(AppSettings::default()).unwrap();
        let mut obj = json.as_object().unwrap().clone();
        obj.remove("history_retention");
        obj.remove("history_max_items");
        let s: AppSettings = serde_json::from_value(obj.into()).unwrap();
        assert_eq!(s.history_retention, super::HistoryRetention::UntilQuit);
        assert_eq!(s.history_limits(), (50, None));
    }

    #[test]
    fn history_limits_follow_the_settings() {
        let mut s = AppSettings::default();
        s.history_retention = super::HistoryRetention::Minutes15;
        s.history_max_items = 10;
        assert_eq!(s.history_limits(), (10, Some(std::time::Duration::from_secs(900))));
        s.history_retention = super::HistoryRetention::Off;
        assert_eq!(s.history_limits(), (0, None));
        assert!(s.history_off());
        let back: super::HistoryRetention = serde_json::from_str("\"1w\"").unwrap();
        assert_eq!(back, super::HistoryRetention::Week);
    }

    #[test]
    fn settings_from_before_issue_21_load_as_version_0() {
        let json = serde_json::to_value(AppSettings::default()).unwrap();
        let mut obj = json.as_object().unwrap().clone();
        for field in ["shortcut_copy_send", "shortcut_cut_send", "shortcut_receive_paste", "shortcuts_version"] {
            obj.remove(field);
        }
        let s: AppSettings = serde_json::from_value(obj.into()).unwrap();
        assert_eq!(s.shortcuts_version, 0);
        assert_eq!(s.shortcut_copy_send, None);
    }
}

#[cfg(test)]
mod pin_tests {
    use super::{generate_pin, pin_should_persist, should_adopt_cluster_pin};

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

    #[test]
    fn only_provisioned_adopts_cluster_pin_on_join() {
        // A joiner only converges onto the cluster's shared PIN when it is in
        // provisioned mode. Auto mode keeps its per-device ephemeral PIN.
        assert!(should_adopt_cluster_pin("provisioned"));
        assert!(!should_adopt_cluster_pin("auto"));
        assert!(!should_adopt_cluster_pin("something-else"));
    }
}
