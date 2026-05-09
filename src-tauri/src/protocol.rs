use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileMetadata {
    pub name: String,
    pub size: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ClipboardPayload {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub files: Option<Vec<FileMetadata>>,
    pub timestamp: u64,
    pub sender: String,
    pub sender_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileRequestPayload {
    pub id: String,        // Matches ClipboardPayload.id (which identifies the batch)
    pub file_index: usize, // Which file in the list?
    pub offset: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileStreamHeader {
    pub id: String, // Message/Batch ID
    pub file_index: usize,
    pub file_name: String,
    pub file_size: u64,
    pub auth_token: String, // Encrypted token proving Cluster Key possession
    // Whether the payload following the header line is zstd-compressed.
    // Defaults to false so headers from peers <= 0.2.2 (which omit the field) parse cleanly.
    #[serde(default)]
    pub compressed: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WelcomePayload {
    pub cluster_key: Vec<u8>,
    pub known_peers: Vec<crate::peer::Peer>,
    pub network_name: String,
    pub network_pin: String,
    // SHA-256 of the responder's TLS cert DER. The initiator pins this for
    // all future connections to the responder. Optional for backward compat
    // with peers paired before cert-pinning landed.
    #[serde(default)]
    pub responder_fingerprint: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PairFingerprintPayload {
    pub device_id: String,
    pub fingerprint: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Message {
    Clipboard(Vec<u8>), // Encrypted ClipboardPayload
    PairRequest {
        msg: Vec<u8>,
        device_id: String,
    },
    PairResponse {
        msg: Vec<u8>,
        device_id: String,
    },
    // Sent by Responder to Initiator after successful handshake.
    // Entire payload (cluster key, known peers, network name, PIN) is JSON-serialized
    // then encrypted with the SPAKE2+ session key — the unauthenticated TLS tunnel
    // must be assumed MITM-readable until cert-pinning lands (see issue #9).
    Welcome {
        encrypted_payload: Vec<u8>,
    },
    // Sent by Initiator to Responder after Welcome, completing the bidirectional
    // fingerprint exchange. Payload is JSON-serialized PairFingerprintPayload
    // encrypted with the SPAKE2+ session key — bound to the SPAKE2 transcript
    // so a MITM cannot substitute the initiator's fingerprint (see issue #9).
    PairFingerprint {
        encrypted_payload: Vec<u8>,
    },
    // Gossip: Broadcast new peer to known peers
    PeerDiscovery(crate::peer::Peer),
    // Broadcast removal of a peer (kick/leave)
    PeerRemoval(String), // Payload is device_id
    // Broadcast deletion of history item
    HistoryDelete(String), // Payload is item ID
    // Encrypted File Request (FileRequestPayload)
    FileRequest(Vec<u8>),
    // Liveness Check
    Ping,
    Pong,
}
