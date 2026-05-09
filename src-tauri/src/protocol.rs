use base64::Engine as _;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileMetadata {
    pub name: String,
    pub size: u64,
}

// In-memory clipboard image data, normalised to PNG on the wire.
// Width/height are hints for the receiver (UI thumbnails, debug logs).
//
// `data` is **base64-encoded** because serde_json serialises a raw `Vec<u8>`
// as a JSON array of integers (`[1,2,3,…]`), which adds ~3.5× per-byte bloat.
// Base64 is ~1.33× — a >2× wire reduction for typical clipboard images.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ClipboardBlob {
    pub mime_type: String,
    pub data: String,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
}

impl ClipboardBlob {
    /// Construct from raw image bytes; encodes them as base64 internally so
    /// callers don't have to know the wire format.
    pub fn from_bytes(
        mime_type: impl Into<String>,
        bytes: &[u8],
        width: Option<u32>,
        height: Option<u32>,
    ) -> Self {
        Self {
            mime_type: mime_type.into(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            width,
            height,
        }
    }

    /// Decode the base64 wire form back into raw bytes ready to feed into
    /// the OS clipboard. Returns the decoded byte vector or a descriptive error.
    pub fn raw_bytes(&self) -> Result<Vec<u8>, String> {
        base64::engine::general_purpose::STANDARD
            .decode(self.data.as_bytes())
            .map_err(|e| format!("invalid base64 in ClipboardBlob.data: {}", e))
    }

    /// Length of the decoded raw bytes — used for size cap checks before
    /// putting a blob on the wire.
    pub fn decoded_len(&self) -> usize {
        // Cheap estimate: 4 base64 chars decode to 3 bytes; ignore padding for
        // this purpose (the cap is approximate anyway).
        (self.data.len() / 4) * 3
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_bytes() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A,
            0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
        ]
    }

    fn sample_blob() -> ClipboardBlob {
        ClipboardBlob::from_bytes("image/png", &sample_bytes(), Some(640), Some(480))
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
        let blob_json = r#"{"mime_type":"image/png","data":"AQID"}"#;
        let parsed: ClipboardBlob = serde_json::from_str(blob_json).unwrap();
        assert_eq!(parsed.mime_type, "image/png");
        assert_eq!(parsed.raw_bytes().unwrap(), vec![1, 2, 3]);
        assert!(parsed.width.is_none());
        assert!(parsed.height.is_none());
    }

    #[test]
    fn clipboard_blob_data_is_base64_on_wire_not_int_array() {
        // Regression guard: serde_json must emit `data` as a base64 string,
        // not the bloat-prone `[123,45,67,...]` integer array.
        let blob = sample_blob();
        let json = serde_json::to_string(&blob).unwrap();
        assert!(json.contains("\"data\":\""), "data should be a JSON string, got: {}", json);
        assert!(!json.contains("\"data\":["), "data must NOT be a JSON array, got: {}", json);
    }

    #[test]
    fn clipboard_blob_round_trip_bytes() {
        let original = sample_bytes();
        let blob = ClipboardBlob::from_bytes("image/png", &original, None, None);
        assert_eq!(blob.raw_bytes().unwrap(), original);
    }
}
