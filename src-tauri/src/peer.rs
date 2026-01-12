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
} // timestamp for pruning old peers
