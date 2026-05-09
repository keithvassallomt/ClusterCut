use quinn::{ClientConfig, Endpoint, ServerConfig};
use rcgen::generate_simple_self_signed;
use sha2::{Digest, Sha256};
use std::error::Error;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

pub type FingerprintResolver = Arc<dyn Fn(SocketAddr) -> Option<Vec<u8>> + Send + Sync>;

#[derive(Clone)]
pub struct Transport {
    pub endpoint: Endpoint,
    skip_verify_transport_config: ClientConfig,
    skip_verify_file_config: ClientConfig,
    local_cert_der: Vec<u8>,
    fingerprint_resolver: Arc<Mutex<Option<FingerprintResolver>>>,
}

impl Transport {
    /// Build a Transport from a pre-existing self-signed cert. Persisting and loading
    /// the cert is the caller's responsibility — `Transport` doesn't touch disk.
    pub fn new(port: u16, cert_der: Vec<u8>, key_der: Vec<u8>) -> Result<Self, Box<dyn Error>> {
        let server_config = configure_server(cert_der.clone(), key_der)?;

        // These are used (a) before a resolver is installed and (b) for the
        // pairing handshake itself — connections where the peer's fingerprint
        // is genuinely unknown. Once a resolver is installed, normal traffic
        // pins to the looked-up fingerprint.
        let skip_verify_transport_config = configure_client(vec![b"clustercut-transport".to_vec()], None)?;
        let skip_verify_file_config = configure_client(vec![b"clustercut-file".to_vec()], None)?;

        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let mut endpoint = Endpoint::server(server_config, addr)?;
        endpoint.set_default_client_config(skip_verify_transport_config.clone());

        Ok(Self {
            endpoint,
            skip_verify_transport_config,
            skip_verify_file_config,
            local_cert_der: cert_der,
            fingerprint_resolver: Arc::new(Mutex::new(None)),
        })
    }

    /// Install (or replace) the resolver that maps a peer address to its pinned
    /// SHA-256 fingerprint. Call this once known_peers is loaded; until then
    /// every send falls back to skip-verify.
    pub fn set_fingerprint_resolver(&self, resolver: FingerprintResolver) {
        *self.fingerprint_resolver.lock().unwrap() = Some(resolver);
    }

    /// SHA-256 of the local cert DER, used as the device's public TLS identity.
    pub fn local_fingerprint(&self) -> Vec<u8> {
        cert_fingerprint(&self.local_cert_der)
    }

    fn resolve_fingerprint(&self, addr: SocketAddr) -> Option<Vec<u8>> {
        self.fingerprint_resolver
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|r| r(addr))
    }

    fn transport_config_for(&self, addr: SocketAddr) -> Result<ClientConfig, Box<dyn Error + Send + Sync>> {
        match self.resolve_fingerprint(addr) {
            Some(fp) => configure_client(vec![b"clustercut-transport".to_vec()], Some(fp))
                .map_err(|e| -> Box<dyn Error + Send + Sync> { e.to_string().into() }),
            None => Ok(self.skip_verify_transport_config.clone()),
        }
    }

    fn file_config_for(&self, addr: SocketAddr) -> Result<ClientConfig, Box<dyn Error + Send + Sync>> {
        match self.resolve_fingerprint(addr) {
            Some(fp) => configure_client(vec![b"clustercut-file".to_vec()], Some(fp))
                .map_err(|e| -> Box<dyn Error + Send + Sync> { e.to_string().into() }),
            None => Ok(self.skip_verify_file_config.clone()),
        }
    }

    pub async fn send_message(
        &self,
        addr: SocketAddr,
        data: &[u8],
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let config = self.transport_config_for(addr)?;
        self.send_message_with_config(addr, data, config).await
    }

    /// Pairing-flow send: bypasses fingerprint pinning because we don't yet
    /// know the peer's fingerprint. The SPAKE2 + PIN handshake authenticates
    /// the session at the application layer; cert pinning takes over for all
    /// subsequent connections (see issue #9).
    pub async fn send_message_unauthenticated(
        &self,
        addr: SocketAddr,
        data: &[u8],
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        self.send_message_with_config(addr, data, self.skip_verify_transport_config.clone())
            .await
    }

    async fn send_message_with_config(
        &self,
        addr: SocketAddr,
        data: &[u8],
        config: ClientConfig,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let connection = self
            .endpoint
            .connect_with(config, addr, "clustercut")?
            .await?;
        let (mut send, _recv) = connection.open_bi().await?;

        send.write_all(data).await?;
        send.finish()?;

        // Give the stream a moment to flush/be accepted before dropping the connection
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        Ok(())
    }

    /// Open a dedicated file stream connection to start sending a file
    /// Returns the SendStream so the caller can pump data into it.
    pub async fn send_file_stream(
        &self,
        addr: SocketAddr,
    ) -> Result<(quinn::Connection, quinn::SendStream), Box<dyn Error + Send + Sync>> {
        let config = self.file_config_for(addr)?;
        let connection = self
            .endpoint
            .connect_with(config, addr, "clustercut")?
            .await?;
        // Use Uni stream for file transfer (Sender -> Receiver)
        let send = connection.open_uni().await?;
        Ok((connection, send))
    }

    pub fn start_listening<F, G>(&self, on_receive_message: F, on_receive_file: G)
    where
        F: Fn(Vec<u8>, SocketAddr) + Send + Sync + 'static + Clone,
        G: Fn(quinn::RecvStream, SocketAddr) + Send + Sync + 'static + Clone,
    {
        let endpoint = self.endpoint.clone();
        tauri::async_runtime::spawn(async move {
            tracing::info!("Starting transport listener loop...");
            while let Some(conn) = endpoint.accept().await {
                // tracing::debug!("Transport accepted a connection attempt...");
                let connection = conn.await;
                match connection {
                    Ok(conn) => {
                        let remote_addr = conn.remote_address();
                        // tracing::info!("Transport established connection with {}", remote_addr);

                        // Check Protocol (ALPN)
                        let protocol = conn
                            .handshake_data()
                            .unwrap()
                            .downcast::<quinn::crypto::rustls::HandshakeData>()
                            .unwrap()
                            .protocol
                            .map(|p| String::from_utf8_lossy(&p).to_string());

                        // Default to transport if unknown
                        let proto = protocol.unwrap_or_else(|| "clustercut-transport".to_string());

                        tracing::debug!("Connection from {} using ALPN: {}", remote_addr, proto);

                        if proto == "clustercut-file" {
                            // File Stream Handler
                            let on_receive_file = on_receive_file.clone();
                            tauri::async_runtime::spawn(async move {
                                tracing::debug!("Handling FILE connection from {}", remote_addr);
                                loop {
                                    // Accept Uni streams for files
                                    match conn.accept_uni().await {
                                        Ok(recv) => {
                                            tracing::info!(
                                                "Accepted FILE stream from {}",
                                                remote_addr
                                            );
                                            on_receive_file(recv, remote_addr);
                                        }
                                        Err(e) => {
                                            tracing::debug!(
                                                "File connection closed/error from {}: {}",
                                                remote_addr,
                                                e
                                            );
                                            break;
                                        }
                                    }
                                }
                            });
                        } else {
                            // Standard Message Handler (clustercut-transport)
                            let on_receive_message = on_receive_message.clone();
                            tauri::async_runtime::spawn(async move {
                                // tracing::debug!("Handling MESSAGE connection from {}", remote_addr);
                                loop {
                                    match conn.accept_bi().await {
                                        Ok((_, mut recv)) => {
                                            // tracing::debug!("Accepted message stream from {}", remote_addr);
                                            // Limit 10MB
                                            if let Ok(buf) =
                                                recv.read_to_end(1024 * 1024 * 10).await
                                            {
                                                if !buf.is_empty() {
                                                    on_receive_message(buf, remote_addr);
                                                }
                                            } else {
                                                tracing::error!(
                                                    "Failed to read from stream from {}",
                                                    remote_addr
                                                );
                                            }
                                        }
                                        Err(_e) => {
                                            // connection closed is normal
                                            // tracing::debug!("Message connection closed/error from {}: {}", remote_addr, e);
                                            break;
                                        }
                                    }
                                }
                            });
                        }
                    }
                    Err(e) => tracing::error!("Connection handshake failed: {}", e),
                }
            }
        });
    }

    pub fn local_addr(&self) -> Result<SocketAddr, Box<dyn Error>> {
        Ok(self.endpoint.local_addr()?)
    }
}

pub fn generate_self_signed_cert() -> Result<(Vec<u8>, Vec<u8>), Box<dyn Error>> {
    // Register BOTH protocols
    let cert = generate_simple_self_signed(vec![
        "clustercut-transport".into(),
        "clustercut-file".into(),
        "clustercut".into(), // Add generic SNI validity
    ])?;
    Ok((cert.cert.der().to_vec(), cert.signing_key.serialize_der()))
}

pub fn cert_fingerprint(cert_der: &[u8]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(cert_der);
    hasher.finalize().to_vec()
}

fn configure_server(cert_der: Vec<u8>, key_der: Vec<u8>) -> Result<ServerConfig, Box<dyn Error>> {
    let cert = rustls::pki_types::CertificateDer::from(cert_der);
    let key =
        rustls::pki_types::PrivateKeyDer::try_from(key_der).map_err(|_| "Invalid private key")?;

    let mut crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;

    crypto.alpn_protocols = vec![
        b"clustercut-transport".to_vec(),
        b"clustercut-file".to_vec(),
    ];

    let mut server_config = ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(crypto)?,
    ));

    // Increase Transport Limits for Server Config
    let mut transport_config = quinn::TransportConfig::default();
    // 100MB Stream Window (allows 100MB in flight for a single stream)
    transport_config.stream_receive_window(quinn::VarInt::from_u32(100 * 1024 * 1024));
    // 250MB Connection Window (allows multiple streams to sum up to this)
    transport_config.receive_window(quinn::VarInt::from_u32(250 * 1024 * 1024));

    // Keep Alive
    transport_config.keep_alive_interval(Some(std::time::Duration::from_secs(5)));
    transport_config.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(std::time::Duration::from_secs(30)).unwrap(),
    ));

    server_config.transport_config(Arc::new(transport_config));

    Ok(server_config)
}

fn configure_client(
    alpn_protocols: Vec<Vec<u8>>,
    pinned_fingerprint: Option<Vec<u8>>,
) -> Result<ClientConfig, Box<dyn Error>> {
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};

    /// Verifier that pins the SHA-256 of the server's cert DER if a fingerprint
    /// is configured. With `expected = None` it accepts any cert (pairing path
    /// before fingerprint exchange, and legacy peers without a stored
    /// fingerprint — see issue #9).
    #[derive(Debug)]
    struct FingerprintVerifier {
        expected: Option<Vec<u8>>,
    }
    impl ServerCertVerifier for FingerprintVerifier {
        fn verify_server_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            if let Some(expected) = &self.expected {
                let actual = cert_fingerprint(end_entity.as_ref());
                if actual.as_slice() != expected.as_slice() {
                    return Err(rustls::Error::General(format!(
                        "cert fingerprint mismatch: expected {}, got {}",
                        hex_short(expected),
                        hex_short(&actual)
                    )));
                }
            }
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            Ok(HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            vec![
                SignatureScheme::RSA_PSS_SHA256,
                SignatureScheme::RSA_PSS_SHA384,
                SignatureScheme::RSA_PSS_SHA512,
                SignatureScheme::ED25519,
                SignatureScheme::ECDSA_NISTP256_SHA256,
                SignatureScheme::ECDSA_NISTP384_SHA384,
            ]
        }
    }

    let mut client_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(FingerprintVerifier {
            expected: pinned_fingerprint,
        }))
        .with_no_client_auth();

    // Set ALPN protocols on the underlying rustls config
    client_config.alpn_protocols = alpn_protocols;

    // Client ALPN will be set per-connection ("connect(..., alpn)") so we don't need default here,
    // but we can add them to supported.
    let mut quic_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_config)?,
    ));

    // Increase Transport Limits for Client Config
    let mut transport_config = quinn::TransportConfig::default();
    transport_config.stream_receive_window(quinn::VarInt::from_u32(100 * 1024 * 1024));
    transport_config.receive_window(quinn::VarInt::from_u32(250 * 1024 * 1024));
    transport_config.keep_alive_interval(Some(std::time::Duration::from_secs(5)));
    transport_config.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(std::time::Duration::from_secs(30)).unwrap(),
    ));

    quic_config.transport_config(Arc::new(transport_config));

    Ok(quic_config)
}

fn hex_short(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take(8)
        .map(|b| format!("{:02x}", b))
        .collect::<String>()
}
