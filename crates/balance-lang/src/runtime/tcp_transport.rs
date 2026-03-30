use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::event::{Event, EventBus};
use super::transport::InProcessTransport;
use super::value::Value;

/// Wire-format request for TCP transport.
#[derive(Debug, Serialize, Deserialize)]
pub struct TransportRequest {
    pub service_id: String,
    pub method: String,
    pub args: Vec<Value>,
    /// HMAC-SHA256 signature over `service_id + method + serialized(args)`.
    /// Present when the client has a signing key configured.
    #[serde(default)]
    pub signature: Option<String>,
    /// Optional request ID for correlating responses in multiplexed connections.
    #[serde(default)]
    pub request_id: u64,
}

/// Wire-format response for TCP transport.
#[derive(Debug, Serialize, Deserialize)]
pub struct TransportResponse {
    pub ok: bool,
    pub value: Option<Value>,
    pub error: Option<String>,
    /// Real events from server-side dispatch (forwarded to client).
    #[serde(default)]
    pub events: Vec<Event>,
    /// Lamport timestamp from the server for causal ordering.
    #[serde(default)]
    pub lamport_time: u64,
    /// Echoed request ID for correlation.
    #[serde(default)]
    pub request_id: u64,
}

/// Read a length-prefixed JSON frame from a stream.
pub async fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .map_err(|e| format!("read length: {e}"))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        return Err(format!("frame too large: {len} bytes"));
    }
    let mut buf = vec![0u8; len];
    stream
        .read_exact(&mut buf)
        .await
        .map_err(|e| format!("read body: {e}"))?;
    Ok(buf)
}

/// Write a length-prefixed JSON frame to a stream.
pub async fn write_frame(stream: &mut TcpStream, data: &[u8]) -> Result<(), String> {
    let len = data.len() as u32;
    stream
        .write_all(&len.to_be_bytes())
        .await
        .map_err(|e| format!("write length: {e}"))?;
    stream
        .write_all(data)
        .await
        .map_err(|e| format!("write body: {e}"))?;
    stream
        .flush()
        .await
        .map_err(|e| format!("flush: {e}"))?;
    Ok(())
}

/// Compute HMAC-SHA256 signature over `service_id + method + serialized(args)`.
/// Returns hex-encoded signature string.
pub fn compute_request_signature(key: &[u8], service_id: &str, method: &str, args: &[Value]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
    mac.update(service_id.as_bytes());
    mac.update(method.as_bytes());
    let args_json = serde_json::to_string(args).unwrap_or_default();
    mac.update(args_json.as_bytes());
    let result = mac.finalize();
    result.into_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>()
}

/// Verify a request signature. Returns Ok(()) if valid, Err(message) if invalid.
pub fn verify_request_signature(
    key: &[u8],
    service_id: &str,
    method: &str,
    args: &[Value],
    signature: &str,
) -> Result<(), String> {
    let expected = compute_request_signature(key, service_id, method, args);
    if expected == signature {
        Ok(())
    } else {
        Err("invalid request signature".to_string())
    }
}

/// TCP transport client. Connects to a remote service endpoint and dispatches
/// method calls over the wire using length-prefixed JSON frames.
pub struct TcpTransportClient;

impl TcpTransportClient {
    /// Dispatch a method call to a remote service over TCP.
    /// Returns the result value, any events forwarded from the server, and the server's lamport timestamp.
    pub async fn dispatch(
        endpoint: &str,
        service_id: &str,
        method: &str,
        args: Vec<Value>,
    ) -> Result<(Value, Vec<Event>, u64), String> {
        Self::dispatch_with_key(endpoint, service_id, method, args, None).await
    }

    /// Dispatch with an optional signing key for transport boundary verification.
    pub async fn dispatch_with_key(
        endpoint: &str,
        service_id: &str,
        method: &str,
        args: Vec<Value>,
        signing_key: Option<&[u8]>,
    ) -> Result<(Value, Vec<Event>, u64), String> {
        let mut stream = TcpStream::connect(endpoint)
            .await
            .map_err(|e| format!("tcp connect to {endpoint}: {e}"))?;

        let signature = signing_key.map(|key| {
            compute_request_signature(key, service_id, method, &args)
        });

        let request = TransportRequest {
            service_id: service_id.to_string(),
            method: method.to_string(),
            args,
            signature,
            request_id: 0,
        };

        let request_json =
            serde_json::to_vec(&request).map_err(|e| format!("serialize request: {e}"))?;
        write_frame(&mut stream, &request_json).await?;

        let response_bytes = read_frame(&mut stream).await?;
        let response: TransportResponse = serde_json::from_slice(&response_bytes)
            .map_err(|e| format!("deserialize response: {e}"))?;

        if response.ok {
            let value = response
                .value
                .ok_or_else(|| "response ok but no value".to_string())?;
            Ok((value, response.events, response.lamport_time))
        } else {
            Err(response.error.unwrap_or_else(|| "unknown error".to_string()))
        }
    }
}

/// TCP transport server. Listens for incoming connections and routes dispatches
/// to a local InProcessTransport.
pub struct TcpTransportServer {
    transport: InProcessTransport,
    event_bus: EventBus,
    /// Optional signing key for request verification at transport boundary.
    signing_key: Option<Vec<u8>>,
}

impl TcpTransportServer {
    pub fn new(transport: InProcessTransport, event_bus: EventBus) -> Self {
        Self {
            transport,
            event_bus,
            signing_key: None,
        }
    }

    /// Set a signing key for verifying incoming request signatures.
    pub fn set_signing_key(&mut self, key: Vec<u8>) {
        self.signing_key = Some(key);
    }

    /// Start listening and serving requests. Runs until dropped.
    pub async fn serve(&mut self, addr: &str) -> Result<(), String> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| format!("bind {addr}: {e}"))?;

        loop {
            let (mut stream, _peer) = listener
                .accept()
                .await
                .map_err(|e| format!("accept: {e}"))?;

            // Handle one request per connection (simple protocol)
            match self.handle_connection(&mut stream).await {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("tcp server error: {e}");
                }
            }
        }
    }

    /// Serve a fixed number of requests then return. Used for testing.
    pub async fn serve_n(&mut self, addr: &str, n: usize) -> Result<(), String> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| format!("bind {addr}: {e}"))?;

        for _ in 0..n {
            let (mut stream, _peer) = listener
                .accept()
                .await
                .map_err(|e| format!("accept: {e}"))?;

            if let Err(e) = self.handle_connection(&mut stream).await {
                eprintln!("tcp server error: {e}");
            }
        }
        Ok(())
    }

    async fn handle_connection(&mut self, stream: &mut TcpStream) -> Result<(), String> {
        self.handle_single_request(stream).await
    }

    /// Serve a fixed number of requests on a single persistent connection. Used for testing.
    pub async fn serve_n_multi(&mut self, addr: &str, n: usize) -> Result<(), String> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| format!("bind {addr}: {e}"))?;

        let (mut stream, _peer) = listener
            .accept()
            .await
            .map_err(|e| format!("accept: {e}"))?;

        for _ in 0..n {
            self.handle_single_request(&mut stream).await?;
        }
        Ok(())
    }

    /// Handle multiple requests on a single connection until the client disconnects.
    pub async fn handle_connection_multi(&mut self, stream: &mut TcpStream) -> Result<(), String> {
        loop {
            match self.handle_single_request(stream).await {
                Ok(()) => {}
                Err(_) => break, // client disconnected or error → stop
            }
        }
        Ok(())
    }

    async fn handle_single_request(&mut self, stream: &mut TcpStream) -> Result<(), String> {
        let request_bytes = read_frame(stream).await?;
        let request: TransportRequest = serde_json::from_slice(&request_bytes)
            .map_err(|e| format!("deserialize request: {e}"))?;

        let request_id = request.request_id;

        // Verify request signature if signing key is configured
        if let Some(ref key) = self.signing_key {
            match &request.signature {
                Some(sig) => {
                    if let Err(e) = verify_request_signature(key, &request.service_id, &request.method, &request.args, sig) {
                        let response = TransportResponse {
                            ok: false,
                            value: None,
                            error: Some(e),
                            events: Vec::new(),
                            lamport_time: self.event_bus.lamport_time(),
                            request_id,
                        };
                        let response_json = serde_json::to_vec(&response).map_err(|e| format!("serialize response: {e}"))?;
                        write_frame(stream, &response_json).await?;
                        return Ok(());
                    }
                }
                None => {
                    let response = TransportResponse {
                        ok: false,
                        value: None,
                        error: Some("request signature required but not provided".to_string()),
                        events: Vec::new(),
                        lamport_time: self.event_bus.lamport_time(),
                        request_id,
                    };
                    let response_json = serde_json::to_vec(&response).map_err(|e| format!("serialize response: {e}"))?;
                    write_frame(stream, &response_json).await?;
                    return Ok(());
                }
            }
        }

        // Snapshot event count before dispatch to capture new events
        let pre_event_count = self.event_bus.events().len();

        // Try substrate dispatch first, then fall back to builtin
        let result = self
            .transport
            .dispatch_substrate(
                &request.service_id,
                &request.method,
                request.args.clone(),
                &mut self.event_bus,
            )
            .or_else(|_| {
                self.transport.dispatch_builtin(
                    &request.service_id,
                    &request.method,
                    request.args,
                )
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
                request_id,
            },
            Err(e) => TransportResponse {
                ok: false,
                value: None,
                error: Some(e),
                events: Vec::new(),
                lamport_time: server_lamport,
                request_id,
            },
        };

        let response_json =
            serde_json::to_vec(&response).map_err(|e| format!("serialize response: {e}"))?;
        write_frame(stream, &response_json).await?;

        Ok(())
    }
}

/// Connection pool for TCP transport clients.
/// Reuses connections across multiple dispatches to the same endpoint.
pub struct TcpConnectionPool {
    connections: std::collections::HashMap<String, TcpStream>,
    next_request_id: u64,
}

impl TcpConnectionPool {
    pub fn new() -> Self {
        Self {
            connections: std::collections::HashMap::new(),
            next_request_id: 1,
        }
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    /// Dispatch a method call, reusing an existing connection if available.
    pub async fn dispatch(
        &mut self,
        endpoint: &str,
        service_id: &str,
        method: &str,
        args: Vec<Value>,
    ) -> Result<(Value, Vec<Event>, u64, u64), String> {
        let request_id = self.next_id();

        // Get or create connection
        let stream = if let Some(s) = self.connections.get_mut(endpoint) {
            s
        } else {
            let s = TcpStream::connect(endpoint)
                .await
                .map_err(|e| format!("tcp connect to {endpoint}: {e}"))?;
            self.connections.insert(endpoint.to_string(), s);
            self.connections.get_mut(endpoint).unwrap()
        };

        let request = TransportRequest {
            service_id: service_id.to_string(),
            method: method.to_string(),
            args,
            signature: None,
            request_id,
        };

        let request_json =
            serde_json::to_vec(&request).map_err(|e| format!("serialize request: {e}"))?;

        // If writing fails, remove the stale connection and fail
        if let Err(e) = write_frame(stream, &request_json).await {
            self.connections.remove(endpoint);
            return Err(e);
        }

        let response_bytes = match read_frame(stream).await {
            Ok(b) => b,
            Err(e) => {
                self.connections.remove(endpoint);
                return Err(e);
            }
        };
        let response: TransportResponse = serde_json::from_slice(&response_bytes)
            .map_err(|e| format!("deserialize response: {e}"))?;

        if response.ok {
            let value = response
                .value
                .ok_or_else(|| "response ok but no value".to_string())?;
            Ok((value, response.events, response.lamport_time, response.request_id))
        } else {
            Err(response.error.unwrap_or_else(|| "unknown error".to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::builtins::StdoutService;
    use crate::runtime::service::ServiceRuntime;
    use crate::runtime::substrate::SubstrateRegistry;

    #[tokio::test]
    async fn test_tcp_round_trip() {
        use tokio::task::LocalSet;

        let local = LocalSet::new();
        local.run_until(async {
            // Set up a server with a StdoutService
            let mut runtime = ServiceRuntime::new();
            runtime.register("stdout/default".to_string(), Box::new(StdoutService::new()));
            let substrate_registry = SubstrateRegistry::new();
            let transport = InProcessTransport::new(runtime, substrate_registry);
            let event_bus = EventBus::new();
            let mut server = TcpTransportServer::new(transport, event_bus);

            // Bind listener and get port, then drop so server can re-bind
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let bound_addr = listener.local_addr().unwrap().to_string();
            drop(listener);

            // Spawn server locally (not Send-bound)
            let server_addr = bound_addr.clone();
            let server_handle = tokio::task::spawn_local(async move {
                server.serve_n(&server_addr, 1).await.unwrap();
            });

            // Give server a moment to start
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

            // Client sends request
            let result = TcpTransportClient::dispatch(
                &bound_addr,
                "stdout/default",
                "writeln",
                vec![Value::String("hello from tcp".into())],
            )
            .await;

            assert!(result.is_ok(), "tcp dispatch failed: {:?}", result.err());
            let (val, _events, _lamport) = result.unwrap();
            assert!(val.is_ack(), "expected Ack, got: {val}");
            assert!(val.ack_key().unwrap().starts_with("stdout_op_"));

            server_handle.await.unwrap();
        }).await;
    }

    #[test]
    fn test_request_serialization() {
        let req = TransportRequest {
            service_id: "svc/main".to_string(),
            method: "put".to_string(),
            args: vec![Value::String("hello".into()), Value::Int(42)],
            signature: None,
            request_id: 0,
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: TransportRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.service_id, "svc/main");
        assert_eq!(decoded.method, "put");
        assert_eq!(decoded.args.len(), 2);
    }

    #[test]
    fn test_response_serialization() {
        let resp = TransportResponse {
            ok: true,
            value: Some(Value::ack("k1".to_string())),
            error: None,
            events: Vec::new(),
            lamport_time: 0,
            request_id: 0,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: TransportResponse = serde_json::from_str(&json).unwrap();
        assert!(decoded.ok);
        assert!(decoded.value.is_some());
    }

    #[tokio::test]
    async fn test_tcp_lamport_time_populated() {
        use tokio::task::LocalSet;

        let local = LocalSet::new();
        local.run_until(async {
            let mut runtime = ServiceRuntime::new();
            runtime.register("stdout/default".to_string(), Box::new(StdoutService::new()));
            let substrate_registry = SubstrateRegistry::new();
            let transport = InProcessTransport::new(runtime, substrate_registry);
            // Create EventBus with some activity so lamport > 0
            let mut event_bus = EventBus::new();
            event_bus.publish("test".into(), "setup".into(), std::collections::HashMap::new());

            let mut server = TcpTransportServer::new(transport, event_bus);

            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let bound_addr = listener.local_addr().unwrap().to_string();
            drop(listener);

            let server_addr = bound_addr.clone();
            let server_handle = tokio::task::spawn_local(async move {
                server.serve_n(&server_addr, 1).await.unwrap();
            });

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

            let result = TcpTransportClient::dispatch(
                &bound_addr,
                "stdout/default",
                "writeln",
                vec![Value::String("hello".into())],
            ).await;

            assert!(result.is_ok());
            let (_val, _events, lamport) = result.unwrap();
            assert!(lamport > 0, "server lamport_time should be > 0");

            server_handle.await.unwrap();
        }).await;
    }

    #[test]
    fn test_response_lamport_backward_compat() {
        // Old responses without lamport_time should deserialize with default 0
        let resp = TransportResponse {
            ok: true,
            value: Some(Value::String("hello".to_string())),
            error: None,
            events: Vec::new(),
            lamport_time: 0,
            request_id: 0,
        };
        let json = serde_json::to_string(&resp).unwrap();
        // Remove lamport_time field to simulate old server
        let json_no_lamport = json.replace(r#","lamport_time":0"#, "");
        let decoded: TransportResponse = serde_json::from_str(&json_no_lamport).unwrap();
        assert!(decoded.ok);
        assert_eq!(decoded.lamport_time, 0);
    }

    #[test]
    fn test_response_backward_compat_no_events_field() {
        // Old servers send no "events" field — #[serde(default)] fills empty Vec
        let resp = TransportResponse {
            ok: true,
            value: Some(Value::String("hello".to_string())),
            error: None,
            events: Vec::new(),
            lamport_time: 0,
            request_id: 0,
        };
        let json = serde_json::to_string(&resp).unwrap();
        // Deserialize with events field present
        let decoded: TransportResponse = serde_json::from_str(&json).unwrap();
        assert!(decoded.ok);
        assert!(decoded.events.is_empty());

        // Also test that omitting "events" entirely still works (backward compat)
        // Remove the "events" key from the JSON string
        let json_no_events = json.replace(r#","events":[]"#, "");
        let decoded2: TransportResponse = serde_json::from_str(&json_no_events).unwrap();
        assert!(decoded2.ok);
        assert!(decoded2.events.is_empty());
    }

    #[test]
    fn test_request_signature_round_trip() {
        let key = b"test-secret-key";
        let sig = super::compute_request_signature(
            key,
            "kv/main",
            "put",
            &[Value::String("k".into()), Value::String("v".into())],
        );
        assert_eq!(sig.len(), 64); // HMAC-SHA256 hex is 64 chars
        assert!(super::verify_request_signature(
            key,
            "kv/main",
            "put",
            &[Value::String("k".into()), Value::String("v".into())],
            &sig,
        ).is_ok());
    }

    #[test]
    fn test_bad_signature_rejected() {
        let key = b"test-secret-key";
        let result = super::verify_request_signature(
            key,
            "kv/main",
            "put",
            &[Value::String("k".into())],
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid request signature"));
    }

    #[test]
    fn test_wrong_key_signature_rejected() {
        let key1 = b"key-one";
        let key2 = b"key-two";
        let sig = super::compute_request_signature(key1, "svc", "method", &[]);
        let result = super::verify_request_signature(key2, "svc", "method", &[], &sig);
        assert!(result.is_err());
    }

    #[test]
    fn test_signature_deterministic() {
        let key = b"deterministic-key";
        let sig1 = super::compute_request_signature(key, "svc/a", "get", &[Value::Int(42)]);
        let sig2 = super::compute_request_signature(key, "svc/a", "get", &[Value::Int(42)]);
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn test_request_signature_field_backward_compat() {
        // Old requests without signature field should deserialize with None
        let json = r#"{"service_id":"svc","method":"m","args":[]}"#;
        let decoded: TransportRequest = serde_json::from_str(json).unwrap();
        assert!(decoded.signature.is_none());
    }

    #[tokio::test]
    async fn test_tcp_server_rejects_missing_signature() {
        use tokio::task::LocalSet;

        let local = LocalSet::new();
        local.run_until(async {
            let mut runtime = ServiceRuntime::new();
            runtime.register("stdout/default".to_string(), Box::new(StdoutService::new()));
            let substrate_registry = SubstrateRegistry::new();
            let transport = InProcessTransport::new(runtime, substrate_registry);
            let event_bus = EventBus::new();
            let mut server = TcpTransportServer::new(transport, event_bus);
            server.set_signing_key(b"secret".to_vec());

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let bound_addr = listener.local_addr().unwrap().to_string();
            drop(listener);

            let server_addr = bound_addr.clone();
            let server_handle = tokio::task::spawn_local(async move {
                server.serve_n(&server_addr, 1).await.unwrap();
            });

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

            // Client sends request WITHOUT signature — should be rejected
            let result = TcpTransportClient::dispatch(
                &bound_addr,
                "stdout/default",
                "writeln",
                vec![Value::String("hello".into())],
            ).await;

            assert!(result.is_err(), "unsigned request should be rejected when server has signing key");
            let err = result.unwrap_err();
            assert!(err.contains("signature required") || err.contains("not provided"), "error: {err}");

            server_handle.await.unwrap();
        }).await;
    }

    #[tokio::test]
    async fn test_tcp_multi_request_single_connection() {
        use tokio::task::LocalSet;

        let local = LocalSet::new();
        local.run_until(async {
            let mut runtime = ServiceRuntime::new();
            runtime.register("stdout/default".to_string(), Box::new(StdoutService::new()));
            let substrate_registry = SubstrateRegistry::new();
            let transport = InProcessTransport::new(runtime, substrate_registry);
            let event_bus = EventBus::new();
            let mut server = TcpTransportServer::new(transport, event_bus);

            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let bound_addr = listener.local_addr().unwrap().to_string();
            drop(listener);

            // Server handles 3 requests on a single connection
            let server_addr = bound_addr.clone();
            let server_handle = tokio::task::spawn_local(async move {
                server.serve_n_multi(&server_addr, 3).await.unwrap();
            });

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

            // Client opens ONE connection and sends 3 requests
            let mut stream = TcpStream::connect(&bound_addr).await.unwrap();

            for i in 0..3 {
                let request = TransportRequest {
                    service_id: "stdout/default".to_string(),
                    method: "writeln".to_string(),
                    args: vec![Value::String(format!("msg {i}"))],
                    signature: None,
                    request_id: i as u64 + 1,
                };
                let req_json = serde_json::to_vec(&request).unwrap();
                write_frame(&mut stream, &req_json).await.unwrap();
                let resp_bytes = read_frame(&mut stream).await.unwrap();
                let resp: TransportResponse = serde_json::from_slice(&resp_bytes).unwrap();
                assert!(resp.ok, "request {i} failed: {:?}", resp.error);
            }

            server_handle.await.unwrap();
        }).await;
    }

    #[tokio::test]
    async fn test_tcp_request_id_correlation() {
        use tokio::task::LocalSet;

        let local = LocalSet::new();
        local.run_until(async {
            let mut runtime = ServiceRuntime::new();
            runtime.register("stdout/default".to_string(), Box::new(StdoutService::new()));
            let substrate_registry = SubstrateRegistry::new();
            let transport = InProcessTransport::new(runtime, substrate_registry);
            let event_bus = EventBus::new();
            let mut server = TcpTransportServer::new(transport, event_bus);

            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let bound_addr = listener.local_addr().unwrap().to_string();
            drop(listener);

            let server_addr = bound_addr.clone();
            let server_handle = tokio::task::spawn_local(async move {
                server.serve_n_multi(&server_addr, 2).await.unwrap();
            });

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

            let mut stream = TcpStream::connect(&bound_addr).await.unwrap();

            // Send two requests with specific IDs and verify they are echoed
            for req_id in [42u64, 99u64] {
                let request = TransportRequest {
                    service_id: "stdout/default".to_string(),
                    method: "writeln".to_string(),
                    args: vec![Value::String("test".into())],
                    signature: None,
                    request_id: req_id,
                };
                let req_json = serde_json::to_vec(&request).unwrap();
                write_frame(&mut stream, &req_json).await.unwrap();
                let resp_bytes = read_frame(&mut stream).await.unwrap();
                let resp: TransportResponse = serde_json::from_slice(&resp_bytes).unwrap();
                assert!(resp.ok);
                assert_eq!(resp.request_id, req_id, "response should echo request_id");
            }

            server_handle.await.unwrap();
        }).await;
    }

    #[tokio::test]
    async fn test_tcp_connection_pool_reuse() {
        use tokio::task::LocalSet;

        let local = LocalSet::new();
        local.run_until(async {
            let mut runtime = ServiceRuntime::new();
            runtime.register("stdout/default".to_string(), Box::new(StdoutService::new()));
            let substrate_registry = SubstrateRegistry::new();
            let transport = InProcessTransport::new(runtime, substrate_registry);
            let event_bus = EventBus::new();
            let mut server = TcpTransportServer::new(transport, event_bus);

            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let bound_addr = listener.local_addr().unwrap().to_string();
            drop(listener);

            let server_addr = bound_addr.clone();
            let server_handle = tokio::task::spawn_local(async move {
                server.serve_n_multi(&server_addr, 3).await.unwrap();
            });

            tokio::time::sleep(std::time::Duration::from_millis(50)).await;

            let mut pool = TcpConnectionPool::new();

            // Three dispatches through the pool — should reuse the same connection
            for i in 0..3 {
                let result = pool.dispatch(
                    &bound_addr,
                    "stdout/default",
                    "writeln",
                    vec![Value::String(format!("pooled {i}"))],
                ).await;
                assert!(result.is_ok(), "pool dispatch {i} failed: {:?}", result.err());
                let (_val, _events, _lamport, resp_id) = result.unwrap();
                // Pool assigns sequential IDs starting at 1
                assert_eq!(resp_id, (i + 1) as u64, "response should echo pool-assigned request_id");
            }

            // Pool should have exactly 1 connection (reused)
            assert_eq!(pool.connections.len(), 1, "pool should reuse a single connection");

            server_handle.await.unwrap();
        }).await;
    }
}
