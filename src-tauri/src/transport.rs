use quinn::{ClientConfig, Endpoint, ServerConfig};
use rcgen::generate_simple_self_signed;
use std::error::Error;
use std::net::SocketAddr;
use std::sync::Arc;

#[derive(Clone)]
pub struct Transport {
    pub endpoint: Endpoint,
}

impl Transport {
    pub fn new(port: u16) -> Result<Self, Box<dyn Error>> {
        let (cert_der, key_der) = generate_self_signed_cert()?;
        let server_config = configure_server(cert_der, key_der)?;
        let client_config = configure_client()?;

        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let mut endpoint = Endpoint::server(server_config, addr)?;
        endpoint.set_default_client_config(client_config);

        Ok(Self { endpoint })
    }

    pub async fn send_message(&self, addr: SocketAddr, data: &[u8]) -> Result<(), Box<dyn Error>> {
        let connection = self.endpoint.connect(addr, "clustercut-transport")?.await?;
        let (mut send, _recv) = connection.open_bi().await?; // Rename recv to _recv

        send.write_all(data).await?;
        send.finish()?;

        // Give the stream a moment to flush/be accepted before dropping the connection
        // This is a hack; a better way is to wait for acknowledgement or use a long-lived connection.
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        Ok(())
    }

    pub fn start_listening<F>(&self, on_receive: F)
    where
        F: Fn(Vec<u8>, SocketAddr) + Send + Sync + 'static + Clone,
    {
        let endpoint = self.endpoint.clone();
        tauri::async_runtime::spawn(async move {
            tracing::info!("Starting transport listener loop...");
            while let Some(conn) = endpoint.accept().await {
                tracing::debug!("Transport accepted a connection attempt...");
                let connection = conn.await;
                match connection {
                    Ok(conn) => {
                        let remote_addr = conn.remote_address();
                        tracing::info!("Transport established connection with {}", remote_addr);
                        let on_receive = on_receive.clone();
                        tauri::async_runtime::spawn(async move {
                            tracing::debug!(
                                "Waiting for bidirectional stream from {}",
                                remote_addr
                            );
                            loop {
                                match conn.accept_bi().await {
                                    Ok((_, mut recv)) => {
                                        tracing::debug!("Accepted stream from {}", remote_addr);
                                        // Limit 10MB
                                        if let Ok(buf) = recv.read_to_end(1024 * 1024 * 10).await {
                                            tracing::trace!(
                                                "Read {} bytes from stream from {}",
                                                buf.len(),
                                                remote_addr
                                            );
                                            on_receive(buf, remote_addr);
                                        } else {
                                            tracing::error!(
                                                "Failed to read from stream from {}",
                                                remote_addr
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            "Failed to accept stream from {}: {}",
                                            remote_addr,
                                            e
                                        );
                                        break;
                                    }
                                }
                            }
                            tracing::debug!("Stream loop ended for {}", remote_addr);
                        });
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
    let cert = generate_simple_self_signed(vec!["clustercut-transport".into()])?;
    Ok((cert.cert.der().to_vec(), cert.signing_key.serialize_der()))
}

fn configure_server(cert_der: Vec<u8>, key_der: Vec<u8>) -> Result<ServerConfig, Box<dyn Error>> {
    let cert = rustls::pki_types::CertificateDer::from(cert_der);
    let key =
        rustls::pki_types::PrivateKeyDer::try_from(key_der).map_err(|_| "Invalid private key")?;

    let server_config = ServerConfig::with_single_cert(vec![cert], key)?;
    Ok(server_config)
}

fn configure_client() -> Result<ClientConfig, Box<dyn Error>> {
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

    let client_config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();

    let quic_config = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(client_config)?,
    ));

    Ok(quic_config)
}
