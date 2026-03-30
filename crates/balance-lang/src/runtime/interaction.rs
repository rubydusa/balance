use std::collections::HashSet;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::event::EventFilter;
use super::value::Value;

/// A first-class interaction value returned by capability method calls.
/// Must be `await`ed to actually dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionHandle {
    pub service_id: String,
    pub port_name: String,
    pub method: String,
    pub args: Vec<Value>,
    pub kind: InteractionKind,
    /// Optional transport endpoint for remote dispatch (e.g., "127.0.0.1:9000").
    pub endpoint: Option<String>,
}

static NEXT_INTERACTION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RetryStrategy {
    AtLeastOnce,
    AtMostOnce,
    ExactlyOnce,
}

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    pub strategy: RetryStrategy,
    pub timeout_ms: u64,
}

impl RetryPolicy {
    pub fn default_policy() -> Self {
        Self {
            max_retries: 3,
            strategy: RetryStrategy::AtLeastOnce,
            timeout_ms: 5000,
        }
    }

    pub fn local_fast() -> Self {
        Self {
            max_retries: 1,
            strategy: RetryStrategy::AtMostOnce,
            timeout_ms: 1000,
        }
    }

    pub fn strong() -> Self {
        Self {
            max_retries: 5,
            strategy: RetryStrategy::ExactlyOnce,
            timeout_ms: 10000,
        }
    }

    pub fn from_profile(profile_name: Option<&str>) -> Self {
        match profile_name {
            Some("local_fast") => Self::local_fast(),
            Some("public_strong") => Self::strong(),
            _ => Self::default_policy(),
        }
    }

    /// Create a RetryPolicy from profile data, using profile-specific values if available.
    pub fn from_profile_data(profile: Option<&super::registry::Profile>) -> Self {
        let mut policy = match profile.map(|p| p.name.as_str()) {
            Some("local_fast") => Self::local_fast(),
            Some("public_strong") => Self::strong(),
            _ => Self::default_policy(),
        };

        if let Some(p) = profile {
            if let Some(timeout) = p.timeout_ms {
                policy.timeout_ms = timeout;
            }
            if let Some(retries) = p.max_retries {
                policy.max_retries = retries;
            }
        }

        policy
    }
}

/// Exponential backoff delay with deterministic jitter (no `rand` dependency).
/// Base delay is 100ms, doubling each attempt, with jitter derived from interaction_id.
pub fn backoff_delay(attempt: u32, interaction_id: u64) -> std::time::Duration {
    let base_ms: u64 = 100;
    let exp_ms = base_ms.saturating_mul(1u64 << attempt.min(10));
    // Deterministic jitter: use interaction_id to add 0-50% of exp_ms
    let jitter = (interaction_id.wrapping_mul(2654435761) % (exp_ms / 2 + 1)) as u64;
    std::time::Duration::from_millis(exp_ms + jitter)
}

/// Circuit breaker: tracks failure count per service, opens circuit after threshold.
#[derive(Debug)]
pub struct CircuitBreaker {
    /// service_id -> (failure_count, last_failure_time_ms)
    states: std::collections::HashMap<String, CircuitBreakerState>,
    /// Number of consecutive failures before opening circuit
    pub failure_threshold: u32,
    /// Cooldown duration in ms before trying half-open
    pub cooldown_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CircuitState {
    Closed,
    Open,
    HalfOpen,
}

#[derive(Debug, Clone)]
struct CircuitBreakerState {
    failure_count: u32,
    last_failure_ms: u64,
    state: CircuitState,
}

impl CircuitBreaker {
    pub fn new(failure_threshold: u32, cooldown_ms: u64) -> Self {
        Self {
            states: std::collections::HashMap::new(),
            failure_threshold,
            cooldown_ms,
        }
    }

    /// Check if a service's circuit is open (should skip dispatch).
    pub fn is_open(&self, service_id: &str) -> bool {
        matches!(self.get_state(service_id), CircuitState::Open)
    }

    /// Get the current circuit state for a service.
    pub fn get_state(&self, service_id: &str) -> CircuitState {
        match self.states.get(service_id) {
            None => CircuitState::Closed,
            Some(s) => {
                if s.state == CircuitState::Open {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    if now - s.last_failure_ms >= self.cooldown_ms {
                        CircuitState::HalfOpen
                    } else {
                        CircuitState::Open
                    }
                } else {
                    s.state
                }
            }
        }
    }

    /// Record a successful call — resets the circuit to Closed.
    pub fn record_success(&mut self, service_id: &str) {
        self.states.insert(service_id.to_string(), CircuitBreakerState {
            failure_count: 0,
            last_failure_ms: 0,
            state: CircuitState::Closed,
        });
    }

    /// Record a failed call — increments failure count, may open circuit.
    pub fn record_failure(&mut self, service_id: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let entry = self.states.entry(service_id.to_string()).or_insert(CircuitBreakerState {
            failure_count: 0,
            last_failure_ms: 0,
            state: CircuitState::Closed,
        });

        entry.failure_count += 1;
        entry.last_failure_ms = now;

        if entry.failure_count >= self.failure_threshold {
            entry.state = CircuitState::Open;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum InteractionKind {
    Command,
    Query,
    /// Block bodies that don't declare settlement/observation.
    Pure,
}

impl fmt::Display for InteractionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InteractionKind::Command => write!(f, "command"),
            InteractionKind::Query => write!(f, "query"),
            InteractionKind::Pure => write!(f, "pure"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum InteractionState {
    Pending,
    Executing,
    Settled,
    Observed,
    Completed,
    /// Command completed without settlement semantics.
    CompletedUnsettled,
    /// Query completed without observation semantics.
    CompletedUnobserved,
    Failed,
    /// Operation was reverted by implicit block-level atomicity.
    Reverted,
}

impl fmt::Display for InteractionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InteractionState::Pending => write!(f, "pending"),
            InteractionState::Executing => write!(f, "executing"),
            InteractionState::Settled => write!(f, "settled"),
            InteractionState::Observed => write!(f, "observed"),
            InteractionState::Completed => write!(f, "completed"),
            InteractionState::CompletedUnsettled => write!(f, "completed(unsettled)"),
            InteractionState::CompletedUnobserved => write!(f, "completed(unobserved)"),
            InteractionState::Failed => write!(f, "failed"),
            InteractionState::Reverted => write!(f, "reverted"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionRecord {
    pub id: u64,
    pub service_id: String,
    pub method: String,
    pub kind: InteractionKind,
    pub state: InteractionState,
    #[serde(skip)]
    pub settle_filter: Option<EventFilter>,
    #[serde(skip)]
    pub observe_filter: Option<EventFilter>,
    pub provisional_result: Option<Value>,
}

/// Tracks the lifecycle of all interactions with enforced state transitions.
pub struct InteractionEngine {
    trace: Vec<InteractionRecord>,
    /// Deduplication: Ack keys already seen for idempotency.
    seen_keys: HashSet<String>,
    /// Cached results for deduplication.
    cached_results: std::collections::HashMap<String, Value>,
}

impl InteractionEngine {
    pub fn new() -> Self {
        Self {
            trace: Vec::new(),
            seen_keys: HashSet::new(),
            cached_results: std::collections::HashMap::new(),
        }
    }

    /// Check if an Ack key has already been seen (deduplication).
    pub fn is_duplicate(&self, key: &str) -> bool {
        self.seen_keys.contains(key)
    }

    /// Record an Ack key as seen and cache its result.
    pub fn record_ack(&mut self, key: String, result: Value) {
        self.seen_keys.insert(key.clone());
        self.cached_results.insert(key, result);
    }

    /// Get cached result for a deduplicated Ack key.
    pub fn get_cached(&self, key: &str) -> Option<&Value> {
        self.cached_results.get(key)
    }

    /// Begin a new interaction, returning its unique ID.
    pub fn begin(&mut self, service_id: &str, method: &str, kind: InteractionKind) -> u64 {
        let id = NEXT_INTERACTION_ID.fetch_add(1, Ordering::Relaxed);
        self.trace.push(InteractionRecord {
            id,
            service_id: service_id.to_string(),
            method: method.to_string(),
            kind,
            state: InteractionState::Pending,
            settle_filter: None,
            observe_filter: None,
            provisional_result: None,
        });
        id
    }

    /// Transition to Executing. Requires Pending.
    pub fn mark_executing(&mut self, id: u64) -> Result<(), String> {
        self.transition(id, InteractionState::Pending, InteractionState::Executing)
    }

    /// Store the provisional result. Requires Executing.
    pub fn set_provisional(&mut self, id: u64, value: Value) -> Result<(), String> {
        let rec = self.find_mut(id)?;
        if rec.state != InteractionState::Executing {
            return Err(format!(
                "interaction {id}: cannot set provisional in state '{}'",
                rec.state
            ));
        }
        rec.provisional_result = Some(value);
        Ok(())
    }

    /// Register a settlement filter on the interaction.
    pub fn set_settle_filter(&mut self, id: u64, filter: EventFilter) -> Result<(), String> {
        let rec = self.find_mut(id)?;
        rec.settle_filter = Some(filter);
        Ok(())
    }

    /// Register an observation filter on the interaction.
    pub fn set_observe_filter(&mut self, id: u64, filter: EventFilter) -> Result<(), String> {
        let rec = self.find_mut(id)?;
        rec.observe_filter = Some(filter);
        Ok(())
    }

    /// Mark as settled. Requires Executing and kind == Command.
    pub fn mark_settled(&mut self, id: u64) -> Result<(), String> {
        let rec = self.find_mut(id)?;
        if rec.state != InteractionState::Executing {
            return Err(format!(
                "interaction {id}: cannot settle in state '{}'",
                rec.state
            ));
        }
        if rec.kind != InteractionKind::Command {
            return Err(format!(
                "interaction {id}: cannot settle a {} (only commands)",
                rec.kind
            ));
        }
        rec.state = InteractionState::Settled;
        Ok(())
    }

    /// Mark as observed. Requires Executing and kind == Query.
    pub fn mark_observed(&mut self, id: u64) -> Result<(), String> {
        let rec = self.find_mut(id)?;
        if rec.state != InteractionState::Executing {
            return Err(format!(
                "interaction {id}: cannot observe in state '{}'",
                rec.state
            ));
        }
        if rec.kind != InteractionKind::Query {
            return Err(format!(
                "interaction {id}: cannot observe a {} (only queries)",
                rec.kind
            ));
        }
        rec.state = InteractionState::Observed;
        Ok(())
    }

    /// Complete the interaction. Commands require Settled, Queries require Observed.
    pub fn complete(&mut self, id: u64) -> Result<(), String> {
        let rec = self.find_mut(id)?;
        match (rec.kind, rec.state) {
            (InteractionKind::Command, InteractionState::Settled) => {
                rec.state = InteractionState::Completed;
                Ok(())
            }
            (InteractionKind::Query, InteractionState::Observed) => {
                rec.state = InteractionState::Completed;
                Ok(())
            }
            // Pure interactions (block bodies) and built-in services can complete from Executing
            (InteractionKind::Pure, InteractionState::Executing) => {
                rec.state = InteractionState::Completed;
                Ok(())
            }
            // Command block bodies: mark as unsettled
            (InteractionKind::Command, InteractionState::Executing) => {
                rec.state = InteractionState::CompletedUnsettled;
                Ok(())
            }
            // Query block bodies: mark as unobserved
            (InteractionKind::Query, InteractionState::Executing) => {
                rec.state = InteractionState::CompletedUnobserved;
                Ok(())
            }
            _ => Err(format!(
                "interaction {id}: cannot complete {} in state '{}'",
                rec.kind, rec.state
            )),
        }
    }

    /// Fail from any state.
    pub fn fail(&mut self, id: u64) -> Result<(), String> {
        let rec = self.find_mut(id)?;
        rec.state = InteractionState::Failed;
        Ok(())
    }

    /// Mark as reverted (by implicit block-level atomicity compensation).
    pub fn mark_reverted(&mut self, id: u64) -> Result<(), String> {
        let rec = self.find_mut(id)?;
        rec.state = InteractionState::Reverted;
        Ok(())
    }

    pub fn trace(&self) -> &[InteractionRecord] {
        &self.trace
    }

    pub fn get(&self, id: u64) -> Option<&InteractionRecord> {
        self.trace.iter().rev().find(|r| r.id == id)
    }

    /// Get the current state of an interaction.
    pub fn get_state(&self, id: u64) -> Option<InteractionState> {
        self.trace.iter().rev().find(|r| r.id == id).map(|r| r.state)
    }

    /// Begin a new interaction submission: creates the record and transitions to Executing.
    /// Returns the interaction ID. This is a convenience method combining `begin` + `mark_executing`.
    pub fn begin_submission(&mut self, service_id: &str, method: &str, kind: InteractionKind) -> Result<u64, String> {
        let id = self.begin(service_id, method, kind);
        self.mark_executing(id)?;
        Ok(id)
    }

    /// Complete an interaction submission: sets the provisional result and finalizes the state.
    /// If `settled` is true, marks as settled/observed (depending on kind) before completing.
    /// If `settled` is false, completes directly (may result in CompletedUnsettled/CompletedUnobserved).
    pub fn complete_submission(&mut self, id: u64, result: Value, settled: bool) -> Result<(), String> {
        self.set_provisional(id, result)?;
        if settled {
            let rec = self.find_mut(id)?;
            match rec.kind {
                InteractionKind::Command => {
                    rec.state = InteractionState::Settled;
                }
                InteractionKind::Query => {
                    rec.state = InteractionState::Observed;
                }
                InteractionKind::Pure => {
                    // Pure interactions don't need settlement/observation
                }
            }
        }
        self.complete(id)
    }

    fn find_mut(&mut self, id: u64) -> Result<&mut InteractionRecord, String> {
        self.trace
            .iter_mut()
            .rev()
            .find(|r| r.id == id)
            .ok_or_else(|| format!("interaction {id} not found"))
    }

    fn transition(
        &mut self,
        id: u64,
        required: InteractionState,
        target: InteractionState,
    ) -> Result<(), String> {
        let rec = self.find_mut(id)?;
        if rec.state != required {
            return Err(format!(
                "interaction {id}: expected state '{}', found '{}'",
                required, rec.state
            ));
        }
        rec.state = target;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_command_lifecycle() {
        let mut engine = InteractionEngine::new();
        let id = engine.begin("svc/main", "put", InteractionKind::Command);
        engine.mark_executing(id).unwrap();
        engine
            .set_provisional(id, Value::String("data".into()))
            .unwrap();
        engine.mark_settled(id).unwrap();
        engine.complete(id).unwrap();
        assert_eq!(engine.get(id).unwrap().state, InteractionState::Completed);
    }

    #[test]
    fn test_valid_query_lifecycle() {
        let mut engine = InteractionEngine::new();
        let id = engine.begin("svc/main", "get", InteractionKind::Query);
        engine.mark_executing(id).unwrap();
        engine.mark_observed(id).unwrap();
        engine.complete(id).unwrap();
        assert_eq!(engine.get(id).unwrap().state, InteractionState::Completed);
    }

    #[test]
    fn test_command_without_settle_becomes_unsettled() {
        let mut engine = InteractionEngine::new();
        let id = engine.begin("svc/main", "put", InteractionKind::Command);
        engine.mark_executing(id).unwrap();
        engine.complete(id).unwrap();
        assert_eq!(
            engine.get(id).unwrap().state,
            InteractionState::CompletedUnsettled
        );
    }

    #[test]
    fn test_query_without_observe_becomes_unobserved() {
        let mut engine = InteractionEngine::new();
        let id = engine.begin("svc/main", "get", InteractionKind::Query);
        engine.mark_executing(id).unwrap();
        engine.complete(id).unwrap();
        assert_eq!(
            engine.get(id).unwrap().state,
            InteractionState::CompletedUnobserved
        );
    }

    #[test]
    fn test_pure_lifecycle() {
        let mut engine = InteractionEngine::new();
        let id = engine.begin("svc/main", "action", InteractionKind::Pure);
        engine.mark_executing(id).unwrap();
        engine.complete(id).unwrap();
        assert_eq!(engine.get(id).unwrap().state, InteractionState::Completed);
    }

    #[test]
    fn test_invalid_settle_on_query() {
        let mut engine = InteractionEngine::new();
        let id = engine.begin("svc/main", "get", InteractionKind::Query);
        engine.mark_executing(id).unwrap();
        let result = engine.mark_settled(id);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("only commands"));
    }

    #[test]
    fn test_invalid_observe_on_command() {
        let mut engine = InteractionEngine::new();
        let id = engine.begin("svc/main", "put", InteractionKind::Command);
        engine.mark_executing(id).unwrap();
        let result = engine.mark_observed(id);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("only queries"));
    }

    #[test]
    fn test_invalid_transition_from_pending() {
        let mut engine = InteractionEngine::new();
        let id = engine.begin("svc/main", "put", InteractionKind::Command);
        // Can't settle from Pending
        let result = engine.mark_settled(id);
        assert!(result.is_err());
    }

    #[test]
    fn test_fail_from_any_state() {
        let mut engine = InteractionEngine::new();
        let id = engine.begin("svc/main", "put", InteractionKind::Command);
        engine.fail(id).unwrap();
        assert_eq!(engine.get(id).unwrap().state, InteractionState::Failed);

        let id2 = engine.begin("svc/main", "get", InteractionKind::Query);
        engine.mark_executing(id2).unwrap();
        engine.fail(id2).unwrap();
        assert_eq!(engine.get(id2).unwrap().state, InteractionState::Failed);
    }

    #[test]
    fn test_mark_reverted() {
        let mut engine = InteractionEngine::new();
        let id = engine.begin("svc/main", "put", InteractionKind::Command);
        engine.mark_executing(id).unwrap();
        engine.mark_settled(id).unwrap();
        engine.mark_reverted(id).unwrap();
        assert_eq!(engine.get(id).unwrap().state, InteractionState::Reverted);
    }

    #[test]
    fn test_reverted_display() {
        assert_eq!(format!("{}", InteractionState::Reverted), "reverted");
    }

    #[test]
    fn test_provisional_result() {
        let mut engine = InteractionEngine::new();
        let id = engine.begin("svc/main", "put", InteractionKind::Command);
        engine.mark_executing(id).unwrap();
        engine
            .set_provisional(id, Value::ack("k1".into()))
            .unwrap();
        let rec = engine.get(id).unwrap();
        assert!(rec.provisional_result.is_some());
    }

    #[test]
    fn test_deduplication() {
        let mut engine = InteractionEngine::new();
        assert!(!engine.is_duplicate("key1"));

        engine.record_ack("key1".into(), Value::String("result1".into()));
        assert!(engine.is_duplicate("key1"));
        assert!(!engine.is_duplicate("key2"));

        let cached = engine.get_cached("key1");
        assert!(cached.is_some());
        assert_eq!(cached.unwrap(), &Value::String("result1".into()));
    }

    #[test]
    fn test_retry_policy_from_profile() {
        let default = RetryPolicy::default_policy();
        assert_eq!(default.max_retries, 3);

        let fast = RetryPolicy::from_profile(Some("local_fast"));
        assert_eq!(fast.max_retries, 1);
        assert_eq!(fast.strategy, RetryStrategy::AtMostOnce);

        let strong = RetryPolicy::from_profile(Some("public_strong"));
        assert_eq!(strong.max_retries, 5);
        assert_eq!(strong.strategy, RetryStrategy::ExactlyOnce);
    }

    #[test]
    fn test_retry_policy_from_profile_data() {
        use crate::runtime::registry::Profile;

        let mut profile = Profile::new("custom");
        profile.timeout_ms = Some(15000);
        profile.max_retries = Some(7);

        let policy = RetryPolicy::from_profile_data(Some(&profile));
        assert_eq!(policy.timeout_ms, 15000);
        assert_eq!(policy.max_retries, 7);
    }

    #[test]
    fn test_retry_policy_from_profile_data_none() {
        let policy = RetryPolicy::from_profile_data(None);
        assert_eq!(policy.max_retries, 3); // default
        assert_eq!(policy.timeout_ms, 5000); // default
    }

    #[test]
    fn test_backoff_delay_increases() {
        let d0 = backoff_delay(0, 42);
        let d1 = backoff_delay(1, 42);
        let d2 = backoff_delay(2, 42);
        // Each should be roughly double the previous (with jitter)
        assert!(d1 > d0, "d1 {:?} should be > d0 {:?}", d1, d0);
        assert!(d2 > d1, "d2 {:?} should be > d1 {:?}", d2, d1);
    }

    #[test]
    fn test_backoff_delay_deterministic() {
        let d1 = backoff_delay(2, 100);
        let d2 = backoff_delay(2, 100);
        assert_eq!(d1, d2, "same inputs should produce same delay");
    }

    #[test]
    fn test_circuit_breaker_closed_by_default() {
        let cb = CircuitBreaker::new(3, 1000);
        assert_eq!(cb.get_state("svc1"), CircuitState::Closed);
        assert!(!cb.is_open("svc1"));
    }

    #[test]
    fn test_circuit_breaker_opens_after_threshold() {
        let mut cb = CircuitBreaker::new(3, 1000);
        cb.record_failure("svc1");
        cb.record_failure("svc1");
        assert_eq!(cb.get_state("svc1"), CircuitState::Closed);

        cb.record_failure("svc1"); // 3rd failure = threshold
        assert!(cb.is_open("svc1"));
    }

    #[test]
    fn test_circuit_breaker_resets_on_success() {
        let mut cb = CircuitBreaker::new(2, 1000);
        cb.record_failure("svc1");
        cb.record_failure("svc1");
        assert!(cb.is_open("svc1"));

        cb.record_success("svc1");
        assert_eq!(cb.get_state("svc1"), CircuitState::Closed);
        assert!(!cb.is_open("svc1"));
    }

    #[test]
    fn test_circuit_breaker_half_open_after_cooldown() {
        let mut cb = CircuitBreaker::new(2, 0); // 0ms cooldown for testing
        cb.record_failure("svc1");
        cb.record_failure("svc1");

        // With 0ms cooldown, should immediately be half-open
        assert_eq!(cb.get_state("svc1"), CircuitState::HalfOpen);
    }

    #[test]
    fn test_circuit_breaker_per_service_isolation() {
        let mut cb = CircuitBreaker::new(2, 1000);
        cb.record_failure("svc1");
        cb.record_failure("svc1");
        assert!(cb.is_open("svc1"));
        assert!(!cb.is_open("svc2")); // different service is still closed
    }

    #[test]
    fn test_retry_policy_from_profile_data_with_transport() {
        use crate::runtime::registry::Profile;

        let profile = Profile::new("quic_fast")
            .with_preference("transport", "quic")
            .with_preference("timeout", "500")
            .with_preference("max_retries", "2");

        assert_eq!(profile.preferred_transport, crate::runtime::registry::TransportPreference::Quic);
        let policy = RetryPolicy::from_profile_data(Some(&profile));
        assert_eq!(policy.timeout_ms, 500);
        assert_eq!(policy.max_retries, 2);
    }

    #[test]
    fn test_backoff_delay_capped() {
        // Attempt 15 should be capped at 2^10 * 100ms base
        let d = backoff_delay(15, 42);
        // Max base = 100 * 1024 = 102400ms, max jitter = ~51200ms
        // Total max ~153600ms
        assert!(d.as_millis() <= 200000, "backoff should be capped");
    }

    #[test]
    fn test_begin_submission() {
        let mut engine = InteractionEngine::new();
        let id = engine.begin_submission("svc/main", "put", InteractionKind::Command).unwrap();
        assert_eq!(engine.get(id).unwrap().state, InteractionState::Executing);
    }

    #[test]
    fn test_complete_submission_settled() {
        let mut engine = InteractionEngine::new();
        let id = engine.begin_submission("svc/main", "put", InteractionKind::Command).unwrap();
        engine.complete_submission(id, Value::String("ok".into()), true).unwrap();
        assert_eq!(engine.get(id).unwrap().state, InteractionState::Completed);
        assert_eq!(
            engine.get(id).unwrap().provisional_result,
            Some(Value::String("ok".into()))
        );
    }

    #[test]
    fn test_complete_submission_unsettled() {
        let mut engine = InteractionEngine::new();
        let id = engine.begin_submission("svc/main", "put", InteractionKind::Command).unwrap();
        engine.complete_submission(id, Value::String("ok".into()), false).unwrap();
        assert_eq!(engine.get(id).unwrap().state, InteractionState::CompletedUnsettled);
    }

    #[test]
    fn test_complete_submission_query_observed() {
        let mut engine = InteractionEngine::new();
        let id = engine.begin_submission("svc/main", "get", InteractionKind::Query).unwrap();
        engine.complete_submission(id, Value::Int(42), true).unwrap();
        assert_eq!(engine.get(id).unwrap().state, InteractionState::Completed);
    }

    #[test]
    fn test_complete_submission_pure() {
        let mut engine = InteractionEngine::new();
        let id = engine.begin_submission("svc/main", "action", InteractionKind::Pure).unwrap();
        engine.complete_submission(id, Value::Unit, false).unwrap();
        assert_eq!(engine.get(id).unwrap().state, InteractionState::Completed);
    }
}
