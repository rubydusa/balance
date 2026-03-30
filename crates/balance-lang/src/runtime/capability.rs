use std::sync::atomic::{AtomicU64, Ordering};

use hmac::{Hmac, Mac};
use rand::random;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::ast::AuthorityQualifier;

type HmacSha256 = Hmac<Sha256>;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CapabilityId(u64);

impl CapabilityId {
    pub(crate) fn new() -> Self {
        Self(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

impl std::fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "cap#{}", self.0)
    }
}

/// How a capability was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum CapabilityOrigin {
    /// Obtained via `resolve` expression
    Resolved,
    /// Injected into an entry point
    EntryInjected,
    /// Passed as a parameter to a function/service
    ParameterPassed,
}

impl std::fmt::Display for CapabilityOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CapabilityOrigin::Resolved => write!(f, "resolved"),
            CapabilityOrigin::EntryInjected => write!(f, "entry-injected"),
            CapabilityOrigin::ParameterPassed => write!(f, "parameter-passed"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityRef {
    id: CapabilityId,
    port_name: String,
    service_id: String,
    /// Random nonce checked at dispatch to prevent forgery.
    token: u64,
    /// How this capability was obtained.
    origin: CapabilityOrigin,
    /// Optional transport endpoint for remote services (e.g., "127.0.0.1:9000").
    endpoint: Option<String>,
    /// Optional HMAC-SHA256 signature for cross-boundary capability verification.
    signature: Option<Vec<u8>>,
    /// Authority qualifier for this capability reference.
    authority: Option<AuthorityQualifier>,
}

impl CapabilityRef {
    pub(crate) fn new(port_name: String, service_id: String) -> Self {
        Self {
            id: CapabilityId::new(),
            port_name,
            service_id,
            token: random::<u64>(),
            origin: CapabilityOrigin::Resolved,
            endpoint: None,
            signature: None,
            authority: None,
        }
    }

    pub(crate) fn new_with_origin(
        port_name: String,
        service_id: String,
        origin: CapabilityOrigin,
    ) -> Self {
        Self {
            id: CapabilityId::new(),
            port_name,
            service_id,
            token: random::<u64>(),
            origin,
            endpoint: None,
            signature: None,
            authority: None,
        }
    }

    pub(crate) fn new_with_endpoint(
        port_name: String,
        service_id: String,
        endpoint: Option<String>,
    ) -> Self {
        Self {
            id: CapabilityId::new(),
            port_name,
            service_id,
            token: random::<u64>(),
            origin: CapabilityOrigin::Resolved,
            endpoint,
            signature: None,
            authority: None,
        }
    }

    /// Create a signed capability using HMAC-SHA256 over (id, port_name, service_id, token).
    pub(crate) fn new_signed(port_name: String, service_id: String, key: &[u8]) -> Self {
        let id = CapabilityId::new();
        let token = random::<u64>();
        let message = format!("{}:{}:{}:{}", id.0, port_name, service_id, token);
        let mut mac =
            HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
        mac.update(message.as_bytes());
        let sig = mac.finalize().into_bytes().to_vec();
        Self {
            id,
            port_name,
            service_id,
            token,
            origin: CapabilityOrigin::Resolved,
            endpoint: None,
            signature: Some(sig),
            authority: None,
        }
    }

    pub fn id(&self) -> CapabilityId {
        self.id
    }

    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    pub fn token(&self) -> u64 {
        self.token
    }

    pub fn origin(&self) -> CapabilityOrigin {
        self.origin
    }

    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }

    /// Get the authority qualifier for this capability.
    pub fn authority(&self) -> Option<AuthorityQualifier> {
        self.authority
    }

    /// Set the authority qualifier for this capability.
    pub(crate) fn set_authority(&mut self, authority: Option<AuthorityQualifier>) {
        self.authority = authority;
    }

    /// Check if this capability can be serialized across boundaries.
    /// Only `@delegate` capabilities can be sent over the wire.
    pub fn can_serialize(&self) -> bool {
        match self.authority {
            Some(AuthorityQualifier::Delegate) => true,
            None => true, // No authority qualifier = no restriction
            _ => false,   // @consume and @borrow cannot be serialized
        }
    }

    /// Verify the authority token matches.
    pub fn verify_token(&self, expected_token: u64) -> bool {
        self.token == expected_token
    }

    /// Get the cryptographic signature, if present.
    pub fn signature(&self) -> Option<&[u8]> {
        self.signature.as_deref()
    }

    /// Verify the HMAC-SHA256 signature against a signing key.
    pub fn verify_signature(&self, key: &[u8]) -> bool {
        let Some(ref sig) = self.signature else {
            return false;
        };
        let message = format!("{}:{}:{}:{}", self.id.0, self.port_name, self.service_id, self.token);
        let mut mac =
            HmacSha256::new_from_slice(key).expect("HMAC can take key of any size");
        mac.update(message.as_bytes());
        mac.verify_slice(sig).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capability_ids_unique() {
        let c1 = CapabilityRef::new("Port1".into(), "svc/1".into());
        let c2 = CapabilityRef::new("Port2".into(), "svc/2".into());
        assert_ne!(c1.id(), c2.id());
    }

    #[test]
    fn test_capability_token_unique() {
        let c1 = CapabilityRef::new("Port1".into(), "svc/1".into());
        let c2 = CapabilityRef::new("Port1".into(), "svc/1".into());
        assert_ne!(c1.token(), c2.token());
    }

    #[test]
    fn test_capability_token_verification() {
        let cap = CapabilityRef::new("Port1".into(), "svc/1".into());
        let correct_token = cap.token();
        assert!(cap.verify_token(correct_token));
        assert!(!cap.verify_token(correct_token + 1));
    }

    #[test]
    fn test_capability_origin() {
        let c1 = CapabilityRef::new("P".into(), "s".into());
        assert_eq!(c1.origin(), CapabilityOrigin::Resolved);

        let c2 =
            CapabilityRef::new_with_origin("P".into(), "s".into(), CapabilityOrigin::EntryInjected);
        assert_eq!(c2.origin(), CapabilityOrigin::EntryInjected);
    }

    #[test]
    fn test_capability_accessors() {
        let cap = CapabilityRef::new("MyPort".into(), "svc/main".into());
        assert_eq!(cap.port_name(), "MyPort");
        assert_eq!(cap.service_id(), "svc/main");
    }

    #[test]
    fn test_hmac_signature_round_trip() {
        let key = b"test-signing-key";
        let cap = CapabilityRef::new_signed("KV".into(), "kv/main".into(), key);
        assert!(cap.signature().is_some());
        assert!(cap.verify_signature(key));
    }

    #[test]
    fn test_hmac_wrong_key_fails() {
        let key = b"correct-key";
        let wrong_key = b"wrong-key";
        let cap = CapabilityRef::new_signed("KV".into(), "kv/main".into(), key);
        assert!(!cap.verify_signature(wrong_key));
    }

    #[test]
    fn test_unsigned_verify_returns_false() {
        let cap = CapabilityRef::new("KV".into(), "kv/main".into());
        assert!(cap.signature().is_none());
        assert!(!cap.verify_signature(b"any-key"));
    }

    // === Gap 5: Random (non-sequential) capability tokens ===

    #[test]
    fn test_capability_tokens_non_sequential() {
        let c1 = CapabilityRef::new("Port1".into(), "svc/1".into());
        let c2 = CapabilityRef::new("Port1".into(), "svc/1".into());
        // Tokens should be random, not sequential (c2 != c1 + 1)
        // With random u64 tokens, the probability of (c2 == c1 + 1) is negligible
        assert_ne!(c1.token(), c2.token(), "tokens should be different");
        // Additional check: tokens should not be sequential
        let diff = if c2.token() > c1.token() {
            c2.token() - c1.token()
        } else {
            c1.token() - c2.token()
        };
        assert_ne!(diff, 1, "tokens should not be sequential (difference should not be 1)");
    }
}
