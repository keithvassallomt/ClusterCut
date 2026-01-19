use crate::peer::Peer;
use crate::storage::AppSettings;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
// use crate::crypto::SpakeState; // We'll just use explicit path or generic if needed, but explicit path is best.
// actually, let's use Any or just simple wrapper if circular dep is issue.
// But valid rust module path is crate::crypto::SpakeState

#[derive(Clone)]
pub struct AppState {
    pub peers: Arc<Mutex<HashMap<String, Peer>>>,
    // Store pending handshakes map: PeerID -> SpakeState
    pub pending_handshakes: Arc<Mutex<HashMap<String, crate::crypto::SpakeState>>>,
    // Store completed session keys waiting for Welcome packet: Addr -> SessionKey
    pub handshake_sessions: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    // Shared Network Key (One key to rule them all)
    pub cluster_key: Arc<Mutex<Option<Vec<u8>>>>,
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
    // Transport instance for sending messages from commands
    pub transport: Arc<Mutex<Option<crate::transport::Transport>>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            peers: Arc::new(Mutex::new(HashMap::new())),
            pending_handshakes: Arc::new(Mutex::new(HashMap::new())),
            handshake_sessions: Arc::new(Mutex::new(HashMap::new())),
            cluster_key: Arc::new(Mutex::new(None)),
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
            transport: Arc::new(Mutex::new(None)),
        }
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    pub fn add_peer(&self, peer: Peer) {
        let mut peers = self.peers.lock().unwrap();
        peers.insert(peer.id.clone(), peer);
    }

    pub fn get_peers(&self) -> HashMap<String, Peer> {
        let peers = self.peers.lock().unwrap();
        peers.clone()
    }
}
