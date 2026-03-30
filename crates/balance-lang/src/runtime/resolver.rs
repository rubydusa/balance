use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::capability::CapabilityRef;
use super::registry::{Registry, ServiceDescriptor, ServiceLocation};

#[cfg(test)]
use super::registry::ServiceRegistry;

/// A record of a resolution decision, for observability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionDecision {
    pub port: String,
    pub publish_id: String,
    pub profile: Option<String>,
    pub candidates_found: usize,
    pub candidates_after_filter: usize,
    pub selected_service: String,
}

#[derive(Debug)]
pub struct Resolver {
    decisions: Vec<ResolutionDecision>,
    /// Services (by publish_id) that have violated their guarantees.
    guarantee_failures: HashSet<String>,
}

impl Resolver {
    pub fn new() -> Self {
        Self {
            decisions: Vec::new(),
            guarantee_failures: HashSet::new(),
        }
    }

    /// Record that a service violated its guarantees.
    pub fn record_guarantee_failure(&mut self, publish_id: &str) {
        self.guarantee_failures.insert(publish_id.to_string());
    }

    /// Check if a service has known guarantee violations.
    pub fn has_guarantee_failure(&self, publish_id: &str) -> bool {
        self.guarantee_failures.contains(publish_id)
    }

    /// Get all services with guarantee failures.
    pub fn guarantee_failures(&self) -> &HashSet<String> {
        &self.guarantee_failures
    }

    /// Get all resolution decisions made so far.
    pub fn decisions(&self) -> &[ResolutionDecision] {
        &self.decisions
    }

    /// Full resolution algorithm:
    /// 1. Find all services providing the requested port
    /// 2. Filter by publish_id/name if specified
    /// 3. Filter by profile preferences (locality, guarantees)
    /// 4. Rank candidates by profile preference ordering
    /// 5. Select best candidate
    /// 6. Bind and return CapabilityRef
    pub fn resolve(
        &mut self,
        registry: &dyn Registry,
        port: &str,
        publish_id: &str,
        profile: Option<&str>,
    ) -> Result<CapabilityRef, String> {
        // Step 1: Find candidates
        let mut candidates: Vec<&ServiceDescriptor> = if !publish_id.is_empty() {
            // Exact lookup by publish_id
            match registry.lookup(port, publish_id) {
                Some(desc) => vec![desc],
                None => Vec::new(),
            }
        } else {
            // Find all services providing this port
            registry.find_by_port(port)
        };

        if candidates.is_empty() {
            if !publish_id.is_empty() {
                // Explicit publish_id was given but not found — hard error.
                // Never fall back to a different service silently.
                return Err(format!(
                    "no service found for port '{port}' with id '{publish_id}'"
                ));
            }
            return Err(format!(
                "no service found for port '{port}'"
            ));
        }

        // Step 2-4: Apply profile filtering
        if let Some(profile_name) = profile {
            if let Some(profile) = registry.get_profile(profile_name) {
                // Filter by structured locality preference
                match profile.locality {
                    super::registry::LocalityPreference::Local => {
                        let local: Vec<_> = candidates
                            .iter()
                            .filter(|d| d.location == ServiceLocation::Local)
                            .copied()
                            .collect();
                        if local.is_empty() {
                            return Err(format!(
                                "no service for port '{}' satisfies profile '{}' locality requirement (local)",
                                port, profile_name
                            ));
                        }
                        candidates = local;
                    }
                    super::registry::LocalityPreference::Remote => {
                        let remote: Vec<_> = candidates
                            .iter()
                            .filter(|d| d.location == ServiceLocation::Remote)
                            .copied()
                            .collect();
                        if !remote.is_empty() {
                            candidates = remote;
                        }
                        // If no remote services, fall through to all candidates
                    }
                    super::registry::LocalityPreference::Any => {}
                }

                // Filter by trust domain
                if let Some(ref domain) = profile.trust_domain {
                    let trusted: Vec<_> = candidates
                        .iter()
                        .filter(|d| d.profile_tags.contains(domain))
                        .copied()
                        .collect();
                    if trusted.is_empty() {
                        return Err(format!(
                            "no service for port '{}' satisfies profile '{}' trust domain '{}'",
                            port, profile_name, domain
                        ));
                    }
                    candidates = trusted;
                }

                // Filter by required guarantees (general mechanism)
                if !profile.required_guarantees.is_empty() {
                    let required: Vec<_> = candidates
                        .iter()
                        .filter(|d| {
                            profile.required_guarantees.iter().all(|rg| {
                                d.guarantees.contains(rg)
                            })
                        })
                        .copied()
                        .collect();
                    if required.is_empty() {
                        return Err(format!(
                            "no service for port '{}' satisfies profile '{}' required guarantees ({:?})",
                            port, profile_name, profile.required_guarantees
                        ));
                    }
                    candidates = required;
                } else if let Some(durability) = profile.preferences.get("durability") {
                    // Legacy fallback: durability=strong check (only when required_guarantees is empty)
                    if durability == "strong" {
                        let strong: Vec<_> = candidates
                            .iter()
                            .filter(|d| {
                                d.guarantees
                                    .iter()
                                    .any(|g| g == "commit_requires_accept" || g == "committed")
                            })
                            .copied()
                            .collect();
                        if strong.is_empty() {
                            return Err(format!(
                                "no service for port '{}' satisfies profile '{}' durability requirement (strong guarantees)",
                                port, profile_name
                            ));
                        }
                        candidates = strong;
                    }
                }
            }
        }

        // Exclude services with known guarantee violations
        candidates.retain(|c| !self.guarantee_failures.contains(&c.publish_id));

        // Step 4b: Sort by version (highest first), deprioritize deprecated
        candidates.sort_by(|a, b| {
            // Non-deprecated before deprecated
            let dep_order = a.deprecated.cmp(&b.deprecated);
            if dep_order != std::cmp::Ordering::Equal {
                return dep_order;
            }
            // Higher version first (reverse order)
            let va = a.version.as_deref().unwrap_or("0.0.0");
            let vb = b.version.as_deref().unwrap_or("0.0.0");
            compare_version_strings(vb, va)
        });

        let candidates_after_filter = candidates.len();

        // Step 5: Select best candidate (first match after sorting)
        let descriptor = candidates.first().ok_or_else(|| {
            format!("no service found for port '{port}' matching profile criteria")
        })?;

        // Warn if selected service is deprecated
        if descriptor.deprecated {
            eprintln!(
                "warning: selected service '{}' is deprecated",
                descriptor.publish_id
            );
        }

        // Record the resolution decision
        self.decisions.push(ResolutionDecision {
            port: port.to_string(),
            publish_id: publish_id.to_string(),
            profile: profile.map(|s| s.to_string()),
            candidates_found: candidates_after_filter, // after all filtering
            candidates_after_filter,
            selected_service: descriptor.publish_id.clone(),
        });

        // Step 6: Bind and return (include transport endpoint if remote)
        Ok(CapabilityRef::new_with_endpoint(
            descriptor.port.clone(),
            descriptor.publish_id.clone(),
            descriptor.transport_endpoint.clone(),
        ))
    }
}

/// Compare two version strings lexicographically by numeric segments.
/// Supports "1.2.3", "1.2", "1" formats. Missing segments treated as 0.
fn compare_version_strings(a: &str, b: &str) -> std::cmp::Ordering {
    let parse_segments = |s: &str| -> Vec<u64> {
        s.split('.')
            .map(|seg| seg.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let sa = parse_segments(a);
    let sb = parse_segments(b);
    let max_len = sa.len().max(sb.len());
    for i in 0..max_len {
        let va = sa.get(i).copied().unwrap_or(0);
        let vb = sb.get(i).copied().unwrap_or(0);
        match va.cmp(&vb) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::registry::{Profile, ServiceDescriptor, ServiceLocation};

    #[test]
    fn test_basic_resolution() {
        let mut reg = ServiceRegistry::new();
        reg.register(ServiceDescriptor {
            name: "MySvc".into(),
            port: "KV".into(),
            publish_id: "kv/main".into(),
            guarantees: vec![],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: None,
            version: None,
            deprecated: false,
            descriptor_version: 0,
        });

        let mut resolver = Resolver::new();
        let cap = resolver.resolve(&reg, "KV", "kv/main", None).unwrap();
        assert_eq!(cap.port_name(), "KV");
        assert_eq!(cap.service_id(), "kv/main");
    }

    #[test]
    fn test_resolution_by_port_only() {
        let mut reg = ServiceRegistry::new();
        reg.register(ServiceDescriptor {
            name: "MySvc".into(),
            port: "KV".into(),
            publish_id: "kv/main".into(),
            guarantees: vec![],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: None,
            version: None,
            deprecated: false,
            descriptor_version: 0,
        });

        let mut resolver = Resolver::new();
        let cap = resolver.resolve(&reg, "KV", "", None).unwrap();
        assert_eq!(cap.service_id(), "kv/main");
    }

    #[test]
    fn test_profile_prefers_local() {
        let mut reg = ServiceRegistry::new();
        reg.register(ServiceDescriptor {
            name: "LocalSvc".into(),
            port: "KV".into(),
            publish_id: "kv/local".into(),
            guarantees: vec![],
            location: ServiceLocation::Local,
            profile_tags: vec!["local".into()],
            replication_factor: None,
            transport_endpoint: None,
            version: None,
            deprecated: false,
            descriptor_version: 0,
        });
        reg.register(ServiceDescriptor {
            name: "RemoteSvc".into(),
            port: "KV".into(),
            publish_id: "kv/remote".into(),
            guarantees: vec![],
            location: ServiceLocation::Remote,
            profile_tags: vec!["remote".into()],
            replication_factor: None,
            transport_endpoint: None,
            version: None,
            deprecated: false,
            descriptor_version: 0,
        });

        let mut resolver = Resolver::new();
        let cap = resolver
            .resolve(&reg, "KV", "", Some("local_fast"))
            .unwrap();
        assert_eq!(cap.service_id(), "kv/local");
    }

    #[test]
    fn test_profile_prefers_strong_guarantees() {
        let mut reg = ServiceRegistry::new();
        reg.register(ServiceDescriptor {
            name: "WeakSvc".into(),
            port: "KV".into(),
            publish_id: "kv/weak".into(),
            guarantees: vec![],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: None,
            version: None,
            deprecated: false,
            descriptor_version: 0,
        });
        reg.register(ServiceDescriptor {
            name: "StrongSvc".into(),
            port: "KV".into(),
            publish_id: "kv/strong".into(),
            guarantees: vec!["commit_requires_accept".into()],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: None,
            version: None,
            deprecated: false,
            descriptor_version: 0,
        });

        let mut resolver = Resolver::new();
        let cap = resolver
            .resolve(&reg, "KV", "", Some("public_strong"))
            .unwrap();
        assert_eq!(cap.service_id(), "kv/strong");
    }

    #[test]
    fn test_resolution_not_found() {
        let reg = ServiceRegistry::new();
        let mut resolver = Resolver::new();
        let result = resolver.resolve(&reg, "NonExistent", "nope", None);
        assert!(result.is_err());
    }

    #[test]
    fn test_required_guarantees_general() {
        let mut reg = ServiceRegistry::new();
        reg.register(ServiceDescriptor {
            name: "FullSvc".into(),
            port: "KV".into(),
            publish_id: "kv/full".into(),
            guarantees: vec!["ordering".into(), "durability".into()],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: None,
            version: None,
            deprecated: false,
            descriptor_version: 0,
        });

        let custom = Profile::new("custom")
            .with_required_guarantee("ordering")
            .with_required_guarantee("durability");
        reg.register_profile(custom).unwrap();

        let mut resolver = Resolver::new();
        let cap = resolver.resolve(&reg, "KV", "", Some("custom")).unwrap();
        assert_eq!(cap.service_id(), "kv/full");
    }

    #[test]
    fn test_required_guarantees_subset_fails() {
        let mut reg = ServiceRegistry::new();
        reg.register(ServiceDescriptor {
            name: "PartialSvc".into(),
            port: "KV".into(),
            publish_id: "kv/partial".into(),
            guarantees: vec!["ordering".into()],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: None,
            version: None,
            deprecated: false,
            descriptor_version: 0,
        });

        let custom = Profile::new("strict")
            .with_required_guarantee("ordering")
            .with_required_guarantee("durability");
        reg.register_profile(custom).unwrap();

        let mut resolver = Resolver::new();
        let result = resolver.resolve(&reg, "KV", "", Some("strict"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("required guarantees"),
            "expected guarantee error, got: {err}"
        );
    }

    #[test]
    fn test_public_strong_uses_required_guarantees() {
        // Existing public_strong profile should still work via the new mechanism
        let mut reg = ServiceRegistry::new();
        reg.register(ServiceDescriptor {
            name: "StrongSvc".into(),
            port: "KV".into(),
            publish_id: "kv/strong".into(),
            guarantees: vec!["commit_requires_accept".into()],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: None,
            version: None,
            deprecated: false,
            descriptor_version: 0,
        });
        reg.register(ServiceDescriptor {
            name: "WeakSvc".into(),
            port: "KV".into(),
            publish_id: "kv/weak".into(),
            guarantees: vec![],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: None,
            version: None,
            deprecated: false,
            descriptor_version: 0,
        });

        let mut resolver = Resolver::new();
        let cap = resolver
            .resolve(&reg, "KV", "", Some("public_strong"))
            .unwrap();
        assert_eq!(cap.service_id(), "kv/strong");
    }

    #[test]
    fn test_guarantee_failure_excludes_service() {
        let mut reg = ServiceRegistry::new();
        reg.register(ServiceDescriptor {
            name: "Svc1".into(),
            port: "KV".into(),
            publish_id: "kv/primary".into(),
            guarantees: vec![],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: None,
            version: None,
            deprecated: false,
            descriptor_version: 0,
        });
        reg.register(ServiceDescriptor {
            name: "Svc2".into(),
            port: "KV".into(),
            publish_id: "kv/backup".into(),
            guarantees: vec![],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: None,
            version: None,
            deprecated: false,
            descriptor_version: 0,
        });

        let mut resolver = Resolver::new();
        // First resolution picks one of the two candidates
        let cap = resolver.resolve(&reg, "KV", "", None).unwrap();
        let first_selected = cap.service_id().to_string();
        let other = if first_selected == "kv/primary" {
            "kv/backup"
        } else {
            "kv/primary"
        };

        // Record guarantee failure for the selected service
        resolver.record_guarantee_failure(&first_selected);

        // Subsequent resolution must pick the other service
        let cap2 = resolver.resolve(&reg, "KV", "", None).unwrap();
        assert_eq!(cap2.service_id(), other);
    }

    #[test]
    fn test_no_guarantee_failure_resolves_normally() {
        let mut reg = ServiceRegistry::new();
        reg.register(ServiceDescriptor {
            name: "Svc1".into(),
            port: "KV".into(),
            publish_id: "kv/main".into(),
            guarantees: vec![],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: None,
            version: None,
            deprecated: false,
            descriptor_version: 0,
        });

        let mut resolver = Resolver::new();
        // Record failure for a different service
        resolver.record_guarantee_failure("kv/other");

        // kv/main should still resolve fine
        let cap = resolver.resolve(&reg, "KV", "", None).unwrap();
        assert_eq!(cap.service_id(), "kv/main");
    }

    #[test]
    fn test_version_parsed_from_publish_id() {
        let (id, ver) = ServiceDescriptor::parse_versioned_id("kv/main@1.2.0");
        assert_eq!(id, "kv/main");
        assert_eq!(ver, Some("1.2.0"));
    }

    #[test]
    fn test_version_parsing_no_version() {
        let (id, ver) = ServiceDescriptor::parse_versioned_id("kv/main");
        assert_eq!(id, "kv/main");
        assert_eq!(ver, None);
    }

    #[test]
    fn test_resolver_prefers_higher_version() {
        let mut reg = ServiceRegistry::new();
        reg.register(ServiceDescriptor {
            name: "SvcV1".into(),
            port: "KV".into(),
            publish_id: "kv/main".into(),
            guarantees: vec![],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: None,
            version: Some("1.0.0".into()),
            deprecated: false,
            descriptor_version: 0,
        });
        reg.register(ServiceDescriptor {
            name: "SvcV2".into(),
            port: "KV".into(),
            publish_id: "kv/main-v2".into(),
            guarantees: vec![],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: None,
            version: Some("2.0.0".into()),
            deprecated: false,
            descriptor_version: 0,
        });

        let mut resolver = Resolver::new();
        let cap = resolver.resolve(&reg, "KV", "", None).unwrap();
        assert_eq!(cap.service_id(), "kv/main-v2");
    }

    #[test]
    fn test_no_version_still_resolves() {
        let mut reg = ServiceRegistry::new();
        reg.register(ServiceDescriptor {
            name: "Svc".into(),
            port: "KV".into(),
            publish_id: "kv/main".into(),
            guarantees: vec![],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: None,
            version: None,
            deprecated: false,
            descriptor_version: 0,
        });

        let mut resolver = Resolver::new();
        let cap = resolver.resolve(&reg, "KV", "kv/main", None).unwrap();
        assert_eq!(cap.service_id(), "kv/main");
    }

    #[test]
    fn test_deprecated_service_deprioritized() {
        let mut reg = ServiceRegistry::new();
        reg.register(ServiceDescriptor {
            name: "OldSvc".into(),
            port: "KV".into(),
            publish_id: "kv/old".into(),
            guarantees: vec![],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: None,
            version: Some("2.0.0".into()),
            deprecated: true,
            descriptor_version: 0,
        });
        reg.register(ServiceDescriptor {
            name: "NewSvc".into(),
            port: "KV".into(),
            publish_id: "kv/new".into(),
            guarantees: vec![],
            location: ServiceLocation::Local,
            profile_tags: vec![],
            replication_factor: None,
            transport_endpoint: None,
            version: Some("1.0.0".into()),
            deprecated: false,
            descriptor_version: 0,
        });

        let mut resolver = Resolver::new();
        // Even though kv/old has higher version, it's deprecated
        let cap = resolver.resolve(&reg, "KV", "", None).unwrap();
        assert_eq!(cap.service_id(), "kv/new");
    }

    #[test]
    fn test_gossip_announcement_includes_version() {
        let ann = super::super::gossip_registry::ServiceAnnouncement {
            name: "KV".into(),
            port: "KV".into(),
            publish_id: "kv/main".into(),
            endpoint: "127.0.0.1:9000".into(),
            guarantees: vec![],
            version: Some("1.5.0".into()),
        };
        let json = serde_json::to_string(&ann).unwrap();
        assert!(json.contains("1.5.0"));
        let decoded: super::super::gossip_registry::ServiceAnnouncement =
            serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.version, Some("1.5.0".into()));
    }

    #[test]
    fn test_trust_domain_filtering() {
        use crate::runtime::registry::{Profile, ServiceRegistry};

        let mut registry = ServiceRegistry::new();
        let mut trusted = ServiceDescriptor::simple("TrustedKV", "KV", "kv/trusted");
        trusted.profile_tags = vec!["internal".to_string()];
        registry.register(trusted);

        let mut untrusted = ServiceDescriptor::simple("UntrustedKV", "KV", "kv/untrusted");
        untrusted.profile_tags = vec!["external".to_string()];
        registry.register(untrusted);

        registry.register_profile(
            Profile::new("secure")
                .with_preference("trust_domain", "internal"),
        ).unwrap();

        let mut resolver = Resolver::new();
        let result = resolver.resolve(&registry, "KV", "", Some("secure"));
        assert!(result.is_ok());
        let cap = result.unwrap();
        assert_eq!(cap.service_id(), "kv/trusted");
    }

    #[test]
    fn test_trust_domain_no_match() {
        use crate::runtime::registry::{Profile, ServiceRegistry};

        let mut registry = ServiceRegistry::new();
        let mut svc = ServiceDescriptor::simple("KVSvc", "KV", "kv/main");
        svc.profile_tags = vec!["external".to_string()];
        registry.register(svc);

        registry.register_profile(
            Profile::new("strict")
                .with_preference("trust_domain", "internal"),
        ).unwrap();

        let mut resolver = Resolver::new();
        let result = resolver.resolve(&registry, "KV", "", Some("strict"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("trust domain"));
    }

    #[test]
    fn test_locality_local_filtering() {
        use crate::runtime::registry::{Profile, ServiceRegistry};

        let mut registry = ServiceRegistry::new();
        let local_svc = ServiceDescriptor::simple("LocalKV", "KV", "kv/local");
        registry.register(local_svc);

        let mut remote_svc = ServiceDescriptor::simple("RemoteKV", "KV", "kv/remote");
        remote_svc.location = ServiceLocation::Remote;
        registry.register(remote_svc);

        registry.register_profile(
            Profile::new("local_only")
                .with_preference("locality", "local"),
        ).unwrap();

        let mut resolver = Resolver::new();
        let result = resolver.resolve(&registry, "KV", "", Some("local_only"));
        assert!(result.is_ok());
        let cap = result.unwrap();
        assert_eq!(cap.service_id(), "kv/local");
    }
}
