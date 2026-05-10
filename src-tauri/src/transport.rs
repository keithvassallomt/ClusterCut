use quinn::{ClientConfig, Endpoint, ServerConfig};
use rcgen::generate_simple_self_signed;
use sha2::{Digest, Sha256};
use std::error::Error;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub type FingerprintResolver = Arc<dyn Fn(SocketAddr) -> Option<Vec<u8>> + Send + Sync>;

#[derive(Clone)]
pub struct Transport {
    pub endpoint: Endpoint,
    no_fp_transport_config: ClientConfig,
    no_fp_file_config: ClientConfig,
    local_cert_der: Vec<u8>,
    fingerprint_resolver: Arc<Mutex<Option<FingerprintResolver>>>,
}

impl Transport {
    /// Build a Transport from a pre-existing self-signed cert. Persisting and loading
    /// the cert is the caller's responsibility — `Transport` doesn't touch disk.
    pub fn new(port: u16, cert_der: Vec<u8>, key_der: Vec<u8>) -> Result<Self, Box<dyn Error>> {
        let server_config = configure_server(cert_der.clone(), key_der)?;

        // Fallback configs used only for legacy peers paired before cert
        // pinning landed (their `known_peers.json` entry has no fingerprint).
        // Phase 2 of the security redesign removes this branch entirely —
        // peers without a pinned fingerprint will be unreachable and must
        // re-pair. Pairing itself never goes through QUIC anymore (it runs
        // on a dedicated plaintext-TCP channel; see `start_pairing_listener`).
        let no_fp_transport_config = configure_client(vec![b"clustercut-transport".to_vec()], None)?;
        let no_fp_file_config = configure_client(vec![b"clustercut-file".to_vec()], None)?;

        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let mut endpoint = Endpoint::server(server_config, addr)?;
        endpoint.set_default_client_config(no_fp_transport_config.clone());

        Ok(Self {
            endpoint,
            no_fp_transport_config,
            no_fp_file_config,
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
            None => Ok(self.no_fp_transport_config.clone()),
        }
    }

    fn file_config_for(&self, addr: SocketAddr) -> Result<ClientConfig, Box<dyn Error + Send + Sync>> {
        match self.resolve_fingerprint(addr) {
            Some(fp) => configure_client(vec![b"clustercut-file".to_vec()], Some(fp))
                .map_err(|e| -> Box<dyn Error + Send + Sync> { e.to_string().into() }),
            None => Ok(self.no_fp_file_config.clone()),
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

    async fn send_message_with_config(
        &self,
        addr: SocketAddr,
        data: &[u8],
        config: ClientConfig,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let len = data.len();
        let large = len > 256 * 1024;
        if large {
            tracing::info!("send_message: connecting to {} for {} byte payload", addr, len);
        }

        let connection = self
            .endpoint
            .connect_with(config, addr, "clustercut")?
            .await?;
        if large {
            tracing::info!("send_message: connection established to {}", addr);
        }

        let (mut send, _recv) = connection.open_bi().await?;
        if large {
            tracing::info!("send_message: bi stream opened to {}", addr);
        }

        send.write_all(data).await?;
        if large {
            tracing::info!("send_message: write_all done ({} bytes) to {}", len, addr);
        }

        send.finish()?;
        if large {
            tracing::info!("send_message: finish() called on stream to {}", addr);
        }

        // Wait until the peer has acknowledged all stream data before letting
        // the connection drop. The previous fixed 500 ms sleep was fine for
        // text payloads but raced multi-MB clipboard images — the function
        // returned and the connection torn down while bytes were still in
        // flight, surfacing as "connection lost" on the receiver mid-read.
        // 30 s upper bound covers slow links without hanging forever if the
        // peer dies. Fall back to a data-size-proportional sleep if stopped()
        // doesn't yield a clean ACK, so a quirky stopped() implementation
        // can't strand the data either.
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            send.stopped(),
        )
        .await
        {
            Ok(Ok(None)) => {
                if large {
                    tracing::info!("send_message: peer ACKed all data ({} bytes) to {}", len, addr);
                }
            }
            other => {
                let fallback_ms = ((len as u64 / 1024).max(500)).min(15_000);
                tracing::warn!(
                    "send_message: stopped() didn't yield clean ACK to {} ({:?}); falling back to {} ms drain wait",
                    addr,
                    other,
                    fallback_ms
                );
                tokio::time::sleep(std::time::Duration::from_millis(fallback_ms)).await;
            }
        }

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
                                            // Cap each message at 64 MB. Sized to fit a 10 MB raw
                                            // clipboard image after the wire-format expansion:
                                            // base64 (1.33×) inside ClipboardPayload JSON, then the
                                            // encrypted ciphertext re-wrapped in
                                            // Message::Clipboard(Vec<u8>) which serde_json emits
                                            // as an integer array (~3.5×). Net ~50 MB worst case.
                                            const MESSAGE_BYTE_CAP: usize = 1024 * 1024 * 64;
                                            match recv.read_to_end(MESSAGE_BYTE_CAP).await {
                                                Ok(buf) => {
                                                    if !buf.is_empty() {
                                                        on_receive_message(buf, remote_addr);
                                                    }
                                                }
                                                Err(quinn::ReadToEndError::TooLong) => {
                                                    tracing::error!(
                                                        "Stream from {} exceeded {} byte cap; dropping. Likely a clipboard image larger than the supported wire size.",
                                                        remote_addr,
                                                        MESSAGE_BYTE_CAP
                                                    );
                                                }
                                                Err(e) => {
                                                    tracing::error!(
                                                        "Failed to read from stream from {}: {}",
                                                        remote_addr,
                                                        e
                                                    );
                                                }
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

// ────────────────────────────────────────────────────────────────────────────
// Plaintext-TCP pairing channel
//
// Pairing runs over TCP on the same numeric port as the QUIC endpoint (UDP
// and TCP cohabit on a single port number). SPAKE2 doesn't need transport
// confidentiality — wrapping it in unauthenticated TLS adds complexity
// without security. Once both sides exchange cert fingerprints, all further
// traffic moves to the QUIC endpoint with cert pinning enforced.
//
// Wire framing: 4-byte big-endian length prefix followed by JSON-serialised
// `PairingMessage`. The cap is generous enough for a Welcome carrying a
// hundred peers but bounded so a misbehaving peer can't OOM us.
// ────────────────────────────────────────────────────────────────────────────

/// Maximum size of one pairing frame on the wire. Welcome is the largest
/// payload (cluster identity + known_peers + fingerprint), and even a
/// pathological cluster sits well under this bound.
pub const PAIRING_FRAME_CAP: usize = 256 * 1024;

pub async fn read_pairing_frame(
    stream: &mut TcpStream,
) -> Result<crate::protocol::PairingMessage, Box<dyn Error + Send + Sync>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > PAIRING_FRAME_CAP {
        return Err(format!("pairing frame too large: {} > {}", len, PAIRING_FRAME_CAP).into());
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    let msg = serde_json::from_slice::<crate::protocol::PairingMessage>(&buf)?;
    Ok(msg)
}

pub async fn write_pairing_frame(
    stream: &mut TcpStream,
    msg: &crate::protocol::PairingMessage,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let body = serde_json::to_vec(msg)?;
    if body.len() > PAIRING_FRAME_CAP {
        return Err(format!("pairing frame too large: {} > {}", body.len(), PAIRING_FRAME_CAP).into());
    }
    let len = (body.len() as u32).to_be_bytes();
    stream.write_all(&len).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    Ok(())
}

/// Open an outbound pairing connection. Caller drives the request/response
/// flow on the returned stream and closes when done.
pub async fn pairing_connect(
    addr: SocketAddr,
) -> Result<TcpStream, Box<dyn Error + Send + Sync>> {
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        TcpStream::connect(addr),
    )
    .await
    .map_err(|_| "pairing connect timeout")??;
    Ok(stream)
}

/// Bind a TCP pairing listener on `[::]:port` and spawn an accept loop that
/// hands each accepted connection to `handler`. The listener runs for the
/// process lifetime — there is no shutdown signal in Phase 1.
pub fn start_pairing_listener<F>(
    port: u16,
    handler: F,
) -> Result<(), Box<dyn Error + Send + Sync>>
where
    F: Fn(TcpStream, SocketAddr) + Send + Sync + Clone + 'static,
{
    let bind_addr = SocketAddr::from(([0, 0, 0, 0], port));
    let std_listener = std::net::TcpListener::bind(bind_addr)?;
    std_listener.set_nonblocking(true)?;
    let listener = TcpListener::from_std(std_listener)?;
    tracing::info!("Pairing TCP listener bound on {}", bind_addr);

    tauri::async_runtime::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer_addr)) => {
                    tracing::debug!("Pairing TCP accept from {}", peer_addr);
                    handler.clone()(stream, peer_addr);
                }
                Err(e) => {
                    tracing::warn!("Pairing TCP accept error: {}", e);
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    });
    Ok(())
}
