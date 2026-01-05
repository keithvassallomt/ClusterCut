use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Message {
    Clipboard(Vec<u8>),
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
    },
}
