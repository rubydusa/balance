use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::registry::{Registry, ServiceDescriptor, Profile};

/// Trait for blockchain-backed service directory providers.
pub trait ChainRegistryProvider: Send + Sync {
    /// Look up a service by port and publish ID.
    fn lookup_service(&self, port: &str, publish_id: &str) -> Result<Option<ServiceDescriptor>, String>;
    /// Register a service on-chain.
    fn register_service(&self, descriptor: &ServiceDescriptor) -> Result<(), String>;
    /// List all services providing a given port.
    fn list_services(&self, port: &str) -> Result<Vec<ServiceDescriptor>, String>;
}

/// A cache entry with a timestamp for TTL-based expiration.
struct CacheEntry {
    descriptor: ServiceDescriptor,
    cached_at: Instant,
}

/// A registry backed by a blockchain provider with a local cache.
pub struct ChainBackedRegistry {
    provider: Box<dyn ChainRegistryProvider>,
    cache: HashMap<String, CacheEntry>,
    profiles: HashMap<String, Profile>,
    /// Time-to-live for cache entries. Default: 60 seconds.
    cache_ttl: Duration,
}

impl ChainBackedRegistry {
    pub fn new(provider: Box<dyn ChainRegistryProvider>) -> Self {
        Self {
            provider,
            cache: HashMap::new(),
            profiles: HashMap::new(),
            cache_ttl: Duration::from_secs(60),
        }
    }

    /// Create a registry with a custom cache TTL.
    pub fn with_ttl(provider: Box<dyn ChainRegistryProvider>, ttl: Duration) -> Self {
        Self {
            provider,
            cache: HashMap::new(),
            profiles: HashMap::new(),
            cache_ttl: ttl,
        }
    }

    /// Refresh the local cache from the chain provider for a given port.
    pub fn refresh_cache(&mut self, port: &str) -> Result<(), String> {
        let services = self.provider.list_services(port)?;
        let now = Instant::now();
        for svc in services {
            self.cache.insert(svc.publish_id.clone(), CacheEntry {
                descriptor: svc,
                cached_at: now,
            });
        }
        Ok(())
    }

    /// Check if a cache entry for the given port is stale and refresh if so.
    /// Returns `Ok(true)` if a refresh was performed, `Ok(false)` if cache was fresh.
    pub fn refresh_if_stale(&mut self, port: &str) -> Result<bool, String> {
        let now = Instant::now();
        let any_stale = self.cache.values()
            .filter(|e| e.descriptor.port == port)
            .any(|e| now.duration_since(e.cached_at) >= self.cache_ttl);
        if any_stale {
            self.refresh_cache(port)?;
            Ok(true)
        } else {
            // If no entries at all, also refresh
            let has_any = self.cache.values().any(|e| e.descriptor.port == port);
            if !has_any {
                self.refresh_cache(port)?;
                Ok(true)
            } else {
                Ok(false)
            }
        }
    }

    fn is_fresh(&self, entry: &CacheEntry) -> bool {
        Instant::now().duration_since(entry.cached_at) < self.cache_ttl
    }
}

impl Registry for ChainBackedRegistry {
    fn register(&mut self, descriptor: ServiceDescriptor) {
        // Register both on-chain and locally
        let _ = self.provider.register_service(&descriptor);
        let publish_id = descriptor.publish_id.clone();
        self.cache.insert(publish_id, CacheEntry {
            descriptor,
            cached_at: Instant::now(),
        });
    }

    fn lookup(&self, port: &str, publish_id: &str) -> Option<&ServiceDescriptor> {
        self.cache
            .get(publish_id)
            .filter(|e| self.is_fresh(e))
            .filter(|e| e.descriptor.port == port || port.is_empty())
            .map(|e| &e.descriptor)
    }

    fn find_by_port(&self, port: &str) -> Vec<&ServiceDescriptor> {
        self.cache.values()
            .filter(|e| self.is_fresh(e))
            .filter(|e| e.descriptor.port == port)
            .map(|e| &e.descriptor)
            .collect()
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

/// File-backed chain provider using NDJSON journal for persistence.
/// On construction, replays the journal to populate an in-memory cache.
/// Writes append to the journal and update the cache atomically.
pub struct FileBackedChainProvider {
    cache: std::sync::Mutex<HashMap<String, ServiceDescriptor>>,
    path: PathBuf,
}

impl FileBackedChainProvider {
    /// Open or create a file-backed chain registry at the given path.
    /// Replays the NDJSON journal to populate the in-memory cache.
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut cache = HashMap::new();

        if path.exists() {
            let file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line.map_err(|e| format!("read {}: {e}", path.display()))?;
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                let desc: ServiceDescriptor = serde_json::from_str(&line)
                    .map_err(|e| format!("parse journal entry: {e}"))?;
                cache.insert(desc.publish_id.clone(), desc);
            }
        }

        Ok(Self {
            cache: std::sync::Mutex::new(cache),
            path: path.to_path_buf(),
        })
    }
}

impl ChainRegistryProvider for FileBackedChainProvider {
    fn lookup_service(&self, port: &str, publish_id: &str) -> Result<Option<ServiceDescriptor>, String> {
        let cache = self.cache.lock().map_err(|e| e.to_string())?;
        Ok(cache
            .get(publish_id)
            .filter(|d| d.port == port || port.is_empty())
            .cloned())
    }

    fn register_service(&self, descriptor: &ServiceDescriptor) -> Result<(), String> {
        // Append to journal
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("open journal: {e}"))?;
        let json = serde_json::to_string(descriptor)
            .map_err(|e| format!("serialize descriptor: {e}"))?;
        writeln!(file, "{json}").map_err(|e| format!("write journal: {e}"))?;
        file.flush().map_err(|e| format!("flush journal: {e}"))?;
        // Update in-memory cache
        let mut cache = self.cache.lock().map_err(|e| e.to_string())?;
        cache.insert(descriptor.publish_id.clone(), descriptor.clone());
        Ok(())
    }

    fn list_services(&self, port: &str) -> Result<Vec<ServiceDescriptor>, String> {
        let cache = self.cache.lock().map_err(|e| e.to_string())?;
        Ok(cache.values().filter(|d| d.port == port).cloned().collect())
    }
}

/// Mock chain provider for testing.
pub struct MockChainProvider {
    services: std::sync::Mutex<HashMap<String, ServiceDescriptor>>,
}

impl MockChainProvider {
    pub fn new() -> Self {
        Self {
            services: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl ChainRegistryProvider for MockChainProvider {
    fn lookup_service(&self, port: &str, publish_id: &str) -> Result<Option<ServiceDescriptor>, String> {
        let services = self.services.lock().map_err(|e| e.to_string())?;
        Ok(services
            .get(publish_id)
            .filter(|d| d.port == port || port.is_empty())
            .cloned())
    }

    fn register_service(&self, descriptor: &ServiceDescriptor) -> Result<(), String> {
        let mut services = self.services.lock().map_err(|e| e.to_string())?;
        services.insert(descriptor.publish_id.clone(), descriptor.clone());
        Ok(())
    }

    fn list_services(&self, port: &str) -> Result<Vec<ServiceDescriptor>, String> {
        let services = self.services.lock().map_err(|e| e.to_string())?;
        Ok(services.values().filter(|d| d.port == port).cloned().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use super::super::registry::ServiceLocation;

    #[test]
    fn test_mock_provider_register_and_lookup() {
        let provider = MockChainProvider::new();
        let desc = ServiceDescriptor::simple("KV", "KV", "kv/main");
        provider.register_service(&desc).unwrap();

        let result = provider.lookup_service("KV", "kv/main").unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().publish_id, "kv/main");
    }

    #[test]
    fn test_mock_provider_list_services() {
        let provider = MockChainProvider::new();
        provider.register_service(&ServiceDescriptor::simple("KV", "KV", "kv/1")).unwrap();
        provider.register_service(&ServiceDescriptor::simple("KV", "KV", "kv/2")).unwrap();
        provider.register_service(&ServiceDescriptor::simple("Queue", "Queue", "q/1")).unwrap();

        let kv_services = provider.list_services("KV").unwrap();
        assert_eq!(kv_services.len(), 2);

        let q_services = provider.list_services("Queue").unwrap();
        assert_eq!(q_services.len(), 1);
    }

    #[test]
    fn test_chain_backed_registry() {
        let provider = MockChainProvider::new();
        let mut registry = ChainBackedRegistry::new(Box::new(provider));

        registry.register(ServiceDescriptor::simple("KV", "KV", "kv/main"));
        assert!(registry.lookup("KV", "kv/main").is_some());
        assert_eq!(registry.find_by_port("KV").len(), 1);
    }

    #[test]
    fn test_cache_refresh() {
        let provider = MockChainProvider::new();
        // Pre-populate the mock provider
        provider.register_service(&ServiceDescriptor::simple("KV", "KV", "kv/1")).unwrap();
        provider.register_service(&ServiceDescriptor::simple("KV", "KV", "kv/2")).unwrap();

        let mut registry = ChainBackedRegistry::new(Box::new(provider));
        // Cache is empty initially
        assert_eq!(registry.find_by_port("KV").len(), 0);

        // After refresh, cache is populated
        registry.refresh_cache("KV").unwrap();
        assert_eq!(registry.find_by_port("KV").len(), 2);
    }

    #[test]
    fn test_chain_registry_profiles() {
        let provider = MockChainProvider::new();
        let mut registry = ChainBackedRegistry::new(Box::new(provider));

        registry.register_profile(Profile::new("test_profile")).unwrap();
        assert!(registry.get_profile("test_profile").is_some());
        assert!(registry.get_profile("nonexistent").is_none());
    }

    #[test]
    fn test_file_backed_register_and_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chain.ndjson");

        let provider = FileBackedChainProvider::open(&path).unwrap();
        let desc = ServiceDescriptor::simple("KV", "KV", "kv/main");
        provider.register_service(&desc).unwrap();

        let result = provider.lookup_service("KV", "kv/main").unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().publish_id, "kv/main");
    }

    #[test]
    fn test_file_backed_persistence_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chain.ndjson");

        // First session: register services
        {
            let provider = FileBackedChainProvider::open(&path).unwrap();
            provider
                .register_service(&ServiceDescriptor::simple("KV", "KV", "kv/1"))
                .unwrap();
            provider
                .register_service(&ServiceDescriptor::simple("KV", "KV", "kv/2"))
                .unwrap();
        }

        // Second session: reopen and verify
        {
            let provider = FileBackedChainProvider::open(&path).unwrap();
            assert!(provider.lookup_service("KV", "kv/1").unwrap().is_some());
            assert!(provider.lookup_service("KV", "kv/2").unwrap().is_some());
            assert!(provider.lookup_service("KV", "kv/3").unwrap().is_none());
        }
    }

    #[test]
    fn test_file_backed_list_by_port() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chain.ndjson");

        let provider = FileBackedChainProvider::open(&path).unwrap();
        provider
            .register_service(&ServiceDescriptor::simple("KV", "KV", "kv/1"))
            .unwrap();
        provider
            .register_service(&ServiceDescriptor::simple("KV", "KV", "kv/2"))
            .unwrap();
        provider
            .register_service(&ServiceDescriptor::simple("Queue", "Queue", "q/1"))
            .unwrap();

        let kv_services = provider.list_services("KV").unwrap();
        assert_eq!(kv_services.len(), 2);

        let q_services = provider.list_services("Queue").unwrap();
        assert_eq!(q_services.len(), 1);
    }

    // === Gap 12: Cache TTL ===

    #[test]
    fn test_cache_ttl_expiration() {
        let provider = MockChainProvider::new();
        // Use zero TTL so entries expire immediately
        let mut registry = ChainBackedRegistry::with_ttl(
            Box::new(provider),
            Duration::from_millis(0),
        );

        registry.register(ServiceDescriptor::simple("KV", "KV", "kv/main"));
        // Immediately stale due to 0ms TTL
        std::thread::sleep(Duration::from_millis(1));
        assert!(registry.lookup("KV", "kv/main").is_none());
        assert_eq!(registry.find_by_port("KV").len(), 0);
    }

    #[test]
    fn test_cache_fresh_no_refresh() {
        let provider = MockChainProvider::new();
        let mut registry = ChainBackedRegistry::with_ttl(
            Box::new(provider),
            Duration::from_secs(3600), // 1 hour TTL
        );
        registry.register(ServiceDescriptor::simple("KV", "KV", "kv/main"));
        // Should be fresh
        assert!(registry.lookup("KV", "kv/main").is_some());
        // refresh_if_stale should return false (no refresh needed)
        let refreshed = registry.refresh_if_stale("KV").unwrap();
        assert!(!refreshed);
    }

    #[test]
    fn test_cache_stale_triggers_refresh() {
        let provider = MockChainProvider::new();
        // Pre-populate chain with a service
        provider.register_service(&ServiceDescriptor::simple("KV", "KV", "kv/main")).unwrap();

        let mut registry = ChainBackedRegistry::with_ttl(
            Box::new(provider),
            Duration::from_millis(0), // instant expiry
        );

        // Force populate cache via refresh
        registry.refresh_cache("KV").unwrap();
        std::thread::sleep(Duration::from_millis(1));

        // Cache is stale → refresh_if_stale should refresh and return true
        let refreshed = registry.refresh_if_stale("KV").unwrap();
        assert!(refreshed);
    }

    #[test]
    fn test_descriptor_version_tracking() {
        let desc = ServiceDescriptor::simple("KV", "KV", "kv/main");
        assert_eq!(desc.descriptor_version, 0);
        // Serialize and deserialize — version should round-trip
        let json = serde_json::to_string(&desc).unwrap();
        let desc2: ServiceDescriptor = serde_json::from_str(&json).unwrap();
        assert_eq!(desc2.descriptor_version, 0);
    }

    #[test]
    fn test_file_backed_with_chain_backed_registry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chain.ndjson");

        // First session: register via ChainBackedRegistry
        {
            let provider = FileBackedChainProvider::open(&path).unwrap();
            let mut registry = ChainBackedRegistry::new(Box::new(provider));
            registry.register(ServiceDescriptor::simple("KV", "KV", "kv/main"));
            assert!(registry.lookup("KV", "kv/main").is_some());
        }

        // Second session: reopen, refresh cache, verify
        {
            let provider = FileBackedChainProvider::open(&path).unwrap();
            let mut registry = ChainBackedRegistry::new(Box::new(provider));
            // Registry cache is empty, but provider cache is populated from journal
            registry.refresh_cache("KV").unwrap();
            assert!(registry.lookup("KV", "kv/main").is_some());
        }
    }
}
