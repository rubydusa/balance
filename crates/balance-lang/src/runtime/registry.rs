use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Trait abstraction for service registries.
/// Implementations: local (ServiceRegistry), networked, gossip, chain-backed.
pub trait Registry {
    fn register(&mut self, descriptor: ServiceDescriptor);
    fn lookup(&self, port: &str, publish_id: &str) -> Option<&ServiceDescriptor>;
    fn find_by_port(&self, port: &str) -> Vec<&ServiceDescriptor>;
    fn get_profile(&self, name: &str) -> Option<&Profile>;
    fn register_profile(&mut self, profile: Profile) -> Result<(), String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ServiceLocation {
    Local,
    Remote, // placeholder for future networking
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDescriptor {
    pub name: String,
    pub port: String,
    pub publish_id: String,
    pub guarantees: Vec<String>,
    pub location: ServiceLocation,
    pub profile_tags: Vec<String>,
    pub replication_factor: Option<u32>,
    /// Optional transport endpoint for remote services (e.g., "127.0.0.1:9000").
    pub transport_endpoint: Option<String>,
    /// Semantic version string (e.g., "1.2.0"). Parsed from publish_id if it contains `@`.
    #[serde(default)]
    pub version: Option<String>,
    /// Whether this service is deprecated. Deprecated services are resolved with lower priority.
    #[serde(default)]
    pub deprecated: bool,
    /// Monotonically increasing version for cache invalidation.
    #[serde(default)]
    pub descriptor_version: u64,
}

impl ServiceDescriptor {
    /// Simple constructor with just name/port/publish_id (backwards compat).
    pub fn simple(name: &str, port: &str, publish_id: &str) -> Self {
        Self {
            name: name.to_string(),
            port: port.to_string(),
            publish_id: publish_id.to_string(),
            guarantees: Vec::new(),
            location: ServiceLocation::Local,
            profile_tags: Vec::new(),
            replication_factor: None,
            transport_endpoint: None,
            version: None,
            deprecated: false,
            descriptor_version: 0,
        }
    }

    /// Parse version from publish_id if it contains `@` (e.g., "kv/main@1.2").
    /// Returns (publish_id_without_version, optional_version).
    pub fn parse_versioned_id(raw_id: &str) -> (&str, Option<&str>) {
        if let Some(at_pos) = raw_id.rfind('@') {
            (&raw_id[..at_pos], Some(&raw_id[at_pos + 1..]))
        } else {
            (raw_id, None)
        }
    }
}

/// Transport preference for profile-based routing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransportPreference {
    Tcp,
    Quic,
    Any,
}

/// Locality preference for profile-based resolution.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LocalityPreference {
    /// Prefer local services; fail if none available.
    Local,
    /// Prefer remote services.
    Remote,
    /// No preference (default).
    Any,
}

/// Profile for resolution preferences.
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub preferences: HashMap<String, String>,
    pub required_guarantees: Vec<String>,
    pub preferred_transport: TransportPreference,
    pub locality: LocalityPreference,
    pub trust_domain: Option<String>,
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub cache_ttl_ms: Option<u64>,
}

impl Profile {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            preferences: HashMap::new(),
            required_guarantees: Vec::new(),
            preferred_transport: TransportPreference::Any,
            locality: LocalityPreference::Any,
            trust_domain: None,
            timeout_ms: None,
            max_retries: None,
            cache_ttl_ms: None,
        }
    }

    pub fn with_preference(mut self, key: &str, value: &str) -> Self {
        self.preferences.insert(key.to_string(), value.to_string());
        // Parse known preference keys
        match key {
            "transport" => {
                self.preferred_transport = match value {
                    "tcp" => TransportPreference::Tcp,
                    "quic" => TransportPreference::Quic,
                    _ => TransportPreference::Any,
                };
            }
            "locality" => {
                self.locality = match value {
                    "local" => LocalityPreference::Local,
                    "remote" => LocalityPreference::Remote,
                    "prefer_local" => LocalityPreference::Local,
                    _ => LocalityPreference::Any,
                };
            }
            "trust_domain" => {
                self.trust_domain = Some(value.to_string());
            }
            "timeout" => {
                self.timeout_ms = value.parse().ok();
            }
            "max_retries" => {
                self.max_retries = value.parse().ok();
            }
            "cache_ttl" => {
                self.cache_ttl_ms = value.parse().ok();
            }
            _ => {}
        }
        self
    }

    pub fn with_required_guarantee(mut self, guarantee: &str) -> Self {
        self.required_guarantees.push(guarantee.to_string());
        self
    }
}

/// Built-in profiles.
pub fn default_profiles() -> HashMap<String, Profile> {
    let mut profiles = HashMap::new();
    profiles.insert(
        "local_fast".to_string(),
        Profile::new("local_fast")
            .with_preference("locality", "prefer_local")
            .with_preference("durability", "weak"),
    );
    profiles.insert(
        "public_strong".to_string(),
        Profile::new("public_strong")
            .with_preference("locality", "any")
            .with_preference("durability", "strong")
            .with_required_guarantee("commit_requires_accept"),
    );
    profiles
}

#[derive(Debug, Default)]
pub struct ServiceRegistry {
    services: HashMap<String, ServiceDescriptor>,
    profiles: HashMap<String, Profile>,
}

impl ServiceRegistry {
    pub fn new() -> Self {
        Self {
            services: HashMap::new(),
            profiles: default_profiles(),
        }
    }

    pub fn register(&mut self, descriptor: ServiceDescriptor) {
        self.services
            .insert(descriptor.publish_id.clone(), descriptor);
    }

    pub fn register_profile(&mut self, profile: Profile) -> Result<(), String> {
        let name = profile.name.clone();
        // Allow overwriting built-in default profiles
        let defaults = default_profiles();
        if self.profiles.contains_key(&name) && !defaults.contains_key(&name) {
            return Err(format!("profile '{}' is already defined", name));
        }
        self.profiles.insert(name, profile);
        Ok(())
    }

    pub fn get_profile(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    pub fn lookup(&self, port: &str, publish_id: &str) -> Option<&ServiceDescriptor> {
        self.services
            .get(publish_id)
            .filter(|d| d.port == port || port.is_empty())
    }

    pub fn find_by_port(&self, port: &str) -> Vec<&ServiceDescriptor> {
        self.services
            .values()
            .filter(|d| d.port == port)
            .collect()
    }

    pub fn all_services(&self) -> impl Iterator<Item = &ServiceDescriptor> {
        self.services.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_transport_preference() {
        let profile = Profile::new("test")
            .with_preference("transport", "quic");
        assert_eq!(profile.preferred_transport, TransportPreference::Quic);
    }

    #[test]
    fn test_profile_timeout() {
        let profile = Profile::new("test")
            .with_preference("timeout", "10000");
        assert_eq!(profile.timeout_ms, Some(10000));
    }

    #[test]
    fn test_profile_max_retries() {
        let profile = Profile::new("test")
            .with_preference("max_retries", "7");
        assert_eq!(profile.max_retries, Some(7));
    }

    #[test]
    fn test_profile_cache_ttl() {
        let profile = Profile::new("test")
            .with_preference("cache_ttl", "5000");
        assert_eq!(profile.cache_ttl_ms, Some(5000));
    }

    #[test]
    fn test_profile_default_transport() {
        let profile = Profile::new("test");
        assert_eq!(profile.preferred_transport, TransportPreference::Any);
    }

    #[test]
    fn test_profile_locality_local() {
        let profile = Profile::new("test")
            .with_preference("locality", "local");
        assert_eq!(profile.locality, LocalityPreference::Local);
    }

    #[test]
    fn test_profile_locality_remote() {
        let profile = Profile::new("test")
            .with_preference("locality", "remote");
        assert_eq!(profile.locality, LocalityPreference::Remote);
    }

    #[test]
    fn test_profile_locality_default() {
        let profile = Profile::new("test");
        assert_eq!(profile.locality, LocalityPreference::Any);
    }

    #[test]
    fn test_profile_trust_domain() {
        let profile = Profile::new("test")
            .with_preference("trust_domain", "internal");
        assert_eq!(profile.trust_domain, Some("internal".to_string()));
    }

    #[test]
    fn test_profile_duplicate_rejected() {
        let mut reg = ServiceRegistry::new();
        reg.register_profile(Profile::new("custom")).unwrap();
        let result = reg.register_profile(Profile::new("custom"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already defined"));
    }

    #[test]
    fn test_profile_overwrite_builtin_ok() {
        let mut reg = ServiceRegistry::new();
        // "local_fast" is a built-in default — overwrite should succeed
        let result = reg.register_profile(Profile::new("local_fast"));
        assert!(result.is_ok());
    }
}

impl Registry for ServiceRegistry {
    fn register(&mut self, descriptor: ServiceDescriptor) {
        self.register(descriptor);
    }

    fn lookup(&self, port: &str, publish_id: &str) -> Option<&ServiceDescriptor> {
        self.lookup(port, publish_id)
    }

    fn find_by_port(&self, port: &str) -> Vec<&ServiceDescriptor> {
        self.find_by_port(port)
    }

    fn get_profile(&self, name: &str) -> Option<&Profile> {
        self.get_profile(name)
    }

    fn register_profile(&mut self, profile: Profile) -> Result<(), String> {
        self.register_profile(profile)
    }
}
