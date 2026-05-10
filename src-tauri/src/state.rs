use crate::peer::Peer;
use crate::storage::AppSettings;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// One entry surfaced to the UI for the "needs re-pair" banner. A peer
/// loaded from `known_peers.json` with `fingerprint = None` predates the
/// v0.3 strict-mTLS model and can no longer be reached over QUIC until
/// re-paired. The frontend lists these on a banner so the user knows
/// who's affected and how to fix it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LegacyPeerInfo {
    pub id: String,
    pub hostname: String,
}

/// One entry in `AppState.local_clipboard_blobs` — everything the sender's
/// `FileRequest` handler needs to serve a clipboard-blob fetch back to the
/// requesting peer with the right `delivery_target` hint and clipboard
/// metadata. Created when the sender writes a >10 MB image to a temp file
/// and broadcasts a descriptor on `Message::Clipboard`.
#[derive(Debug, Clone)]
pub struct ClipboardBlobMetadata {
    pub path: std::path::PathBuf,
    pub mime_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub total_size: u64,
}
#[derive(Clone)]
pub struct AppState {
    pub peers: Arc<Mutex<HashMap<String, Peer>>>,
    // Cluster identifier (UUID). Replaces the v0.2 `cluster_key` shared
    // secret — secrecy/auth is now provided by mTLS, and this is just a
    // non-secret handle for grouping in the UI and for gossip-loop
    // suppression. Generated at first boot of a fresh cluster.
    pub cluster_id: Arc<Mutex<String>>,
    /// Peers loaded from `known_peers.json` without a pinned fingerprint
    /// (pre-mTLS pairings). Surfaced to the UI for a one-time
    /// "please re-pair" banner. Cleared as each entry gets re-paired.
    pub legacy_peers: Arc<Mutex<Vec<LegacyPeerInfo>>>,
    // Known Peers (Persisted list of devices we know about)
    pub known_peers: Arc<Mutex<HashMap<String, Peer>>>,
    pub local_device_id: Arc<Mutex<String>>,
    // Discovery Service
    pub discovery: Arc<Mutex<Option<crate::discovery::Discovery>>>,
    // Last Clipboard Content (for deduplication and loop prevention)
    pub last_clipboard_content: Arc<Mutex<String>>,
    // Human Readable Network Name
    pub network_name: Arc<Mutex<String>>,
    // Network PIN (6-char alphanumeric, for auto-joining)
    // Network PIN (6-char alphanumeric, for auto-joining)
    pub network_pin: Arc<Mutex<String>>,
    // App Settings
    pub settings: Arc<Mutex<AppSettings>>,
    // Pending Removals (Debounce for mDNS)
    pub pending_removals: Arc<Mutex<HashMap<String, u64>>>,
    // Pending Clipboard Content (Received but not yet applied due to Auto-Receive OFF)
    pub pending_clipboard: Arc<Mutex<Option<crate::protocol::ClipboardPayload>>>,
    // Shutdown flag for graceful termination of background threads
    pub shutdown: Arc<AtomicBool>,
    // Mapping of Message ID -> File Paths (for serving file requests)
    // Mapping of Message ID -> File Paths (for serving file requests)
    pub local_files: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// Sender-side registry of large clipboard blobs that ride the file-
    /// transfer ALPN (§3.3 Tier B). Keyed by the originating
    /// `ClipboardPayload.id`. When a peer responds to the descriptor
    /// broadcast with a `FileRequest` for this id, we pull the entry, open
    /// the temp file at `path`, and stream it under
    /// `delivery_target = Clipboard { mime, w, h }`. Receivers know to land
    /// the bytes on their OS clipboard rather than disk.
    pub local_clipboard_blobs: Arc<Mutex<HashMap<String, ClipboardBlobMetadata>>>,
    /// Receiver-side: the `ClipboardPayload.id` of a clipboard-blob fetch
    /// currently in flight, or `None` if idle. Used so a fresh clipboard
    /// event arriving mid-fetch can mark the older fetch as abandoned —
    /// bytes still drain off the wire to keep QUIC happy, but they don't
    /// overwrite the OS clipboard once they finish landing. Newest copy wins.
    pub in_flight_clipboard_fetch: Arc<Mutex<Option<String>>>,
    // Transport instance for sending messages from commands
    pub transport: Arc<Mutex<Option<crate::transport::Transport>>>,
    // Tray Menu Handle
    pub tray_menu: Arc<Mutex<Option<tauri::menu::Menu<tauri::Wry>>>>,
    // Current Theme (Linux workaround)
    pub current_theme: Arc<Mutex<Option<String>>>,
    // Startup Time (for notification suppression)
    pub startup_time: std::time::Instant,
    // Network State (for notification suppression during outages/suspend)
    pub network_available: Arc<AtomicBool>,
    pub network_suspended: Arc<AtomicBool>,
    pub resume_grace_until: Arc<Mutex<Option<std::time::Instant>>>,
    pub last_known_local_ip: Arc<Mutex<Option<std::net::IpAddr>>>,
    // Deferred join notifications (peer IDs awaiting ping verification)
    pub pending_join_notifications: Arc<Mutex<HashSet<String>>>,
    // Heartbeat fallback counter (consecutive rounds where all sends failed)
    pub consecutive_heartbeat_failures: Arc<AtomicU32>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            peers: Arc::new(Mutex::new(HashMap::new())),
            cluster_id: Arc::new(Mutex::new(String::new())),
            legacy_peers: Arc::new(Mutex::new(Vec::new())),
            known_peers: Arc::new(Mutex::new(HashMap::new())),
            local_device_id: Arc::new(Mutex::new(String::new())),
            discovery: Arc::new(Mutex::new(None)),
            last_clipboard_content: Arc::new(Mutex::new(String::new())),
            network_name: Arc::new(Mutex::new(String::new())),
            network_pin: Arc::new(Mutex::new(String::new())),
            settings: Arc::new(Mutex::new(AppSettings::default())),
            pending_removals: Arc::new(Mutex::new(HashMap::new())),
            pending_clipboard: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(AtomicBool::new(false)),
            local_files: Arc::new(Mutex::new(HashMap::new())),
            local_clipboard_blobs: Arc::new(Mutex::new(HashMap::new())),
            in_flight_clipboard_fetch: Arc::new(Mutex::new(None)),
            transport: Arc::new(Mutex::new(None)),
            tray_menu: Arc::new(Mutex::new(None)),
            current_theme: Arc::new(Mutex::new(None)),
            startup_time: std::time::Instant::now(),
            network_available: Arc::new(AtomicBool::new(true)),
            network_suspended: Arc::new(AtomicBool::new(false)),
            resume_grace_until: Arc::new(Mutex::new(None)),
            last_known_local_ip: Arc::new(Mutex::new(None)),
            pending_join_notifications: Arc::new(Mutex::new(HashSet::new())),
            consecutive_heartbeat_failures: Arc::new(AtomicU32::new(0)),
        }
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    /// Look up the pinned cert fingerprint for the peer at `addr`, used by
    /// the *client* side of QUIC handshakes to verify the responder's cert.
    /// Returns None for peers not in known_peers or peers without a pinned
    /// fingerprint (those need to re-pair under the v0.3 strict mTLS model).
    pub fn fingerprint_for(&self, addr: std::net::SocketAddr) -> Option<Vec<u8>> {
        let peers = self.known_peers.lock().unwrap();
        peers
            .values()
            .find(|p| p.ip == addr.ip())
            .and_then(|p| p.fingerprint.clone())
    }

    /// True if `fp` matches the pinned fingerprint of any peer in
    /// known_peers. Used by the *server* side of QUIC handshakes (mTLS
    /// client-cert validation) where we only know the presented cert,
    /// not which peer is connecting.
    pub fn knows_fingerprint(&self, fp: &[u8]) -> bool {
        let peers = self.known_peers.lock().unwrap();
        peers
            .values()
            .any(|p| p.fingerprint.as_deref() == Some(fp))
    }

    pub fn add_peer(&self, peer: Peer) {
        let mut peers = self.peers.lock().unwrap();
        peers.insert(peer.id.clone(), peer);
    }

    pub fn get_peers(&self) -> HashMap<String, Peer> {
        let peers = self.peers.lock().unwrap();
        peers.clone()
    }

    pub fn should_notify(&self) -> bool {
        if self.startup_time.elapsed() < std::time::Duration::from_secs(60) {
            return false;
        }
        if !self.network_available.load(Ordering::Relaxed) {
            return false;
        }
        if self.network_suspended.load(Ordering::Relaxed) {
            return false;
        }
        if let Some(end) = *self.resume_grace_until.lock().unwrap() {
            if std::time::Instant::now() < end {
                return false;
            }
        }
        true
    }
}
