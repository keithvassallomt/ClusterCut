#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Peer {
    pub id: String,
    pub ip: std::net::IpAddr,
    pub port: u16,
    pub hostname: String,
    pub last_seen: u64,
    pub is_trusted: bool,
    // Discovery method
    #[serde(default)]
    pub is_manual: bool,
    // Network Name (discovered via mDNS)
    #[serde(default)]
    pub network_name: Option<String>,
    /// Legacy field from the pre-mTLS cluster-key signature scheme.
    /// Always None on v0.3+; kept as `#[serde(default)]` so on-disk
    /// `known_peers.json` from older versions deserialises cleanly.
    #[serde(default, skip_serializing)]
    pub signature: Option<String>,
    // SHA-256 of the peer's TLS cert DER. Set during pairing. Peers
    // paired before mTLS landed have None here and need to re-pair
    // (Phase 4 surfaces this in the UI).
    #[serde(default)]
    pub fingerprint: Option<Vec<u8>>,
    /// Protocol-compatibility version advertised by the peer in mDNS
    /// (TXT property `proto`). Used to flag peers running pre-mTLS
    /// builds that can't talk to this device. None when the property
    /// is absent (older builds didn't set it). May get persisted to
    /// `known_peers.json` as a side effect of save calls — that's
    /// harmless since the field is refreshed from mDNS at every
    /// resolution.
    #[serde(default)]
    pub protocol_version: Option<String>,
}

/// Frontend-only view of a peer. Carries all `Peer` fields plus a
/// `compatible` flag computed locally by Rust — never sent peer-to-peer.
/// Emitted via `peer-update` events and returned by `get_peers` so the
/// TypeScript UI can read `peer.compatible` directly without re-implementing
/// the version-comparison logic.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PeerView {
    pub id: String,
    pub ip: std::net::IpAddr,
    pub port: u16,
    pub hostname: String,
    pub last_seen: u64,
    pub is_trusted: bool,
    pub is_manual: bool,
    pub network_name: Option<String>,
    pub fingerprint: Option<Vec<u8>>,
    pub protocol_version: Option<String>,
    /// True when the peer's advertised `protocol_version` is >= the minimum
    /// this build requires. Computed by `net_util::is_protocol_compatible`;
    /// never travels over the peer-to-peer wire.
    pub compatible: bool,
}

impl PeerView {
    pub fn from_peer(peer: &Peer) -> Self {
        Self {
            id: peer.id.clone(),
            ip: peer.ip,
            port: peer.port,
            hostname: peer.hostname.clone(),
            last_seen: peer.last_seen,
            is_trusted: peer.is_trusted,
            is_manual: peer.is_manual,
            network_name: peer.network_name.clone(),
            fingerprint: peer.fingerprint.clone(),
            protocol_version: peer.protocol_version.clone(),
            compatible: crate::net_util::is_protocol_compatible(peer.protocol_version.as_deref()),
        }
    }
}
