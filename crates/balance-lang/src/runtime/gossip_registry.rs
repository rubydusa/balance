use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use serde::{Serialize, Deserialize};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

use super::registry::{Registry, ServiceDescriptor, Profile, ServiceLocation};

/// SWIM-like gossip protocol messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GossipMessage {
    Ping { sender: String },
    Ack { sender: String },
    PingReq { target: String, sender: String },
    Announce { services: Vec<ServiceAnnouncement> },
    Leave { sender: String },
}

/// A service announcement for gossip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAnnouncement {
    pub name: String,
    pub port: String,
    pub publish_id: String,
    pub endpoint: String,
    pub guarantees: Vec<String>,
    #[serde(default)]
    pub version: Option<String>,
}

/// Member health state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MemberState {
    Alive,
    Suspect,
    Dead,
}

/// A known member in the gossip cluster.
#[derive(Debug, Clone)]
pub struct Member {
    pub addr: SocketAddr,
    pub state: MemberState,
    pub last_seen: std::time::Instant,
    pub services: Vec<ServiceAnnouncement>,
}

/// Gossip-based service registry.
pub struct GossipRegistry {
    local_services: HashMap<String, ServiceDescriptor>,
    profiles: HashMap<String, Profile>,
    members: HashMap<String, Member>,
    local_addr: Option<SocketAddr>,
    /// Optional channel for notifying external systems (e.g., evaluator) of newly discovered services.
    discovery_tx: Option<tokio::sync::mpsc::UnboundedSender<ServiceDescriptor>>,
}

impl GossipRegistry {
    pub fn new() -> Self {
        Self {
            local_services: HashMap::new(),
            profiles: HashMap::new(),
            members: HashMap::new(),
            local_addr: None,
            discovery_tx: None,
        }
    }

    pub fn with_addr(addr: SocketAddr) -> Self {
        Self {
            local_services: HashMap::new(),
            profiles: HashMap::new(),
            members: HashMap::new(),
            local_addr: Some(addr),
            discovery_tx: None,
        }
    }

    /// Set the discovery channel for notifying external systems of new services.
    pub fn set_discovery_tx(&mut self, tx: tokio::sync::mpsc::UnboundedSender<ServiceDescriptor>) {
        self.discovery_tx = Some(tx);
    }

    /// Process an incoming gossip message and return optional response.
    pub fn handle_message(&mut self, from: SocketAddr, msg: GossipMessage) -> Option<GossipMessage> {
        match msg {
            GossipMessage::Ping { sender } => {
                self.update_member(&sender, from, &[]);
                let local_id = self.local_id();
                Some(GossipMessage::Ack { sender: local_id })
            }
            GossipMessage::Ack { sender } => {
                self.update_member(&sender, from, &[]);
                None
            }
            GossipMessage::PingReq { target: _, sender } => {
                self.update_member(&sender, from, &[]);
                // In a full implementation, would forward ping to target
                None
            }
            GossipMessage::Announce { services } => {
                // Merge announced services
                let member_id = from.to_string();
                for svc in &services {
                    let is_new = !self.local_services.contains_key(&svc.publish_id);
                    let descriptor = ServiceDescriptor {
                        name: svc.name.clone(),
                        port: svc.port.clone(),
                        publish_id: svc.publish_id.clone(),
                        guarantees: svc.guarantees.clone(),
                        location: ServiceLocation::Remote,
                        profile_tags: Vec::new(),
                        replication_factor: None,
                        transport_endpoint: Some(svc.endpoint.clone()),
                        version: svc.version.clone(),
                        deprecated: false,
                        descriptor_version: 0,
                    };
                    if is_new {
                        if let Some(ref tx) = self.discovery_tx {
                            let _ = tx.send(descriptor.clone());
                        }
                    }
                    self.local_services.insert(svc.publish_id.clone(), descriptor);
                }
                self.update_member(&member_id, from, &services);
                None
            }
            GossipMessage::Leave { sender } => {
                if let Some(member) = self.members.get_mut(&sender) {
                    member.state = MemberState::Dead;
                }
                None
            }
        }
    }

    /// Create announcement messages for local services.
    pub fn create_announcement(&self) -> GossipMessage {
        let services: Vec<ServiceAnnouncement> = self.local_services
            .values()
            .filter(|s| s.location == ServiceLocation::Local)
            .map(|s| ServiceAnnouncement {
                name: s.name.clone(),
                port: s.port.clone(),
                publish_id: s.publish_id.clone(),
                endpoint: s.transport_endpoint.clone().unwrap_or_default(),
                guarantees: s.guarantees.clone(),
                version: s.version.clone(),
            })
            .collect();
        GossipMessage::Announce { services }
    }

    /// Get alive members.
    pub fn alive_members(&self) -> Vec<&Member> {
        self.members.values()
            .filter(|m| m.state == MemberState::Alive)
            .collect()
    }

    fn local_id(&self) -> String {
        self.local_addr
            .map(|a| a.to_string())
            .unwrap_or_else(|| "local".to_string())
    }

    fn update_member(&mut self, id: &str, addr: SocketAddr, services: &[ServiceAnnouncement]) {
        let member = self.members.entry(id.to_string()).or_insert_with(|| Member {
            addr,
            state: MemberState::Alive,
            last_seen: std::time::Instant::now(),
            services: Vec::new(),
        });
        member.state = MemberState::Alive;
        member.last_seen = std::time::Instant::now();
        member.addr = addr;
        if !services.is_empty() {
            member.services = services.to_vec();
        }
    }

    /// Mark members as suspect/dead based on timeout.
    pub fn check_timeouts(&mut self, suspect_timeout: std::time::Duration, dead_timeout: std::time::Duration) {
        let now = std::time::Instant::now();
        for member in self.members.values_mut() {
            let elapsed = now.duration_since(member.last_seen);
            if elapsed > dead_timeout {
                member.state = MemberState::Dead;
            } else if elapsed > suspect_timeout {
                member.state = MemberState::Suspect;
            }
        }
    }

    /// Remove services from members that have been marked as dead.
    pub fn purge_dead_members(&mut self) {
        let dead_ids: Vec<String> = self.members.iter()
            .filter(|(_, m)| m.state == MemberState::Dead)
            .map(|(id, _)| id.clone())
            .collect();

        for id in &dead_ids {
            if let Some(member) = self.members.get(id) {
                for svc in &member.services {
                    // Only remove if it was from a remote member
                    if let Some(desc) = self.local_services.get(&svc.publish_id) {
                        if desc.location == ServiceLocation::Remote {
                            self.local_services.remove(&svc.publish_id);
                        }
                    }
                }
            }
        }
    }

    /// Get seed nodes for initial cluster join.
    pub fn seed_nodes(&self) -> Vec<SocketAddr> {
        self.members.values()
            .filter(|m| m.state == MemberState::Alive)
            .map(|m| m.addr)
            .collect()
    }
}

/// Handle to a running gossip network loop.
pub struct GossipHandle {
    shutdown: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl GossipHandle {
    /// Signal the gossip loop to stop and wait for it to finish.
    pub async fn stop(self) {
        let _ = self.shutdown.send(true);
        let _ = self.task.await;
    }
}

/// Start a gossip network loop for the given registry.
/// Returns a handle that can be used to stop the loop.
pub async fn start_gossip(
    registry: Arc<Mutex<GossipRegistry>>,
    bind_addr: SocketAddr,
    seed_nodes: Vec<SocketAddr>,
) -> Result<GossipHandle, String> {
    let socket = UdpSocket::bind(bind_addr)
        .await
        .map_err(|e| format!("failed to bind gossip UDP socket on {bind_addr}: {e}"))?;
    let actual_addr = socket.local_addr().map_err(|e| e.to_string())?;

    // Update the registry's local_addr
    {
        let mut reg = registry.lock().await;
        reg.local_addr = Some(actual_addr);
    }

    let socket = Arc::new(socket);
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    // Send initial pings to seed nodes
    {
        let reg = registry.lock().await;
        let local_id = reg.local_id();
        let ping = GossipMessage::Ping { sender: local_id };
        let data = serde_json::to_vec(&ping).unwrap_or_default();
        for seed in &seed_nodes {
            let _ = socket.send_to(&data, seed).await;
        }
    }

    let task = {
        let socket = Arc::clone(&socket);
        let registry = Arc::clone(&registry);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65536];
            let mut tick_interval = tokio::time::interval(std::time::Duration::from_millis(500));

            loop {
                tokio::select! {
                    // Check shutdown
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            // Send Leave message to all known members
                            let reg = registry.lock().await;
                            let local_id = reg.local_id();
                            let leave = GossipMessage::Leave { sender: local_id };
                            let data = serde_json::to_vec(&leave).unwrap_or_default();
                            for member in reg.alive_members() {
                                let _ = socket.send_to(&data, member.addr).await;
                            }
                            break;
                        }
                    }
                    // Receive messages
                    result = socket.recv_from(&mut buf) => {
                        match result {
                            Ok((len, from)) => {
                                if let Ok(msg) = serde_json::from_slice::<GossipMessage>(&buf[..len]) {
                                    let mut reg = registry.lock().await;
                                    if let Some(response) = reg.handle_message(from, msg) {
                                        let data = serde_json::to_vec(&response).unwrap_or_default();
                                        let _ = socket.send_to(&data, from).await;
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    // Periodic tick: ping random member, check timeouts
                    _ = tick_interval.tick() => {
                        let mut reg = registry.lock().await;
                        // Check timeouts
                        reg.check_timeouts(
                            std::time::Duration::from_secs(2),
                            std::time::Duration::from_secs(5),
                        );
                        reg.purge_dead_members();

                        // Pick a random alive member and send ping
                        let alive = reg.alive_members();
                        if !alive.is_empty() {
                            let idx = (std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_nanos() as usize) % alive.len();
                            let target = alive[idx].addr;
                            let local_id = reg.local_id();
                            let ping = GossipMessage::Ping { sender: local_id };
                            let data = serde_json::to_vec(&ping).unwrap_or_default();
                            drop(reg); // release lock before await
                            let _ = socket.send_to(&data, target).await;
                        }
                    }
                }
            }
        })
    };

    Ok(GossipHandle { shutdown: shutdown_tx, task })
}

/// Broadcast a service announcement to all alive members.
pub async fn announce_service(
    registry: &Arc<Mutex<GossipRegistry>>,
    socket: &UdpSocket,
) {
    let reg = registry.lock().await;
    let announcement = reg.create_announcement();
    let data = serde_json::to_vec(&announcement).unwrap_or_default();
    for member in reg.alive_members() {
        let _ = socket.send_to(&data, member.addr).await;
    }
}

impl Registry for GossipRegistry {
    fn register(&mut self, descriptor: ServiceDescriptor) {
        self.local_services.insert(descriptor.publish_id.clone(), descriptor);
    }

    fn lookup(&self, port: &str, publish_id: &str) -> Option<&ServiceDescriptor> {
        self.local_services
            .get(publish_id)
            .filter(|d| d.port == port || port.is_empty())
    }

    fn find_by_port(&self, port: &str) -> Vec<&ServiceDescriptor> {
        self.local_services.values().filter(|d| d.port == port).collect()
    }

    fn get_profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    fn register_profile(&mut self, profile: Profile) -> Result<(), String> {
        let name = profile.name.clone();
        if self.profiles.contains_key(&name) {
            return Err(format!("profile '{}' is already defined", name));
        }
        self.profiles.insert(name, profile);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gossip_message_serde() {
        let msg = GossipMessage::Ping { sender: "node1".into() };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: GossipMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            GossipMessage::Ping { sender } => assert_eq!(sender, "node1"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_announce_serde() {
        let msg = GossipMessage::Announce {
            services: vec![ServiceAnnouncement {
                name: "KVService".into(),
                port: "KV".into(),
                publish_id: "kv/main".into(),
                endpoint: "127.0.0.1:9000".into(),
                guarantees: vec!["commit_requires_accept".into()],
                version: None,
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: GossipMessage = serde_json::from_str(&json).unwrap();
        match decoded {
            GossipMessage::Announce { services } => {
                assert_eq!(services.len(), 1);
                assert_eq!(services[0].publish_id, "kv/main");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_ping_ack_flow() {
        let addr1: SocketAddr = "127.0.0.1:8001".parse().unwrap();
        let addr2: SocketAddr = "127.0.0.1:8002".parse().unwrap();

        let mut _registry1 = GossipRegistry::with_addr(addr1);
        let mut registry2 = GossipRegistry::with_addr(addr2);

        // Node1 sends ping to node2
        let ping = GossipMessage::Ping { sender: addr1.to_string() };
        let response = registry2.handle_message(addr1, ping);

        // Node2 should respond with Ack
        assert!(response.is_some());
        match response.unwrap() {
            GossipMessage::Ack { sender } => assert_eq!(sender, addr2.to_string()),
            _ => panic!("expected Ack"),
        }

        // Node2 should know about node1
        assert_eq!(registry2.alive_members().len(), 1);
    }

    #[test]
    fn test_service_announcement() {
        let addr1: SocketAddr = "127.0.0.1:8001".parse().unwrap();
        let addr2: SocketAddr = "127.0.0.1:8002".parse().unwrap();

        let mut registry1 = GossipRegistry::with_addr(addr1);
        let mut registry2 = GossipRegistry::with_addr(addr2);

        // Register a service on node1
        registry1.register(ServiceDescriptor {
            name: "KVService".into(),
            port: "KV".into(),
            publish_id: "kv/main".into(),
            guarantees: vec![],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: Some(addr1.to_string()),
            version: None,
            deprecated: false,
            descriptor_version: 0,
        });

        // Create and send announcement
        let announcement = registry1.create_announcement();
        registry2.handle_message(addr1, announcement);

        // Node2 should now know about the service
        assert!(registry2.lookup("KV", "kv/main").is_some());
    }

    #[test]
    fn test_registry_trait_implementation() {
        let mut reg = GossipRegistry::new();
        reg.register(ServiceDescriptor::simple("KV", "KV", "kv/main"));
        assert!(reg.lookup("KV", "kv/main").is_some());
        assert!(reg.lookup("KV", "kv/other").is_none());
        assert_eq!(reg.find_by_port("KV").len(), 1);
    }

    #[test]
    fn test_leave_message() {
        let addr: SocketAddr = "127.0.0.1:8001".parse().unwrap();
        let mut registry = GossipRegistry::new();

        // First ping to register member
        registry.handle_message(addr, GossipMessage::Ping { sender: "node1".into() });
        assert_eq!(registry.alive_members().len(), 1);

        // Then leave
        registry.handle_message(addr, GossipMessage::Leave { sender: "node1".into() });
        assert_eq!(registry.alive_members().len(), 0);
    }

    #[test]
    fn test_purge_dead_members() {
        let addr: SocketAddr = "127.0.0.1:8001".parse().unwrap();
        let mut registry = GossipRegistry::with_addr("127.0.0.1:8000".parse().unwrap());

        // Register remote service via announcement
        let announcement = GossipMessage::Announce {
            services: vec![ServiceAnnouncement {
                name: "KVService".into(),
                port: "KV".into(),
                publish_id: "kv/remote".into(),
                endpoint: "127.0.0.1:9001".into(),
                guarantees: vec![],
                version: None,
            }],
        };
        registry.handle_message(addr, announcement);
        assert!(registry.lookup("KV", "kv/remote").is_some());

        // Mark member as dead
        registry.handle_message(addr, GossipMessage::Leave { sender: addr.to_string() });
        registry.purge_dead_members();

        // Remote service should be removed
        assert!(registry.lookup("KV", "kv/remote").is_none());
    }

    #[test]
    fn test_check_timeouts() {
        let addr: SocketAddr = "127.0.0.1:8001".parse().unwrap();
        let mut registry = GossipRegistry::new();

        // Register member
        registry.handle_message(addr, GossipMessage::Ping { sender: "node1".into() });
        assert_eq!(registry.alive_members().len(), 1);

        // Check with very short timeout — should mark as dead immediately
        // (since last_seen is in the past)
        registry.check_timeouts(
            std::time::Duration::from_nanos(1),
            std::time::Duration::from_nanos(1),
        );

        // Should be dead now
        assert_eq!(registry.alive_members().len(), 0);
    }

    #[test]
    fn test_seed_nodes() {
        let addr1: SocketAddr = "127.0.0.1:8001".parse().unwrap();
        let addr2: SocketAddr = "127.0.0.1:8002".parse().unwrap();
        let mut registry = GossipRegistry::new();

        registry.handle_message(addr1, GossipMessage::Ping { sender: "node1".into() });
        registry.handle_message(addr2, GossipMessage::Ping { sender: "node2".into() });

        let seeds = registry.seed_nodes();
        assert_eq!(seeds.len(), 2);
        assert!(seeds.contains(&addr1));
        assert!(seeds.contains(&addr2));
    }

    #[tokio::test]
    async fn test_gossip_udp_two_nodes() {
        // Start two gossip registries on random ports
        let reg1 = Arc::new(Mutex::new(GossipRegistry::new()));
        let reg2 = Arc::new(Mutex::new(GossipRegistry::new()));

        let addr1: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let addr2: SocketAddr = "127.0.0.1:0".parse().unwrap();

        // Start node1 with no seeds
        let handle1 = start_gossip(Arc::clone(&reg1), addr1, vec![]).await.unwrap();
        let actual_addr1 = {
            let reg = reg1.lock().await;
            reg.local_addr.unwrap()
        };

        // Start node2 with node1 as seed
        let handle2 = start_gossip(Arc::clone(&reg2), addr2, vec![actual_addr1]).await.unwrap();

        // Give time for ping/ack exchange
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Node1 should know about node2
        {
            let reg = reg1.lock().await;
            assert!(reg.alive_members().len() >= 1, "node1 should know about node2");
        }

        // Node2 should know about node1
        {
            let reg = reg2.lock().await;
            assert!(reg.alive_members().len() >= 1, "node2 should know about node1");
        }

        handle1.stop().await;
        handle2.stop().await;
    }

    #[tokio::test]
    async fn test_gossip_udp_service_discovery() {
        let reg1 = Arc::new(Mutex::new(GossipRegistry::new()));
        let reg2 = Arc::new(Mutex::new(GossipRegistry::new()));

        let addr1: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let addr2: SocketAddr = "127.0.0.1:0".parse().unwrap();

        // Start both nodes
        let handle1 = start_gossip(Arc::clone(&reg1), addr1, vec![]).await.unwrap();
        let actual_addr1 = {
            let reg = reg1.lock().await;
            reg.local_addr.unwrap()
        };

        let handle2 = start_gossip(Arc::clone(&reg2), addr2, vec![actual_addr1]).await.unwrap();
        let actual_addr2 = {
            let reg = reg2.lock().await;
            reg.local_addr.unwrap()
        };

        // Wait for connectivity
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Register a service on node1
        {
            let mut reg = reg1.lock().await;
            reg.register(ServiceDescriptor {
                name: "KVService".into(),
                port: "KV".into(),
                publish_id: "kv/main".into(),
                guarantees: vec![],
                location: ServiceLocation::Local,
                profile_tags: vec![],
                replication_factor: None,
                transport_endpoint: Some(actual_addr1.to_string()),
                version: None,
                deprecated: false,
                descriptor_version: 0,
            });
        }

        // Send announcement from node1 to node2 directly via UDP
        let announce_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        {
            let reg = reg1.lock().await;
            let announcement = reg.create_announcement();
            let data = serde_json::to_vec(&announcement).unwrap();
            announce_socket.send_to(&data, actual_addr2).await.unwrap();
        }

        // Wait for message processing
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Node2 should now be able to look up the service
        {
            let reg = reg2.lock().await;
            let found = reg.lookup("KV", "kv/main");
            assert!(found.is_some(), "node2 should have discovered kv/main");
            assert_eq!(found.unwrap().transport_endpoint, Some(actual_addr1.to_string()));
        }

        handle1.stop().await;
        handle2.stop().await;
    }

    #[tokio::test]
    async fn test_gossip_udp_member_failure_detection() {
        let reg1 = Arc::new(Mutex::new(GossipRegistry::new()));
        let reg2 = Arc::new(Mutex::new(GossipRegistry::new()));

        let addr1: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let addr2: SocketAddr = "127.0.0.1:0".parse().unwrap();

        let handle1 = start_gossip(Arc::clone(&reg1), addr1, vec![]).await.unwrap();
        let actual_addr1 = {
            let reg = reg1.lock().await;
            reg.local_addr.unwrap()
        };

        let handle2 = start_gossip(Arc::clone(&reg2), addr2, vec![actual_addr1]).await.unwrap();

        // Wait for connectivity
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Verify node1 sees node2
        {
            let reg = reg1.lock().await;
            assert!(reg.alive_members().len() >= 1);
        }

        // Stop node2
        handle2.stop().await;

        // Wait for failure detection (timeouts set to 2s/5s in the loop)
        // Use aggressive manual timeout check instead of waiting
        {
            let mut reg = reg1.lock().await;
            reg.check_timeouts(
                std::time::Duration::from_nanos(1),
                std::time::Duration::from_nanos(1),
            );
            reg.purge_dead_members();
            assert_eq!(reg.alive_members().len(), 0, "node2 should be detected as dead");
        }

        handle1.stop().await;
    }

    #[test]
    fn test_discovery_channel_notifies_new_services() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let addr: SocketAddr = "127.0.0.1:8001".parse().unwrap();
        let mut registry = GossipRegistry::with_addr("127.0.0.1:8000".parse().unwrap());
        registry.set_discovery_tx(tx);

        // Announce a new service
        let announcement = GossipMessage::Announce {
            services: vec![ServiceAnnouncement {
                name: "KVService".into(),
                port: "KV".into(),
                publish_id: "kv/remote".into(),
                endpoint: "127.0.0.1:9001".into(),
                guarantees: vec![],
                version: None,
            }],
        };
        registry.handle_message(addr, announcement);

        // Channel should have the discovered service
        let desc = rx.try_recv().unwrap();
        assert_eq!(desc.publish_id, "kv/remote");
        assert_eq!(desc.port, "KV");
    }

    #[test]
    fn test_discovery_channel_no_duplicate_notifications() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let addr: SocketAddr = "127.0.0.1:8001".parse().unwrap();
        let mut registry = GossipRegistry::with_addr("127.0.0.1:8000".parse().unwrap());
        registry.set_discovery_tx(tx);

        let announcement = GossipMessage::Announce {
            services: vec![ServiceAnnouncement {
                name: "KVService".into(),
                port: "KV".into(),
                publish_id: "kv/remote".into(),
                endpoint: "127.0.0.1:9001".into(),
                guarantees: vec![],
                version: None,
            }],
        };

        // First announcement
        registry.handle_message(addr, announcement.clone());
        assert!(rx.try_recv().is_ok());

        // Second announcement (same service) — should NOT notify again
        registry.handle_message(addr, announcement);
        assert!(rx.try_recv().is_err(), "duplicate announcement should not trigger discovery");
    }

    #[test]
    fn test_gossip_registry_as_evaluator_registry() {
        let mut registry = GossipRegistry::new();
        registry.register(ServiceDescriptor::simple("KV", "KV", "kv/main"));

        // Test via Registry trait
        let reg: &dyn Registry = &registry;
        assert!(reg.lookup("KV", "kv/main").is_some());
        assert_eq!(reg.find_by_port("KV").len(), 1);
    }
}
