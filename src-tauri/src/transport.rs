use quinn::{ClientConfig, Endpoint, ServerConfig};
use rcgen::generate_simple_self_signed;
use sha2::{Digest, Sha256};
use std::error::Error;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Maps a peer's socket address to the SHA-256 fingerprint of the cert we
/// expect that peer to present. Used by the *client* side to pin the server
/// cert during handshake.
pub type FingerprintResolver = Arc<dyn Fn(SocketAddr) -> Option<Vec<u8>> + Send + Sync>;

/// Predicate over fingerprints: returns true if the given SHA-256 is one of
/// our paired peers. Used by the *server* side to validate a presented
/// client cert during the QUIC mTLS handshake — at that point we don't yet
/// know which peer is connecting, only that it must be one of ours.
pub type KnownFingerprintsResolver = Arc<dyn Fn(&[u8]) -> bool + Send + Sync>;

#[derive(Clone)]
pub struct Transport {
    pub endpoint: Endpoint,
    local_cert_der: Vec<u8>,
    local_key_der: Vec<u8>,
    fingerprint_resolver: Arc<Mutex<Option<FingerprintResolver>>>,
    known_fingerprints_resolver: Arc<Mutex<Option<KnownFingerprintsResolver>>>,
}

impl Transport {
    /// Build a Transport from a pre-existing self-signed cert. Persisting and loading
    /// the cert is the caller's responsibility — `Transport` doesn't touch disk.
    pub fn new(port: u16, cert_der: Vec<u8>, key_der: Vec<u8>) -> Result<Self, Box<dyn Error>> {
        let known_fingerprints_resolver: Arc<Mutex<Option<KnownFingerprintsResolver>>> =
            Arc::new(Mutex::new(None));

        let server_config = configure_server(
            cert_der.clone(),
            key_der.clone(),
            known_fingerprints_resolver.clone(),
        )?;

        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        // No `set_default_client_config` — every outbound connection goes
        // through `connect_with(...)` with a per-peer pinned config. Calling
        // `connect()` without an explicit config will error, which is what
        // we want: there is no safe default for an mTLS-only endpoint.
        let endpoint = Endpoint::server(server_config, addr)?;

        Ok(Self {
            endpoint,
            local_cert_der: cert_der,
            local_key_der: key_der,
            fingerprint_resolver: Arc::new(Mutex::new(None)),
            known_fingerprints_resolver,
        })
    }

    /// Install (or replace) the resolver that maps a peer address to its pinned
    /// SHA-256 fingerprint. Call this once known_peers is loaded.
    pub fn set_fingerprint_resolver(&self, resolver: FingerprintResolver) {
        *self.fingerprint_resolver.lock().unwrap() = Some(resolver);
    }

    /// Install (or replace) the predicate used by the server side of the
    /// QUIC mTLS handshake to recognise paired peers' client certs. Without
    /// this set, every inbound QUIC connection is rejected.
    pub fn set_known_fingerprints_resolver(&self, resolver: KnownFingerprintsResolver) {
        *self.known_fingerprints_resolver.lock().unwrap() = Some(resolver);
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
        let fp = self.resolve_fingerprint(addr).ok_or_else(|| -> Box<dyn Error + Send + Sync> {
            format!("no pinned fingerprint for {addr}; peer must re-pair").into()
        })?;
        configure_client(
            vec![b"clustercut-transport".to_vec()],
            fp,
            self.local_cert_der.clone(),
            self.local_key_der.clone(),
        )
        .map_err(|e| -> Box<dyn Error + Send + Sync> { e.to_string().into() })
    }

    fn file_config_for(&self, addr: SocketAddr) -> Result<ClientConfig, Box<dyn Error + Send + Sync>> {
        let fp = self.resolve_fingerprint(addr).ok_or_else(|| -> Box<dyn Error + Send + Sync> {
            format!("no pinned fingerprint for {addr}; peer must re-pair").into()
        })?;
        configure_client(
            vec![b"clustercut-file".to_vec()],
            fp,
            self.local_cert_der.clone(),
            self.local_key_der.clone(),
        )
        .map_err(|e| -> Box<dyn Error + Send + Sync> { e.to_string().into() })
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

fn configure_server(
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
    known_fingerprints_resolver: Arc<Mutex<Option<KnownFingerprintsResolver>>>,
) -> Result<ServerConfig, Box<dyn Error>> {
    let cert = rustls::pki_types::CertificateDer::from(cert_der);
    let key =
        rustls::pki_types::PrivateKeyDer::try_from(key_der).map_err(|_| "Invalid private key")?;

    let algorithms = signature_algorithms()?;
    let client_verifier = Arc::new(KnownFingerprintsClientVerifier {
        resolver: known_fingerprints_resolver,
        algorithms,
        no_subjects: Vec::new(),
    });

    let mut crypto = rustls::ServerConfig::builder()
        .with_client_cert_verifier(client_verifier)
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

fn signature_algorithms() -> Result<rustls::crypto::WebPkiSupportedAlgorithms, Box<dyn Error>> {
    let provider = rustls::crypto::CryptoProvider::get_default()
        .ok_or("no rustls crypto provider installed")?;
    Ok(provider.signature_verification_algorithms)
}

/// Server-cert verifier used by the client side of every QUIC connection.
/// Pins the SHA-256 of the server's end-entity cert DER (substituting the
/// usual CA chain validation) AND delegates the TLS handshake-signature
/// check to rustls's WebPKI implementation so the peer is forced to prove
/// possession of the matching private key — fingerprint match alone is
/// not enough (see issue #9).
#[derive(Debug)]
struct PinnedFingerprintServerVerifier {
    expected: Vec<u8>,
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl rustls::client::danger::ServerCertVerifier for PinnedFingerprintServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let actual = cert_fingerprint(end_entity.as_ref());
        if actual.as_slice() != self.expected.as_slice() {
            return Err(rustls::Error::General(format!(
                "server cert fingerprint mismatch: expected {}, got {}",
                hex_short(&self.expected),
                hex_short(&actual)
            )));
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// Client-cert verifier used by the server side of every QUIC connection.
/// Accepts any client cert whose SHA-256 fingerprint is in our paired-peers
/// set (consulted live via the resolver — new pairings during the session
/// are picked up without restart). Delegates handshake-signature validation
/// to rustls's WebPKI just like the server-cert verifier above.
struct KnownFingerprintsClientVerifier {
    resolver: Arc<Mutex<Option<KnownFingerprintsResolver>>>,
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
    /// rustls's `ClientCertVerifier` returns a slice of root-hint subjects
    /// in the CertificateRequest. We don't use a CA / DN-based system, so
    /// this is intentionally empty.
    no_subjects: Vec<rustls::DistinguishedName>,
}

impl std::fmt::Debug for KnownFingerprintsClientVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KnownFingerprintsClientVerifier")
            .finish_non_exhaustive()
    }
}

impl rustls::server::danger::ClientCertVerifier for KnownFingerprintsClientVerifier {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &self.no_subjects
    }

    fn verify_client_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        let actual = cert_fingerprint(end_entity.as_ref());
        let resolver = self.resolver.lock().unwrap();
        match resolver.as_ref() {
            Some(r) if r(&actual) => {
                Ok(rustls::server::danger::ClientCertVerified::assertion())
            }
            Some(_) => Err(rustls::Error::General(format!(
                "client cert fingerprint not in known peers: {}",
                hex_short(&actual)
            ))),
            None => Err(rustls::Error::General(
                "client auth attempted before known-fingerprints resolver installed".into(),
            )),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

fn configure_client(
    alpn_protocols: Vec<Vec<u8>>,
    pinned_fingerprint: Vec<u8>,
    local_cert_der: Vec<u8>,
    local_key_der: Vec<u8>,
) -> Result<ClientConfig, Box<dyn Error>> {
    let algorithms = signature_algorithms()?;
    let verifier = Arc::new(PinnedFingerprintServerVerifier {
        expected: pinned_fingerprint,
        algorithms,
    });

    let cert = rustls::pki_types::CertificateDer::from(local_cert_der);
    let key = rustls::pki_types::PrivateKeyDer::try_from(local_key_der)
        .map_err(|_| "Invalid client key")?;

    let mut client_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(vec![cert], key)?;

    client_config.alpn_protocols = alpn_protocols;

    let mut quic_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_config)?,
    ));

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
// `PairingMessage`. Per WIRE-PROTOCOL-0.3.1 §H4, the cap is sized for the
// largest legitimate frame and nothing larger — no forward-compatibility
// headroom, since the pairing protocol is intentionally not backwards-
// compatible.
// ────────────────────────────────────────────────────────────────────────────

/// Maximum size of one pairing frame on the wire (0.3.1).
///
/// Size budget for the largest frame (T2/T3, AEAD-wrapped identity):
/// - PairingMessage enum tag + JSON wrapper: ~30 B
/// - nonce (12 B)  → JSON int-array: ~48 B
/// - ciphertext = AEAD(plaintext) + 16 B tag
///     - plaintext = JSON of `PairIdInner{device_id (≤256 B), fingerprint (32 B)}`
///         worst-case device_id with full JSON escaping (`\u00xx`): ~6×256 = ~1536 B
///         fingerprint 32 B → JSON int-array: ~128 B
///         JSON wrapper / field names: ~60 B
///         plaintext total: ~1.8 KB
///     - ciphertext = plaintext + 16 B tag → ~1.8 KB
///     - ciphertext → JSON int-array on the wire: ~4× → ~7.5 KB
/// - 4-byte length prefix lives outside the cap (consumed before the cap
///   check), so the cap bounds the JSON body only.
///
/// 8 KB rounds up cleanly above that ceiling. T0/T1 frames are far smaller
/// (a single Ed25519 SPAKE2 element ~32 B, encoded ~128 B JSON), and easily
/// fit under the same cap.
pub const PAIRING_FRAME_CAP: usize = 8 * 1024;

/// Idle/protocol timeout applied around the responder's `handle_pairing_connection`.
/// Per WIRE-PROTOCOL-0.3.1 §H6, the responder must not let an accepted-but-idle
/// pairing connection block the cap=1 single-flight slot indefinitely. The
/// expected end-to-end pairing exchange completes in well under a second on a
/// LAN; the wall-clock budget here covers TCP slowness + JSON encode/decode
/// + a stray re-transmit, not human input.
pub const PAIRING_PROTOCOL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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
///
/// Bind happens synchronously (so a port conflict surfaces as an error
/// immediately) but the std → tokio listener conversion is deferred into
/// the spawned task because it requires an active tokio runtime.
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
    tracing::info!("Pairing TCP listener bound on {}", bind_addr);

    tauri::async_runtime::spawn(async move {
        let listener = match TcpListener::from_std(std_listener) {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("Failed to attach pairing listener to tokio runtime: {}", e);
                return;
            }
        };
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
