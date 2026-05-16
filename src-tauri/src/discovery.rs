use local_ip_address::local_ip;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::error::Error;

pub const SERVICE_TYPE: &str = "_clustercut._tcp.local.";

/// Protocol-compatibility version advertised in the mDNS `proto` TXT
/// property. Bumped whenever a wire-format or transport-security change
/// breaks compatibility with older peers.
///
/// - 0.3.0: strict-mTLS transport + plaintext-payload model (first break
///   from 0.2.x).
/// - 0.3.1: pairing-channel hardening per WIRE-PROTOCOL-0.3.1.md
///   (AEAD-wrapped identity in T2/T3, role-labelled SPAKE2 transcript,
///   `Welcome` deferred to QUIC/mTLS `ClusterInfo` exchange). Wire-
///   incompatible with 0.3.0; surfaced in the UI as "please upgrade".
pub const CLUSTERCUT_PROTOCOL_VERSION: &str = "0.3.1";

pub struct Discovery {
    daemon: ServiceDaemon,
    registered_service: Option<String>, // Stores fullname of registered service
}

impl Discovery {
    pub fn new() -> Result<Self, Box<dyn Error>> {
        let daemon = ServiceDaemon::new()?;
        Ok(Self {
            daemon,
            registered_service: None,
        })
    }

    pub fn register(
        &mut self,
        device_id: &str,
        network_name: &str,
        port: u16,
    ) -> Result<(), Box<dyn Error>> {
        // If already registered, unregister first
        if let Some(fullname) = &self.registered_service {
            tracing::info!("Unregistering old service: {}", fullname);
            let _ = self.daemon.unregister(fullname);
            // Short pause to ensure unregistration propagates locally if needed
            // std::thread::sleep(std::time::Duration::from_millis(100));
        }

        // Get the local IP address
        let ip = local_ip()?;

        // Hostname usually needs to be unique on the network, but we'll base it on device ID for now.
        // Format: device_id.local.
        let m_hostname = format!("{}.local.", device_id);

        // Get actual system hostname for UI display
        let system_hostname = hostname::get()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|_| "Unknown Device".to_string());

        // Properties can be used to send public key fingerprint or other metadata.
        // `proto` carries the wire-protocol-compatibility version (see
        // [`CLUSTERCUT_PROTOCOL_VERSION`]). Peers below the receiver's expected
        // value get a yellow warning indicator in the UI.
        let properties = [
            ("version", "0.1.0"),
            ("proto", CLUSTERCUT_PROTOCOL_VERSION),
            ("id", device_id),
            ("n", network_name),     // n = network name
            ("h", &system_hostname), // h = visible hostname
        ];

        let service_info = ServiceInfo::new(
            SERVICE_TYPE,
            device_id,
            &m_hostname,
            &ip.to_string(),
            port,
            &properties[..],
        )?;

        // Store fullname for unregistering later
        let fullname = service_info.get_fullname().to_string();

        self.daemon.register(service_info)?;
        tracing::info!(
            "Registered service: {} ({}) on {}:{}",
            device_id,
            fullname,
            ip,
            port
        );

        self.registered_service = Some(fullname);

        Ok(())
    }

    pub fn browse(&self) -> Result<mdns_sd::Receiver<ServiceEvent>, Box<dyn Error>> {
        let receiver = self.daemon.browse(SERVICE_TYPE)?;
        Ok(receiver)
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        if let Some(fullname) = &self.registered_service {
            tracing::info!("Unregistering service: {}", fullname);
            if let Err(e) = self.daemon.unregister(fullname) {
                tracing::error!("Failed to unregister service: {}", e);
            }
            // Give the daemon time to send the goodbye packet before we drop it (and likely kill its background thread)
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    }
}
