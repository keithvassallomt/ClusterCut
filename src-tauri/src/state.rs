use crate::peer::Peer;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
// use crate::crypto::SpakeState; // We'll just use explicit path or generic if needed, but explicit path is best.
// actually, let's use Any or just simple wrapper if circular dep is issue.
// But valid rust module path is crate::crypto::SpakeState

#[derive(Clone)]
pub struct AppState {
    pub peers: Arc<Mutex<HashMap<String, Peer>>>,
    // Store pending handshakes map: PeerID -> SpakeState
    // We need to wrap SpakeState to be Send + Sync (it should be)
    pub pending_handshakes: Arc<Mutex<HashMap<String, crate::crypto::SpakeState>>>,
    // Store trusted keys: PeerID -> Shared Key (32 bytes usually)
    pub trusted_keys: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    pub local_device_id: Arc<Mutex<String>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            peers: Arc::new(Mutex::new(HashMap::new())),
            pending_handshakes: Arc::new(Mutex::new(HashMap::new())),
            trusted_keys: Arc::new(Mutex::new(HashMap::new())),
            local_device_id: Arc::new(Mutex::new(String::new())),
        }
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
