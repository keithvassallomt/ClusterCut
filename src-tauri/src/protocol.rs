use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileMetadata {
    pub name: String,
    pub size: u64,
}

// In-memory clipboard image data, normalised to PNG on the wire.
// Width/height are hints for the receiver (UI thumbnails, debug logs).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ClipboardBlob {
    pub mime_type: String,
    pub data: Vec<u8>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ClipboardPayload {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub files: Option<Vec<FileMetadata>>,
    #[serde(default)]
    pub blob: Option<ClipboardBlob>,
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
    // Liveness Check
    Ping,
    Pong,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_blob() -> ClipboardBlob {
        ClipboardBlob {
            mime_type: "image/png".to_string(),
            data: vec![
                0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
                0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
            ],
            width: Some(640),
            height: Some(480),
        }
    }

    fn sample_payload(blob: Option<ClipboardBlob>) -> ClipboardPayload {
        ClipboardPayload {
            id: "test-id".to_string(),
            text: String::new(),
            files: None,
            blob,
            timestamp: 1_700_000_000,
            sender: "test-host".to_string(),
            sender_id: "test-id-123".to_string(),
        }
    }

    #[test]
    fn clipboard_blob_round_trip_json() {
        let blob = sample_blob();
        let payload = sample_payload(Some(blob.clone()));
        let json = serde_json::to_vec(&payload).expect("serialize");
        let parsed: ClipboardPayload = serde_json::from_slice(&json).expect("deserialize");
        assert_eq!(parsed.blob.expect("blob preserved"), blob);
        assert_eq!(parsed.id, "test-id");
    }

    #[test]
    fn clipboard_payload_pre_blob_format_deserializes_with_no_blob() {
        // Simulates a payload from a peer running ClusterCut <= 0.2.3 (no `blob` field).
        let old_json = r#"{
            "id": "abc",
            "text": "hello",
            "files": null,
            "timestamp": 1234,
            "sender": "old-peer",
            "sender_id": "id-old"
        }"#;
        let parsed: ClipboardPayload = serde_json::from_str(old_json).expect("parse old");
        assert!(parsed.blob.is_none());
        assert_eq!(parsed.text, "hello");
    }

    #[test]
    fn clipboard_blob_round_trip_through_encryption() {
        let key = [0u8; 32];
        let blob = sample_blob();
        let payload = sample_payload(Some(blob.clone()));
        let json = serde_json::to_vec(&payload).unwrap();
        let cipher = crate::crypto::encrypt(&key, &json).unwrap();
        let plain = crate::crypto::decrypt(&key, &cipher).unwrap();
        let recovered: ClipboardPayload = serde_json::from_slice(&plain).unwrap();
        assert_eq!(recovered.blob.expect("blob survives crypto"), blob);
    }

    #[test]
    fn clipboard_blob_omits_dimensions_when_none() {
        // Width/height are #[serde(default)] — payloads without them should still parse.
        let blob_json = r#"{"mime_type":"image/png","data":[1,2,3]}"#;
        let parsed: ClipboardBlob = serde_json::from_str(blob_json).unwrap();
        assert_eq!(parsed.mime_type, "image/png");
        assert_eq!(parsed.data, vec![1, 2, 3]);
        assert!(parsed.width.is_none());
        assert!(parsed.height.is_none());
    }
}
