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
    // Cluster Authentication Signature (Base64)
    #[serde(default)]
    pub signature: Option<String>,
    // SHA-256 of the peer's TLS cert DER. Set during pairing or learned via
    // gossip from a trusted peer; absent for peers paired before cert-pinning
    // landed (those connections fall back to skip-verify until re-pair).
    #[serde(default)]
    pub fingerprint: Option<Vec<u8>>,
}
