use super::event::EventBus;
use super::registry::{ServiceDescriptor, ServiceLocation};
use super::service::ServiceRuntime;
use super::substrate::SubstrateRegistry;
use super::tcp_transport::TcpTransportServer;
use super::transport::InProcessTransport;

/// A server that hosts services and listens for remote dispatch requests over TCP.
pub struct ServiceServer {
    addr: String,
    transport: Option<InProcessTransport>,
    event_bus: EventBus,
}

impl ServiceServer {
    /// Create a new server bound to the given address.
    pub fn new(addr: &str) -> Self {
        Self {
            addr: addr.to_string(),
            transport: None,
            event_bus: EventBus::new(),
        }
    }

    /// Set the transport layer (runtime + substrate registry).
    pub fn with_transport(
        mut self,
        runtime: ServiceRuntime,
        substrate_registry: SubstrateRegistry,
    ) -> Self {
        self.transport = Some(InProcessTransport::new(runtime, substrate_registry));
        self
    }

    /// The address this server is configured to bind to.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Create a ServiceDescriptor for a service hosted by this server.
    pub fn hosted_descriptor(
        &self,
        name: &str,
        port: &str,
        publish_id: &str,
    ) -> ServiceDescriptor {
        ServiceDescriptor {
            name: name.to_string(),
            port: port.to_string(),
            publish_id: publish_id.to_string(),
            guarantees: Vec::new(),
            location: ServiceLocation::Remote,
            profile_tags: Vec::new(),
            replication_factor: None,
            transport_endpoint: Some(self.addr.clone()),
            version: None,
            deprecated: false,
            descriptor_version: 0,
        }
    }

    /// Start serving requests. Runs until the process is stopped.
    pub async fn serve(self) -> Result<(), String> {
        let transport = self
            .transport
            .ok_or("no transport configured for server")?;
        let mut server = TcpTransportServer::new(transport, self.event_bus);
        server.serve(&self.addr).await
    }

    /// Serve a fixed number of requests then return. Used for testing.
    pub async fn serve_n(self, n: usize) -> Result<(), String> {
        let transport = self
            .transport
            .ok_or("no transport configured for server")?;
        let mut server = TcpTransportServer::new(transport, self.event_bus);
        server.serve_n(&self.addr, n).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_creation() {
        let server = ServiceServer::new("127.0.0.1:9000");
        assert_eq!(server.addr(), "127.0.0.1:9000");
    }

    #[test]
    fn test_hosted_descriptor() {
        let server = ServiceServer::new("127.0.0.1:9000");
        let desc = server.hosted_descriptor("MySvc", "KV", "kv/main");
        assert_eq!(desc.name, "MySvc");
        assert_eq!(desc.port, "KV");
        assert_eq!(desc.publish_id, "kv/main");
        assert_eq!(desc.location, ServiceLocation::Remote);
        assert_eq!(
            desc.transport_endpoint,
            Some("127.0.0.1:9000".to_string())
        );
    }
}
