use std::collections::HashMap;

use super::registry::{
    Profile, Registry, ServiceDescriptor, ServiceLocation, ServiceRegistry,
};

/// A registry that combines a local registry with remote service
/// descriptors loaded from configuration (JSON/TOML).
///
/// For distributed deployments, see also GossipRegistry and ChainBackedRegistry.
pub struct NetworkedRegistry {
    local: ServiceRegistry,
    remote_descriptors: Vec<ServiceDescriptor>,
}

impl NetworkedRegistry {
    pub fn new() -> Self {
        Self {
            local: ServiceRegistry::new(),
            remote_descriptors: Vec::new(),
        }
    }

    /// Add a remote service from configuration.
    pub fn add_remote(
        &mut self,
        name: &str,
        port: &str,
        publish_id: &str,
        endpoint: &str,
    ) {
        let desc = ServiceDescriptor {
            name: name.to_string(),
            port: port.to_string(),
            publish_id: publish_id.to_string(),
            guarantees: Vec::new(),
            location: ServiceLocation::Remote,
            profile_tags: Vec::new(),
            replication_factor: None,
            transport_endpoint: Some(endpoint.to_string()),
            version: None,
            deprecated: false,
            descriptor_version: 0,
        };
        self.remote_descriptors.push(desc.clone());
        self.local.register(desc);
    }

    /// Load remote services from a JSON configuration string.
    /// Format: [{ "name": "...", "port": "...", "publish_id": "...", "endpoint": "..." }]
    pub fn load_from_json(&mut self, json: &str) -> Result<usize, String> {
        let entries: Vec<HashMap<String, String>> =
            serde_json::from_str(json).map_err(|e| format!("invalid JSON: {e}"))?;
        let mut count = 0;
        for entry in &entries {
            let name = entry.get("name").ok_or("missing 'name'")?;
            let port = entry.get("port").ok_or("missing 'port'")?;
            let publish_id = entry.get("publish_id").ok_or("missing 'publish_id'")?;
            let endpoint = entry.get("endpoint").ok_or("missing 'endpoint'")?;
            self.add_remote(name, port, publish_id, endpoint);
            count += 1;
        }
        Ok(count)
    }

    /// Get the number of remote descriptors loaded.
    pub fn remote_count(&self) -> usize {
        self.remote_descriptors.len()
    }
}

impl Registry for NetworkedRegistry {
    fn register(&mut self, descriptor: ServiceDescriptor) {
        self.local.register(descriptor);
    }

    fn lookup(&self, port: &str, publish_id: &str) -> Option<&ServiceDescriptor> {
        self.local.lookup(port, publish_id)
    }

    fn find_by_port(&self, port: &str) -> Vec<&ServiceDescriptor> {
        self.local.find_by_port(port)
    }

    fn get_profile(&self, name: &str) -> Option<&Profile> {
        self.local.get_profile(name)
    }

    fn register_profile(&mut self, profile: Profile) -> Result<(), String> {
        self.local.register_profile(profile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_networked_registry_local() {
        let mut reg = NetworkedRegistry::new();
        reg.register(ServiceDescriptor::simple("KV", "KV", "kv/main"));
        assert!(reg.lookup("KV", "kv/main").is_some());
    }

    #[test]
    fn test_networked_registry_add_remote() {
        let mut reg = NetworkedRegistry::new();
        reg.add_remote("RemoteKV", "KV", "kv/remote", "10.0.0.1:9000");
        let desc = reg.lookup("KV", "kv/remote").unwrap();
        assert_eq!(desc.location, ServiceLocation::Remote);
        assert_eq!(
            desc.transport_endpoint.as_deref(),
            Some("10.0.0.1:9000")
        );
        assert_eq!(reg.remote_count(), 1);
    }

    #[test]
    fn test_networked_registry_load_json() {
        let json = r#"[
            {"name": "Svc1", "port": "KV", "publish_id": "kv/1", "endpoint": "host1:9000"},
            {"name": "Svc2", "port": "KV", "publish_id": "kv/2", "endpoint": "host2:9000"}
        ]"#;
        let mut reg = NetworkedRegistry::new();
        let count = reg.load_from_json(json).unwrap();
        assert_eq!(count, 2);
        assert_eq!(reg.remote_count(), 2);
        assert!(reg.lookup("KV", "kv/1").is_some());
        assert!(reg.lookup("KV", "kv/2").is_some());
    }

    #[test]
    fn test_networked_registry_find_by_port() {
        let mut reg = NetworkedRegistry::new();
        reg.register(ServiceDescriptor::simple("LocalKV", "KV", "kv/local"));
        reg.add_remote("RemoteKV", "KV", "kv/remote", "host:9000");

        let all = reg.find_by_port("KV");
        assert_eq!(all.len(), 2);
    }
}
