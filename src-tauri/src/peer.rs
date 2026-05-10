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
    /// is absent (older builds didn't set it). Not persisted to
    /// `known_peers.json` — refreshed from mDNS each time.
    #[serde(default, skip_serializing)]
    pub protocol_version: Option<String>,
}
