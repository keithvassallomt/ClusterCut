use quinn::{ClientConfig, Endpoint, ServerConfig};
use rcgen::generate_simple_self_signed;
use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Clone)]
pub struct Transport {
    pub endpoint: Endpoint,
    transport_config: ClientConfig,
    file_config: ClientConfig,
}

impl Transport {
    pub fn new(port: u16) -> Result<Self, Box<dyn Error>> {
        let (cert_der, key_der) = generate_self_signed_cert()?;
        let server_config = configure_server(cert_der, key_der)?;

        let transport_config = configure_client(vec![b"clustercut-transport".to_vec()])?;
        let file_config = configure_client(vec![b"clustercut-file".to_vec()])?;

        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let mut endpoint = Endpoint::server(server_config, addr)?;
        endpoint.set_default_client_config(transport_config.clone());

        Ok(Self {
            endpoint,
            transport_config,
            file_config,
        })
    }

    pub async fn send_message(
        &self,
        addr: SocketAddr,
        data: &[u8],
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        // Use connect_with to enforce specific ALPN config
        let connection = self
            .endpoint
            .connect_with(self.transport_config.clone(), addr, "clustercut")?
            .await?;
        let (mut send, _recv) = connection.open_bi().await?;

        send.write_all(data).await?;
        send.finish()?;

        // Wait until the peer has acknowledged all stream data before letting
        // the connection drop. The previous fixed 500 ms sleep was fine for
        // text payloads but raced multi-MB clipboard images — the function
        // returned and the connection torn down while bytes were still in
        // flight, surfacing as "connection lost" on the receiver mid-read.
        // 30 s upper bound covers slow links without hanging forever if the
        // peer dies.
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            send.stopped(),
        )
        .await;

        Ok(())
    }

    /// Open a dedicated file stream connection to start sending a file
    /// Returns the SendStream so the caller can pump data into it.
    pub async fn send_file_stream(
        &self,
        addr: SocketAddr,
    ) -> Result<(quinn::Connection, quinn::SendStream), Box<dyn Error + Send + Sync>> {
        // Use connect_with to enforce specific ALPN config
        let connection = self
            .endpoint
            .connect_with(self.file_config.clone(), addr, "clustercut")?
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

fn generate_self_signed_cert() -> Result<(Vec<u8>, Vec<u8>), Box<dyn Error>> {
    // Register BOTH protocols
    let cert = generate_simple_self_signed(vec![
        "clustercut-transport".into(),
        "clustercut-file".into(),
        "clustercut".into(), // Add generic SNI validity
    ])?;
    Ok((cert.cert.der().to_vec(), cert.signing_key.serialize_der()))
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

fn configure_client(alpn_protocols: Vec<Vec<u8>>) -> Result<ClientConfig, Box<dyn Error>> {
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};

    #[derive(Debug)]
    struct SkipServerVerification;
    impl ServerCertVerifier for SkipServerVerification {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
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
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
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
