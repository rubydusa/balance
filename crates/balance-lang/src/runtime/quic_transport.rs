//! QUIC transport layer for Balance services.
//!
//! Provides the same framing protocol as TCP transport (length-prefixed JSON)
//! but over QUIC streams for better multiplexing and TLS-by-default.
//!
//! Enable the `quic` feature flag for real QUIC support via `quinn`.
//! Without it, `QuicTransportClient::dispatch` falls back to TCP.

use super::event::Event;
#[allow(unused_imports)]
use super::tcp_transport::{TransportRequest, TransportResponse};
use super::value::Value;

/// Check if QUIC transport is available (compiled with quic feature).
pub fn quic_available() -> bool {
    cfg!(feature = "quic")
}

/// QUIC endpoint configuration.
#[derive(Debug, Clone)]
pub struct QuicConfig {
    /// Bind address for the QUIC server (UDP).
    pub bind_addr: String,
    /// Optional TLS certificate path.
    pub cert_path: Option<String>,
    /// Optional TLS key path.
    pub key_path: Option<String>,
    /// Maximum concurrent streams per connection.
    pub max_concurrent_streams: u32,
    /// Connection idle timeout in seconds.
    pub idle_timeout_secs: u64,
}

impl Default for QuicConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:9443".to_string(),
            cert_path: None,
            key_path: None,
            max_concurrent_streams: 100,
            idle_timeout_secs: 30,
        }
    }
}

// ── QUIC transport client ──────────────────────────────────────────────────

pub struct QuicTransportClient;

impl QuicTransportClient {
    /// Dispatch a method call to a remote service over QUIC.
    ///
    /// When compiled with the `quic` feature, uses quinn for real QUIC transport.
    /// Otherwise, falls back to TCP transport with a warning.
    #[cfg(feature = "quic")]
    pub async fn dispatch(
        endpoint: &str,
        service_id: &str,
        method: &str,
        args: Vec<Value>,
    ) -> Result<(Value, Vec<Event>, u64), String> {
        use std::sync::Arc;

        let provider = rustls::crypto::ring::default_provider();
        let crypto = rustls::ClientConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("rustls config: {e}"))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipServerVerification(
                rustls::crypto::ring::default_provider(),
            )))
            .with_no_client_auth();

        let quic_crypto = quinn::crypto::rustls::QuicClientConfig::try_from(crypto)
            .map_err(|e| format!("quic crypto: {e}"))?;
        let client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));

        let mut ep = quinn::Endpoint::client("0.0.0.0:0".parse().unwrap())
            .map_err(|e| format!("quic endpoint: {e}"))?;
        ep.set_default_client_config(client_config);

        let addr: std::net::SocketAddr =
            endpoint.parse().map_err(|e| format!("parse address: {e}"))?;
        let conn = ep
            .connect(addr, "localhost")
            .map_err(|e| format!("quic connect: {e}"))?
            .await
            .map_err(|e| format!("quic handshake: {e}"))?;

        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| format!("quic open stream: {e}"))?;

        // Write request (length-prefixed JSON)
        let request = TransportRequest {
            service_id: service_id.to_string(),
            method: method.to_string(),
            args,
            signature: None,
            request_id: 0,
        };
        let request_json =
            serde_json::to_vec(&request).map_err(|e| format!("serialize request: {e}"))?;
        let len = request_json.len() as u32;
        send.write_all(&len.to_be_bytes())
            .await
            .map_err(|e| format!("write length: {e}"))?;
        send.write_all(&request_json)
            .await
            .map_err(|e| format!("write body: {e}"))?;
        let _ = send.finish();

        // Read response (length-prefixed JSON)
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf)
            .await
            .map_err(|e| format!("read length: {e}"))?;
        let resp_len = u32::from_be_bytes(len_buf) as usize;
        if resp_len > 16 * 1024 * 1024 {
            return Err(format!("response frame too large: {resp_len} bytes"));
        }
        let mut buf = vec![0u8; resp_len];
        recv.read_exact(&mut buf)
            .await
            .map_err(|e| format!("read body: {e}"))?;

        let response: TransportResponse =
            serde_json::from_slice(&buf).map_err(|e| format!("deserialize response: {e}"))?;

        if response.ok {
            let value = response
                .value
                .ok_or_else(|| "response ok but no value".to_string())?;
            Ok((value, response.events, response.lamport_time))
        } else {
            Err(response
                .error
                .unwrap_or_else(|| "unknown error".to_string()))
        }
    }

    #[cfg(not(feature = "quic"))]
    pub async fn dispatch(
        endpoint: &str,
        service_id: &str,
        method: &str,
        args: Vec<Value>,
    ) -> Result<(Value, Vec<Event>, u64), String> {
        eprintln!(
            "warning: QUIC transport not available (compile with 'quic' feature), falling back to TCP"
        );
        super::tcp_transport::TcpTransportClient::dispatch(endpoint, service_id, method, args).await
    }
}

// ── QUIC transport server (feature-gated) ──────────────────────────────────

#[cfg(feature = "quic")]
pub struct QuicTransportServer {
    transport: super::transport::InProcessTransport,
    event_bus: super::event::EventBus,
}

#[cfg(feature = "quic")]
impl QuicTransportServer {
    pub fn new(
        transport: super::transport::InProcessTransport,
        event_bus: super::event::EventBus,
    ) -> Self {
        Self {
            transport,
            event_bus,
        }
    }

    /// Generate a self-signed certificate for testing/development.
    pub fn generate_self_signed_cert()
        -> Result<(Vec<rustls::pki_types::CertificateDer<'static>>, rustls::pki_types::PrivateKeyDer<'static>), String>
    {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
            .map_err(|e| format!("generate cert: {e}"))?;
        let key_der = rustls::pki_types::PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der());
        let cert_der = rustls::pki_types::CertificateDer::from(cert.cert);
        Ok((vec![cert_der], key_der.into()))
    }

    /// Start serving over QUIC. Runs until dropped.
    pub async fn serve(
        &mut self,
        certs: Vec<rustls::pki_types::CertificateDer<'static>>,
        key: rustls::pki_types::PrivateKeyDer<'static>,
        addr: &str,
    ) -> Result<(), String> {
        let endpoint = self.make_endpoint(certs, key, addr)?;
        loop {
            let incoming = endpoint
                .accept()
                .await
                .ok_or_else(|| "endpoint closed".to_string())?;
            let conn = incoming
                .await
                .map_err(|e| format!("accept connection: {e}"))?;
            if let Err(e) = self.handle_connection(conn).await {
                eprintln!("quic server error: {e}");
            }
        }
    }

    /// Serve a fixed number of requests then return. Used for testing.
    pub async fn serve_n(
        &mut self,
        certs: Vec<rustls::pki_types::CertificateDer<'static>>,
        key: rustls::pki_types::PrivateKeyDer<'static>,
        addr: &str,
        n: usize,
    ) -> Result<(), String> {
        let endpoint = self.make_endpoint(certs, key, addr)?;
        for _ in 0..n {
            let incoming = endpoint
                .accept()
                .await
                .ok_or_else(|| "endpoint closed".to_string())?;
            let conn = incoming
                .await
                .map_err(|e| format!("accept connection: {e}"))?;
            if let Err(e) = self.handle_connection(conn).await {
                eprintln!("quic server error: {e}");
            }
        }
        // Allow in-flight responses to reach the peer before closing
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        endpoint.close(quinn::VarInt::from_u32(0), b"done");
        Ok(())
    }

    fn make_endpoint(
        &self,
        certs: Vec<rustls::pki_types::CertificateDer<'static>>,
        key: rustls::pki_types::PrivateKeyDer<'static>,
        addr: &str,
    ) -> Result<quinn::Endpoint, String> {
        use std::sync::Arc;

        let provider = rustls::crypto::ring::default_provider();
        let server_crypto = rustls::ServerConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("rustls config: {e}"))?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| format!("server cert: {e}"))?;

        let quic_crypto = quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
            .map_err(|e| format!("quic crypto: {e}"))?;
        let server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_crypto));

        let bind_addr: std::net::SocketAddr =
            addr.parse().map_err(|e| format!("parse bind addr: {e}"))?;
        quinn::Endpoint::server(server_config, bind_addr)
            .map_err(|e| format!("bind {addr}: {e}"))
    }

    async fn handle_connection(&mut self, conn: quinn::Connection) -> Result<(), String> {
        let (mut send, mut recv) = conn
            .accept_bi()
            .await
            .map_err(|e| format!("accept stream: {e}"))?;

        // Read request (length-prefixed JSON)
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf)
            .await
            .map_err(|e| format!("read length: {e}"))?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > 16 * 1024 * 1024 {
            return Err(format!("request frame too large: {len} bytes"));
        }
        let mut buf = vec![0u8; len];
        recv.read_exact(&mut buf)
            .await
            .map_err(|e| format!("read body: {e}"))?;

        let request: TransportRequest =
            serde_json::from_slice(&buf).map_err(|e| format!("deserialize request: {e}"))?;

        // Snapshot event count before dispatch to capture new events
        let pre_event_count = self.event_bus.events().len();

        // Dispatch via InProcessTransport (substrate first, then builtin)
        let result = self
            .transport
            .dispatch_substrate(
                &request.service_id,
                &request.method,
                request.args.clone(),
                &mut self.event_bus,
            )
            .or_else(|_| {
                self.transport
                    .dispatch_builtin(&request.service_id, &request.method, request.args)
            });

        // Collect new events emitted during dispatch
        let new_events: Vec<Event> = self.event_bus.events()[pre_event_count..].to_vec();
        let server_lamport = self.event_bus.lamport_time();

        let response = match result {
            Ok(val) => TransportResponse {
                ok: true,
                value: Some(val),
                error: None,
                events: new_events,
                lamport_time: server_lamport,
                request_id: 0,
            },
            Err(e) => TransportResponse {
                ok: false,
                value: None,
                error: Some(e),
                events: Vec::new(),
                lamport_time: server_lamport,
                request_id: 0,
            },
        };

        // Write response (length-prefixed JSON)
        let response_json =
            serde_json::to_vec(&response).map_err(|e| format!("serialize response: {e}"))?;
        let resp_len = response_json.len() as u32;
        send.write_all(&resp_len.to_be_bytes())
            .await
            .map_err(|e| format!("write length: {e}"))?;
        send.write_all(&response_json)
            .await
            .map_err(|e| format!("write body: {e}"))?;
        let _ = send.finish();

        Ok(())
    }
}

// ── Skip server verification (dev/test only) ──────────────────────────────

#[cfg(feature = "quic")]
#[derive(Debug)]
struct SkipServerVerification(rustls::crypto::CryptoProvider);

#[cfg(feature = "quic")]
impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0
            .signature_verification_algorithms
            .supported_schemes()
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quic_config_default() {
        let config = QuicConfig::default();
        assert_eq!(config.bind_addr, "127.0.0.1:9443");
        assert!(config.cert_path.is_none());
        assert_eq!(config.max_concurrent_streams, 100);
        assert_eq!(config.idle_timeout_secs, 30);
    }

    #[test]
    fn test_request_response_types_shared() {
        let req = TransportRequest {
            service_id: "svc/main".to_string(),
            method: "get".to_string(),
            args: vec![Value::String("key1".into())],
            signature: None,
            request_id: 0,
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: TransportRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.service_id, "svc/main");
        assert_eq!(decoded.method, "get");
    }

    #[cfg(not(feature = "quic"))]
    #[test]
    fn test_quic_not_available() {
        assert!(!quic_available());
    }

    #[cfg(feature = "quic")]
    #[test]
    fn test_quic_available() {
        assert!(quic_available());
    }

    #[cfg(feature = "quic")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_quic_round_trip() {
        use std::sync::Arc;

        // Generate self-signed cert
        let cert_gen =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let key_der =
            rustls::pki_types::PrivatePkcs8KeyDer::from(cert_gen.key_pair.serialize_der());
        let cert_der = rustls::pki_types::CertificateDer::from(cert_gen.cert);

        // Create server endpoint
        let provider = rustls::crypto::ring::default_provider();
        let server_crypto =
            rustls::ServerConfig::builder_with_provider(Arc::new(provider))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![cert_der], key_der.into())
                .unwrap();
        let quic_crypto =
            quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto).unwrap();
        let server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_crypto));
        let server_ep = quinn::Endpoint::server(
            server_config,
            "127.0.0.1:0".parse().unwrap(),
        )
        .unwrap();
        let bound_addr = server_ep.local_addr().unwrap().to_string();

        // Spawn echo server (Send-safe, no InProcessTransport)
        let server_handle = tokio::spawn(async move {
            let incoming = server_ep.accept().await.unwrap();
            let conn = incoming.await.unwrap();
            let (mut send, mut recv) = conn.accept_bi().await.unwrap();

            // Read request frame
            let mut len_buf = [0u8; 4];
            recv.read_exact(&mut len_buf).await.unwrap();
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut buf = vec![0u8; len];
            recv.read_exact(&mut buf).await.unwrap();
            let request: TransportRequest = serde_json::from_slice(&buf).unwrap();

            // Echo response
            let response = TransportResponse {
                ok: true,
                value: Some(Value::String(format!("quic: {}", request.method))),
                error: None,
                events: Vec::new(),
                lamport_time: 0,
                request_id: 0,
            };
            let response_json = serde_json::to_vec(&response).unwrap();
            send.write_all(&(response_json.len() as u32).to_be_bytes())
                .await
                .unwrap();
            send.write_all(&response_json).await.unwrap();
            let _ = send.finish();
            // Keep connection alive until client closes it
            conn.closed().await;
        });

        // Client dispatch
        let result = QuicTransportClient::dispatch(
            &bound_addr,
            "test/svc",
            "echo",
            vec![],
        )
        .await;

        server_handle.await.unwrap();

        assert!(result.is_ok(), "quic dispatch failed: {:?}", result.err());
        let (val, _events, _lamport) = result.unwrap();
        assert_eq!(val, Value::String("quic: echo".to_string()));
    }
}
