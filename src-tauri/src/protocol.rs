use base64::Engine as _;
use serde::{Deserialize, Serialize};

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

/// Wire-protocol 0.3.1: the inner struct of an AEAD-wrapped pairing frame
/// (T2 ResponderId, T3 InitiatorId). Carries the identity bytes that a peer
/// commits to during pairing. Decryption of the surrounding ciphertext under
/// the role-distinct sub-key is the *only* thing that authenticates these
/// fields — there is no separate confirm tag, and the bytes are not trusted
/// until the AEAD tag verifies.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PairIdInner {
    /// Caller-supplied stable device identifier. Capped at
    /// `MAX_DEVICE_ID_BYTES` on serialise (deterministic UTF-8-safe truncation)
    /// so both ends derive the same identifier from the same source string
    /// without burdening framing with variable-length growth.
    pub device_id: String,
    /// SHA-256 of the peer's TLS cert DER. The receiver pins this as the
    /// expected fingerprint for the peer's `device_id` for all future QUIC
    /// mTLS handshakes.
    pub fingerprint: Vec<u8>,
}

/// Hard cap on `device_id` length, enforced via deterministic UTF-8-safe
/// truncation at both ends of the pairing channel. Picked to be comfortably
/// larger than any device_id we generate today (UUIDs are 36 bytes) while
/// keeping the maximum pairing-frame size a small, fixed constant.
pub const MAX_DEVICE_ID_BYTES: usize = 256;

/// Deterministically truncate a `device_id` to at most `MAX_DEVICE_ID_BYTES`
/// bytes without splitting a UTF-8 codepoint. Both initiator and responder
/// run identifiers through this before serialising, so a long source string
/// produces the same truncated form on both sides.
pub fn truncate_device_id(s: &str) -> String {
    if s.len() <= MAX_DEVICE_ID_BYTES {
        return s.to_string();
    }
    let mut end = MAX_DEVICE_ID_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Cluster bootstrap payload delivered post-pairing over QUIC/mTLS (T7).
/// In 0.3.0 this lived inside the pairing-channel `Welcome` frame; 0.3.1
/// moves it onto the already-authenticated QUIC channel so the pairing
/// channel does one job and one job only — pin cert fingerprints.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ClusterInfo {
    /// Stable cluster identifier (UUID). Non-secret handle for grouping in
    /// the UI and gossip-loop suppression.
    pub cluster_id: String,
    pub known_peers: Vec<crate::peer::Peer>,
    pub network_name: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum Message {
    /// `ClipboardPayload` carried directly. Pre-v0.3 the field was
    /// app-layer-encrypted bytes wrapped here; v0.3+ relies on QUIC mTLS
    /// for confidentiality and authenticity, so the payload travels as
    /// a typed struct. Image bytes inside `ClipboardBlob.data` are still
    /// base64-encoded so they don't bloat to a JSON int-array.
    Clipboard(ClipboardPayload),
    // Gossip: Broadcast new peer to known peers
    PeerDiscovery(crate::peer::Peer),
    // Broadcast removal of a peer (kick/leave)
    PeerRemoval(String), // Payload is device_id
    // Broadcast deletion of history item
    HistoryDelete(String), // Payload is item ID
    // File request (typed; pre-v0.3 was an encrypted byte-string wrapper)
    FileRequest(FileRequestPayload),
    // Liveness Check
    Ping,
    Pong,
    /// 0.3.1: initiator → responder over QUIC/mTLS immediately after pairing
    /// completes, asking for the cluster bootstrap state that 0.3.0 used to
    /// ship inside the pairing-channel `Welcome`.
    ClusterInfoRequest,
    /// 0.3.1: responder → initiator over QUIC/mTLS in reply to
    /// `ClusterInfoRequest`. mTLS authenticates both ends, so no extra
    /// per-payload tag is needed.
    ClusterInfo(ClusterInfo),
}

/// Messages exchanged on the dedicated plaintext-TCP pairing channel.
/// Pairing never travels over QUIC: SPAKE2 needs no transport-layer
/// confidentiality (it's a PAKE), and wrapping it in unauthenticated TLS
/// adds complexity without security. Once pairing completes, all further
/// traffic moves to mutually-authenticated QUIC (steady-state `Message`).
///
/// Wire-protocol 0.3.3 (this file is the sole spec):
///
/// ```text
/// T0  Initiator → Responder   PairRequest  { spake_msg }
/// T1  Responder → Initiator   PairResponse { spake_msg }
/// T2  Initiator → Responder   InitiatorKC  { nonce, ciphertext = AEAD(k_i2r, nonce, INITIATOR_KC_PLAINTEXT) }
/// T3  Responder → Initiator   ResponderId  { nonce, ciphertext = AEAD(k_r2i, nonce, inner) }
/// T4  Initiator → Responder   InitiatorId  { nonce, ciphertext = AEAD(k_i2r, nonce, inner) }
/// ```
///
/// 0.3.3 diff from 0.3.1: T2 `InitiatorKC` is new. It forces the initiator
/// to prove possession of the SPAKE2-derived `k_i2r` (i.e. correct PIN)
/// *before* the responder sends its AEAD-encrypted identity. This closes
/// the online brute-force budget-bypass where a 0.3.1 attacker could
/// disconnect after T2 and brute-force the captured ResponderId offline
/// without ever triggering an AEAD-failure counter increment.
///
/// No plaintext identity fields on the pairing channel: `device_id` and
/// fingerprint appear only inside the AEAD-protected `inner` (see
/// [`PairIdInner`]). A wrong-PIN MITM derives a different `k_{i2r,r2i}`
/// and AEAD decryption fails closed before either side trusts a byte of
/// the payload. The cluster bootstrap state (`cluster_id`, `known_peers`,
/// `network_name`) is deferred to a post-pairing exchange over QUIC/mTLS
/// — see [`Message::ClusterInfoRequest`] / [`Message::ClusterInfo`].
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum PairingMessage {
    /// T0 — opening SPAKE2 element from the initiator. No identity bytes.
    PairRequest { spake_msg: Vec<u8> },
    /// T1 — answering SPAKE2 element from the responder. No identity bytes.
    PairResponse { spake_msg: Vec<u8> },
    /// T2 (wire 0.3.3) — initiator's AEAD-wrapped key-confirmation frame.
    /// Plaintext is the fixed `INITIATOR_KC_PLAINTEXT` constant; the tag
    /// authenticates the (k_i2r, nonce, plaintext) triple. Responder bumps
    /// the H1 AEAD-failure counter on tag failure.
    InitiatorKC { nonce: Vec<u8>, ciphertext: Vec<u8> },
    /// T3 — responder's AEAD-wrapped identity, decryptable under `k_r2i`.
    ResponderId { nonce: Vec<u8>, ciphertext: Vec<u8> },
    /// T4 — initiator's AEAD-wrapped identity, decryptable under `k_i2r`.
    InitiatorId { nonce: Vec<u8>, ciphertext: Vec<u8> },
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
    fn message_clipboard_round_trips_through_json() {
        let blob = sample_blob();
        let payload = sample_payload(Some(blob.clone()));
        let msg = Message::Clipboard(payload);
        let json = serde_json::to_vec(&msg).expect("serialize");
        let parsed: Message = serde_json::from_slice(&json).expect("deserialize");
        match parsed {
            Message::Clipboard(p) => {
                assert_eq!(p.blob.expect("blob preserved"), blob);
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

    // ─── §0.3.1 — device_id truncation and pairing inner round-trip ───

    #[test]
    fn truncate_device_id_keeps_short_ids_unchanged() {
        let id = "550e8400-e29b-41d4-a716-446655440000"; // 36-byte UUID
        assert_eq!(truncate_device_id(id), id);
    }

    #[test]
    fn truncate_device_id_caps_at_max_bytes() {
        let long = "a".repeat(MAX_DEVICE_ID_BYTES * 4);
        let out = truncate_device_id(&long);
        assert!(out.len() <= MAX_DEVICE_ID_BYTES);
        assert_eq!(out.len(), MAX_DEVICE_ID_BYTES);
    }

    #[test]
    fn truncate_device_id_respects_utf8_boundaries() {
        // 4-byte UTF-8 codepoints; ensure we never split mid-codepoint.
        let unit = "𠮷"; // 4 bytes in UTF-8
        let s = unit.repeat(MAX_DEVICE_ID_BYTES); // way past the cap
        let out = truncate_device_id(&s);
        assert!(out.len() <= MAX_DEVICE_ID_BYTES);
        // Must still be valid UTF-8 — round-tripping through a String is the
        // de-facto check (String would panic on invalid UTF-8 construction).
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn pairing_message_variants_have_no_cluster_state() {
        // Wire-protocol 0.3.1 invariant: cluster bootstrap state
        // (`cluster_id`, `known_peers`, `network_name`) MUST NOT travel on
        // the pairing channel — it now rides post-pairing QUIC/mTLS as
        // `Message::ClusterInfo`. Asserting via serialised representations
        // here so an accidental future re-introduction of a Welcome-style
        // variant is caught by this test.
        let t0 = PairingMessage::PairRequest { spake_msg: vec![1, 2, 3] };
        let t1 = PairingMessage::PairResponse { spake_msg: vec![4, 5, 6] };
        let t2 = PairingMessage::ResponderId { nonce: vec![0; 12], ciphertext: vec![7, 8, 9] };
        let t3 = PairingMessage::InitiatorId { nonce: vec![0; 12], ciphertext: vec![10, 11, 12] };
        for msg in [t0, t1, t2, t3] {
            let s = serde_json::to_string(&msg).unwrap();
            for forbidden in ["cluster_id", "known_peers", "network_name", "Welcome", "PairFingerprint"] {
                assert!(
                    !s.contains(forbidden),
                    "PairingMessage serialisation must not contain {:?}; got {}",
                    forbidden, s
                );
            }
        }
    }

    #[test]
    fn cluster_info_lives_on_the_message_enum_only() {
        // Companion check to the above: ClusterInfo MUST be a `Message`
        // variant (so it rides QUIC/mTLS) and not reachable via
        // `PairingMessage`. Rejecting a hand-crafted "Welcome"-tagged
        // PairingMessage proves the variant is gone.
        let info = ClusterInfo {
            cluster_id: "c".to_string(),
            known_peers: vec![],
            network_name: "n".to_string(),
        };
        let wrapped = Message::ClusterInfo(info);
        let s = serde_json::to_string(&wrapped).unwrap();
        let _: Message = serde_json::from_str(&s).expect("ClusterInfo round-trips via Message");

        // A legacy 0.3.0 "Welcome" tag in PairingMessage must no longer parse.
        let legacy = r#"{"Welcome":{"cluster_id":"x","known_peers":[],"network_name":"y","responder_fingerprint":[],"confirm":[]}}"#;
        let parsed: Result<PairingMessage, _> = serde_json::from_str(legacy);
        assert!(parsed.is_err(), "Welcome variant must no longer deserialise on the pairing channel");
    }

    #[test]
    fn pair_id_inner_round_trips_through_json() {
        let inner = PairIdInner {
            device_id: "device-a".to_string(),
            fingerprint: vec![0xAA, 0xBB, 0xCC, 0xDD],
        };
        let bytes = serde_json::to_vec(&inner).unwrap();
        let parsed: PairIdInner = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed, inner);
    }

    #[test]
    fn file_stream_header_clipboard_delivery_target_round_trips() {
        let header = FileStreamHeader {
            id: "blob-1".to_string(),
            file_index: 0,
            file_name: "blob-1.png".to_string(),
            file_size: 12_345_678,
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
