use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// serde adapter: serialise/deserialise `Vec<u8>` as a base64 JSON string
/// instead of the default `[1,2,3,…]` integer array. Used for the encrypted
/// `Message::Clipboard` payload, which can be tens of MB; the int-array
/// form inflates random-byte content by ~3.57× (measured), pushing
/// large blobs over the receiver's 64 MB transport cap. Base64 inflates
/// at ~1.33×, comfortably under the cap.
mod b64_bytes {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&BASE64.encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        BASE64.decode(s.as_bytes()).map_err(serde::de::Error::custom)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct FileMetadata {
    pub name: String,
    pub size: u64,
}

// In-memory clipboard image data.
// Width/height are hints for the receiver (UI thumbnails, debug logs).
//
// Two modes:
// - **Inline** (`fetch_id = None`): `data` carries the bytes base64-encoded.
//   Used for images ≤ MAX_CLIPBOARD_IMAGE_WIRE_BYTES.
// - **Descriptor** (`fetch_id = Some`): `data` is empty. The receiver fetches
//   the actual bytes via `Message::FileRequest` referencing `fetch_id` over
//   the existing `clustercut-file` ALPN stream, then writes them to its OS
//   clipboard under `mime_type`. Used for images > MAX_CLIPBOARD_IMAGE_WIRE_BYTES.
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
    /// Descriptor mode. When `Some`, `data` is empty and the receiver must
    /// fetch the bytes via `Message::FileRequest` referencing this id (which
    /// equals the parent `ClipboardPayload.id`). Older peers that omit this
    /// field deserialise it as `None` and treat the blob as inline — they
    /// just decode an empty `data`, which produces an empty image they
    /// quietly drop. Graceful degradation against pre-§3.3 peers.
    #[serde(default)]
    pub fetch_id: Option<String>,
    /// Total raw byte size of the eventual blob — receiver uses this to
    /// decide auto-fetch vs. user-confirm against `max_auto_download_size`.
    #[serde(default)]
    pub total_size: Option<u64>,
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
            fetch_id: None,
            total_size: None,
        }
    }

    /// Construct a descriptor blob: no inline bytes, just metadata so the
    /// receiver can decide whether to auto-fetch via the file-transfer ALPN
    /// stream or prompt the user to accept. `fetch_id` should equal the
    /// parent `ClipboardPayload.id`.
    pub fn descriptor(
        mime_type: impl Into<String>,
        fetch_id: impl Into<String>,
        total_size: u64,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Self {
        Self {
            mime_type: mime_type.into(),
            data: String::new(),
            width,
            height,
            fetch_id: Some(fetch_id.into()),
            total_size: Some(total_size),
        }
    }

    /// True if this blob is a descriptor (no inline bytes; receiver must
    /// fetch via `Message::FileRequest`).
    pub fn is_descriptor(&self) -> bool {
        self.fetch_id.is_some()
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

// One alternate clipboard representation alongside the primary plain `text`
// (e.g. text/html, text/rtf, image/svg+xml). Receivers re-stock these on
// the local clipboard so the destination app can pick whichever format it
// understands best — same buffet the sender saw.
//
// Text formats (text/html, text/rtf, image/svg+xml, etc.) carry their bytes
// directly as a UTF-8 string in `data` with `binary: false`. Binary formats
// (anything not safely round-trippable as UTF-8) base64-encode the bytes
// into `data` with `binary: true`. Same rationale as `ClipboardBlob.data`:
// serde_json would otherwise emit `Vec<u8>` as `[1,2,3,…]` which is ~3.5×
// larger than base64.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ClipboardFormat {
    pub mime_type: String,
    pub data: String,
    #[serde(default)]
    pub binary: bool,
}

impl ClipboardFormat {
    /// Construct from a UTF-8 string (text/html, text/rtf, image/svg+xml, …).
    pub fn from_text(mime_type: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            mime_type: mime_type.into(),
            data: text.into(),
            binary: false,
        }
    }

    /// Construct from arbitrary bytes — base64-encoded internally.
    pub fn from_bytes(mime_type: impl Into<String>, bytes: &[u8]) -> Self {
        Self {
            mime_type: mime_type.into(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
            binary: true,
        }
    }

    /// Decode `data` back into raw bytes, transparently base64-decoding when
    /// `binary == true`.
    pub fn raw_bytes(&self) -> Result<Vec<u8>, String> {
        if self.binary {
            base64::engine::general_purpose::STANDARD
                .decode(self.data.as_bytes())
                .map_err(|e| format!("invalid base64 in ClipboardFormat.data: {}", e))
        } else {
            Ok(self.data.as_bytes().to_vec())
        }
    }

    /// Decoded byte length — base64 estimate for binary, str length for text.
    /// Used for size-cap checks and dedup signatures.
    pub fn decoded_len(&self) -> usize {
        if self.binary {
            (self.data.len() / 4) * 3
        } else {
            self.data.len()
        }
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
    // Additional representations of the same copy event (text/html, text/rtf, …)
    // alongside the primary `text`. Receivers re-stock all that they can write
    // to their OS clipboard. Older peers without this field deserialize as None
    // and just paste plain text — graceful degradation.
    #[serde(default)]
    pub formats: Option<Vec<ClipboardFormat>>,
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

/// Where the receiver should land an incoming file-transfer stream. `Disk`
/// is the existing behaviour (write to `temp_downloads`, surface via
/// `file-received`, paste as a file reference). `Clipboard` is the §3.3
/// large-blob path: accumulate bytes in memory and write them to the OS
/// clipboard as an image under the given MIME / dims. The wire shape and
/// streaming/auth/ack machinery are identical for both — the only thing
/// that changes is what the receiver does with the bytes once delivered.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum DeliveryTarget {
    Disk,
    Clipboard {
        mime_type: String,
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
    },
}

impl Default for DeliveryTarget {
    fn default() -> Self {
        DeliveryTarget::Disk
    }
}

fn default_delivery_target() -> DeliveryTarget {
    DeliveryTarget::Disk
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
    /// Disk (existing default) or Clipboard. Receiver routes the stream's
    /// bytes accordingly. Older peers that omit the field deserialise as
    /// Disk and behave exactly as today.
    #[serde(default = "default_delivery_target")]
    pub delivery_target: DeliveryTarget,
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
    /// Encrypted `ClipboardPayload`. Field is base64-encoded on the wire
    /// (see `b64_bytes` above) — at multi-MB blob sizes the default
    /// `Vec<u8>` JSON int-array encoding (3.57×) exceeds the receiver's
    /// 64 MB transport cap; base64 stays at 1.33× and fits comfortably.
    /// **Wire-format break**: peers running pre-§3.x versions deserialise
    /// this as a JSON int-array and reject the base64 string, surfacing
    /// as a deserialisation error on the receive side.
    Clipboard(#[serde(with = "b64_bytes")] Vec<u8>),
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
            formats: None,
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

    fn sample_payload_with_formats(formats: Vec<ClipboardFormat>) -> ClipboardPayload {
        ClipboardPayload {
            id: "fmt-id".to_string(),
            text: "Hello bold world".to_string(),
            files: None,
            blob: None,
            formats: Some(formats),
            timestamp: 1_700_000_000,
            sender: "test-host".to_string(),
            sender_id: "test-id-123".to_string(),
        }
    }

    #[test]
    fn clipboard_format_text_round_trip_json() {
        let html = ClipboardFormat::from_text(
            "text/html",
            "<p>Hello <strong>bold</strong> world</p>",
        );
        let payload = sample_payload_with_formats(vec![html.clone()]);
        let json = serde_json::to_vec(&payload).expect("serialize");
        let parsed: ClipboardPayload = serde_json::from_slice(&json).expect("deserialize");
        let formats = parsed.formats.expect("formats preserved");
        assert_eq!(formats, vec![html]);
    }

    #[test]
    fn clipboard_format_binary_round_trip_bytes() {
        let raw = vec![0x00, 0x01, 0xFF, 0xFE, 0xAB, 0xCD];
        let fmt = ClipboardFormat::from_bytes("application/octet-stream", &raw);
        assert!(fmt.binary);
        assert_eq!(fmt.raw_bytes().unwrap(), raw);
    }

    #[test]
    fn clipboard_format_text_raw_bytes_is_utf8() {
        let fmt = ClipboardFormat::from_text("text/html", "héllo");
        assert!(!fmt.binary);
        assert_eq!(fmt.raw_bytes().unwrap(), "héllo".as_bytes());
    }

    #[test]
    fn clipboard_payload_pre_formats_format_deserializes_with_no_formats() {
        // Simulates a payload from a 0.3.0-alpha peer (image-sync but no rich-text).
        // The `formats` field is missing entirely.
        let old_json = r#"{
            "id": "abc",
            "text": "hello",
            "files": null,
            "blob": null,
            "timestamp": 1234,
            "sender": "old-peer",
            "sender_id": "id-old"
        }"#;
        let parsed: ClipboardPayload = serde_json::from_str(old_json).expect("parse old");
        assert!(parsed.formats.is_none());
        assert!(parsed.blob.is_none());
        assert_eq!(parsed.text, "hello");
    }

    #[test]
    fn clipboard_format_round_trip_through_encryption() {
        let key = [0u8; 32];
        let html = ClipboardFormat::from_text(
            "text/html",
            "<p>Hello <strong>bold</strong> world</p>",
        );
        let rtf = ClipboardFormat::from_text(
            "text/rtf",
            r"{\rtf1\ansi Hello {\b bold} world}",
        );
        let payload = sample_payload_with_formats(vec![html.clone(), rtf.clone()]);
        let json = serde_json::to_vec(&payload).unwrap();
        let cipher = crate::crypto::encrypt(&key, &json).unwrap();
        let plain = crate::crypto::decrypt(&key, &cipher).unwrap();
        let recovered: ClipboardPayload = serde_json::from_slice(&plain).unwrap();
        let formats = recovered.formats.expect("formats survive crypto");
        assert_eq!(formats, vec![html, rtf]);
    }

    #[test]
    fn measure_clipboard_message_wire_inflation() {
        // Regression guard: the Message::Clipboard variant must use the
        // base64 serde adapter, not the default Vec<u8> JSON int-array,
        // or large-blob clipboard sync will silently break against the
        // 64 MB transport cap.
        let cipher: Vec<u8> = (0..10_000).map(|i| ((i * 37 + 11) % 256) as u8).collect();
        let msg = Message::Clipboard(cipher.clone());
        let json = serde_json::to_vec(&msg).unwrap();
        let ratio = json.len() as f64 / cipher.len() as f64;
        println!(
            "Message::Clipboard({} cipher bytes) -> JSON {} bytes (ratio {:.2}x)",
            cipher.len(),
            json.len(),
            ratio
        );
        // Base64 inflation is ~1.34× plus `{"Clipboard":"..."}` overhead.
        // Anything over 1.5× is a clear regression to int-array shape.
        assert!(
            ratio < 1.5,
            "Message::Clipboard wire inflation regressed to {:.2}x — base64 adapter likely missing",
            ratio
        );
    }

    #[test]
    fn clipboard_message_round_trips_through_base64_adapter() {
        let cipher: Vec<u8> = vec![0u8, 1, 2, 254, 255, 128, 64, 32];
        let msg = Message::Clipboard(cipher.clone());
        let json = serde_json::to_vec(&msg).unwrap();
        let parsed: Message = serde_json::from_slice(&json).unwrap();
        match parsed {
            Message::Clipboard(round_tripped) => {
                assert_eq!(round_tripped, cipher);
            }
            other => panic!("expected Clipboard variant, got {:?}", other),
        }
    }

    #[test]
    fn clipboard_format_binary_field_defaults_to_false() {
        // An older payload that pre-dates the `binary` flag should still parse.
        let json = r#"{"mime_type":"text/html","data":"<p>x</p>"}"#;
        let parsed: ClipboardFormat = serde_json::from_str(json).unwrap();
        assert!(!parsed.binary);
        assert_eq!(parsed.data, "<p>x</p>");
    }

    // ─── §3.3 — descriptor mode (large blobs ride the file-transfer ALPN) ───

    #[test]
    fn clipboard_blob_descriptor_round_trips_through_json() {
        let blob = ClipboardBlob::descriptor("image/png", "abc-123", 25_000_000, Some(1920), Some(1080));
        assert!(blob.is_descriptor());
        assert_eq!(blob.fetch_id.as_deref(), Some("abc-123"));
        assert_eq!(blob.total_size, Some(25_000_000));
        assert!(blob.data.is_empty());

        let json = serde_json::to_string(&blob).unwrap();
        let parsed: ClipboardBlob = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, blob);
    }

    #[test]
    fn clipboard_blob_pre_descriptor_field_format_deserializes_as_inline() {
        // A 0.3.0-alpha-2 payload without fetch_id/total_size must still
        // parse, with the new fields defaulting to None (inline mode).
        let old_json = r#"{
            "mime_type": "image/png",
            "data": "AQID",
            "width": 100,
            "height": 100
        }"#;
        let parsed: ClipboardBlob = serde_json::from_str(old_json).unwrap();
        assert!(!parsed.is_descriptor());
        assert!(parsed.fetch_id.is_none());
        assert!(parsed.total_size.is_none());
        assert_eq!(parsed.raw_bytes().unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn file_stream_header_pre_delivery_target_field_deserializes_as_disk() {
        // A pre-§3.3 header (peers up through 0.3.0-alpha-2) without
        // delivery_target must still parse and behave as Disk delivery.
        let old_json = r#"{
            "id": "abc",
            "file_index": 0,
            "file_name": "test.txt",
            "file_size": 42,
            "auth_token": "QUFB",
            "compressed": false
        }"#;
        let parsed: FileStreamHeader = serde_json::from_str(old_json).unwrap();
        assert_eq!(parsed.delivery_target, DeliveryTarget::Disk);
    }

    #[test]
    fn file_stream_header_clipboard_delivery_target_round_trips() {
        let header = FileStreamHeader {
            id: "blob-1".to_string(),
            file_index: 0,
            file_name: "blob-1.png".to_string(),
            file_size: 12_345_678,
            auth_token: "QUFB".to_string(),
            compressed: false,
            delivery_target: DeliveryTarget::Clipboard {
                mime_type: "image/png".to_string(),
                width: Some(1920),
                height: Some(1080),
            },
        };
        let json = serde_json::to_string(&header).unwrap();
        let parsed: FileStreamHeader = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.delivery_target, header.delivery_target);
    }
}
