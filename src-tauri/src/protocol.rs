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
    // Sent by Responder to Initiator after successful handshake
    Welcome {
        encrypted_cluster_key: Vec<u8>, // Encrypted with SPAKE2+ session key
        known_peers: Vec<crate::peer::Peer>,
        network_name: String,
        network_pin: String,
    },
    // Gossip: Broadcast new peer to known peers
    PeerDiscovery(crate::peer::Peer),
    // Broadcast removal of a peer (kick/leave)
    PeerRemoval(String), // Payload is device_id
    // Broadcast deletion of history item
    HistoryDelete(String), // Payload is item ID
    // Encrypted File Request (FileRequestPayload)
    FileRequest(Vec<u8>),
}
