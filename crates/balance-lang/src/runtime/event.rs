use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::broadcast;

use serde::{Deserialize, Serialize};

use super::causal::LamportClock;
use super::persistent_event::PersistentEventLog;
use super::value::Value;

static NEXT_EVENT_ID: AtomicU64 = AtomicU64::new(1);
static MONOTONIC_CLOCK: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EventId(pub u64);

impl EventId {
    fn next() -> Self {
        Self(NEXT_EVENT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

impl std::fmt::Display for EventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "event#{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub timestamp: u64,
    pub source: String,
    pub event_type: String,
    pub data: HashMap<String, Value>,
}

/// A filter handle for matching events in a stream.
#[derive(Debug, Clone)]
pub struct EventFilter {
    pub source: String,
    pub event_type: String,
}

/// An ordered event log for a single source.
#[derive(Debug, Default)]
pub struct EventStream {
    events: Vec<Event>,
}

impl EventStream {
    fn new() -> Self {
        Self { events: Vec::new() }
    }

    fn append(&mut self, event: Event) {
        self.events.push(event);
    }

    fn latest(&self) -> Option<&Event> {
        self.events.last()
    }

    /// Returns the substrate-local offset of the most recent event, extracted
    /// from `data["offset"]`. Returns None if no events or no offset field.
    fn frontier(&self) -> Option<i64> {
        self.events.last().and_then(|e| match e.data.get("offset") {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        })
    }

    /// Returns the substrate-local offset of the most recent event of a specific type,
    /// extracted from `data["offset"]`. Returns None if no matching events or no offset field.
    fn frontier_for(&self, event_type: &str) -> Option<i64> {
        self.events
            .iter()
            .rev()
            .find(|e| e.event_type == event_type)
            .and_then(|e| match e.data.get("offset") {
                Some(Value::Int(n)) => Some(*n),
                _ => None,
            })
    }

    fn find_by_type(&self, event_type: &str) -> Option<&Event> {
        self.events.iter().rev().find(|e| e.event_type == event_type)
    }

    fn events(&self) -> &[Event] {
        &self.events
    }
}

pub struct EventBus {
    streams: HashMap<String, EventStream>,
    /// Flat list of all events for global trace.
    all_events: Vec<Event>,
    /// Broadcast sender for async event notification.
    sender: broadcast::Sender<Event>,
    /// Node identifier for distributed event attribution.
    node_id: String,
    /// Optional persistent event log for durability.
    persistent_log: Option<PersistentEventLog>,
    /// Lamport clock for causal ordering across nodes.
    lamport_clock: LamportClock,
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field("streams", &self.streams)
            .field("all_events", &self.all_events)
            .field("node_id", &self.node_id)
            .field("lamport_clock", &self.lamport_clock)
            .finish()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(256);
        Self {
            streams: HashMap::new(),
            all_events: Vec::new(),
            sender,
            node_id: String::new(),
            persistent_log: None,
            lamport_clock: LamportClock::new(),
        }
    }

    /// Create an EventBus with persistence enabled.
    /// Events will be written to the given path as NDJSON.
    pub fn with_persistence(path: &Path, node_id: &str) -> Result<Self, String> {
        let persistent_log = PersistentEventLog::open(path, node_id)?;
        let (sender, _) = broadcast::channel(256);
        Ok(Self {
            streams: HashMap::new(),
            all_events: Vec::new(),
            sender,
            node_id: node_id.to_string(),
            persistent_log: Some(persistent_log),
            lamport_clock: LamportClock::new(),
        })
    }

    /// Recover events from the persistent log into memory.
    /// Should be called after `with_persistence()` to restore state.
    pub fn recover(&mut self) -> Result<usize, String> {
        let events = if let Some(ref log) = self.persistent_log {
            log.recover()?
        } else {
            return Ok(0);
        };

        let count = events.len();
        for event in events {
            // Update lamport clock from recovered event timestamps
            if let Some(lamport) = event.data.get("lamport_time") {
                if let Value::Int(t) = lamport {
                    self.lamport_clock.merge(*t as u64);
                }
            }
            let source = event.source.clone();
            self.streams
                .entry(source)
                .or_insert_with(EventStream::new)
                .append(event.clone());
            self.all_events.push(event);
        }
        Ok(count)
    }

    /// Get the node ID for this event bus.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Set the node ID for this event bus.
    pub fn set_node_id(&mut self, node_id: &str) {
        self.node_id = node_id.to_string();
    }

    /// Get the current Lamport timestamp without advancing.
    pub fn lamport_time(&self) -> u64 {
        self.lamport_clock.current()
    }

    /// Merge with a received Lamport timestamp from a remote node.
    pub fn merge_lamport(&mut self, received: u64) -> u64 {
        self.lamport_clock.merge(received)
    }

    /// Publish an event to the appropriate stream and broadcast it.
    pub fn publish(&mut self, source: String, event_type: String, mut data: HashMap<String, Value>) {
        // Tick lamport clock and include in event data
        let lamport_time = self.lamport_clock.tick();
        data.entry("lamport_time".to_string())
            .or_insert(Value::Int(lamport_time as i64));

        let event = Event {
            id: EventId::next(),
            timestamp: MONOTONIC_CLOCK.fetch_add(1, Ordering::Relaxed),
            source: source.clone(),
            event_type,
            data,
        };

        // Persist if log is configured
        if let Some(ref mut log) = self.persistent_log {
            if let Err(e) = log.append(&event) {
                eprintln!("warning: failed to persist event: {e}");
            }
        }

        self.streams
            .entry(source)
            .or_insert_with(EventStream::new)
            .append(event.clone());
        self.all_events.push(event.clone());
        // Best-effort broadcast — ignore if no receivers
        let _ = self.sender.send(event);
    }

    /// Create a filter handle for subscribing to events.
    pub fn subscribe(&self, source: &str, event_type: &str) -> EventFilter {
        EventFilter {
            source: source.to_string(),
            event_type: event_type.to_string(),
        }
    }

    /// Check if a matching event exists for the given filter.
    pub fn match_event(&self, filter: &EventFilter) -> Option<&Event> {
        self.streams
            .get(&filter.source)
            .and_then(|stream| stream.find_by_type(&filter.event_type))
    }

    /// In synchronous mode, same as match_event. Semantically distinct for future async.
    pub fn wait_for(&self, filter: &EventFilter) -> Option<&Event> {
        self.match_event(filter)
    }

    /// Get the substrate-local offset frontier for a source stream.
    /// Extracted from the most recent event's `data["offset"]`.
    pub fn frontier(&self, source: &str) -> Option<i64> {
        self.streams.get(source).and_then(|stream| stream.frontier())
    }

    /// Get the substrate-local offset frontier for a specific event type in a source stream.
    /// Unlike `frontier()`, this filters by event type before returning the offset.
    pub fn frontier_for(&self, source: &str, event_type: &str) -> Option<i64> {
        self.streams
            .get(source)
            .and_then(|stream| stream.frontier_for(event_type))
    }

    /// Get the latest event for a source stream.
    pub fn latest(&self, source: &str) -> Option<&Event> {
        self.streams.get(source).and_then(|stream| stream.latest())
    }

    /// Get all events in a specific stream.
    pub fn stream_events(&self, source: &str) -> &[Event] {
        self.streams
            .get(source)
            .map(|s| s.events())
            .unwrap_or(&[])
    }

    /// Legacy: match by source and event_type strings directly.
    pub fn match_event_by_strings(&self, source: &str, event_type: &str) -> Option<&Event> {
        let filter = EventFilter {
            source: source.to_string(),
            event_type: event_type.to_string(),
        };
        self.match_event(&filter)
    }

    /// General predicate match: search a source stream newest-to-oldest for
    /// the first event satisfying the predicate. This is the canonical matching
    /// primitive (execution_model §15: settle_on/observe_on reduce to predicates).
    pub fn match_event_predicate<F>(&self, source: &str, predicate: F) -> Option<&Event>
    where
        F: Fn(&Event) -> bool,
    {
        self.streams.get(source).and_then(|stream| {
            stream.events().iter().rev().find(|e| predicate(e))
        })
    }

    /// Async predicate wait: fast-path check existing events, then broadcast fallback.
    /// Canonical async form of predicate matching.
    pub async fn wait_for_event_predicate<F>(&self, source: &str, predicate: F) -> Event
    where
        F: Fn(&Event) -> bool,
    {
        // Fast path: check existing events
        if let Some(event) = self.match_event_predicate(source, &predicate) {
            return event.clone();
        }
        // Slow path: subscribe and wait
        let mut rx = self.sender.subscribe();
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if event.source == source && predicate(&event) {
                        return event;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    if let Some(event) = self.match_event_predicate(source, &predicate) {
                        return event.clone();
                    }
                    panic!("EventBus closed while waiting for predicate match");
                }
            }
        }
    }

    /// Match by source, event_type, and minimum substrate-local offset
    /// (frontier-based observation per spec Section 6).
    /// Returns the most recent event with data["offset"] >= min_offset.
    /// Delegates to `match_event_predicate` internally.
    pub fn match_event_frontier(
        &self,
        source: &str,
        event_type: &str,
        min_offset: i64,
    ) -> Option<&Event> {
        self.match_event_predicate(source, |e| {
            e.event_type == event_type
                && e.data
                    .get("offset")
                    .and_then(|v| match v {
                        Value::Int(n) => Some(*n),
                        _ => None,
                    })
                    .map(|n| n >= min_offset)
                    .unwrap_or(false)
        })
    }

    /// Match by source, event_type, and correlated key value.
    /// The key_value is matched against the event's "key" data field.
    /// Delegates to `match_event_predicate` internally.
    pub fn match_event_correlated(
        &self,
        source: &str,
        event_type: &str,
        key_value: &str,
    ) -> Option<&Event> {
        self.match_event_predicate(source, |e| {
            e.event_type == event_type
                && e.data
                    .get("key")
                    .map(|v| format!("{v}") == key_value)
                    .unwrap_or(false)
        })
    }

    /// Get all events across all streams (global trace).
    pub fn events(&self) -> &[Event] {
        &self.all_events
    }

    /// Find events matching a predicate across all streams.
    pub fn find_events<F>(&self, predicate: F) -> Vec<&Event>
    where
        F: Fn(&Event) -> bool,
    {
        self.all_events.iter().filter(|e| predicate(e)).collect()
    }

    /// Subscribe to all events from a source matching a given event type.
    /// Returns all matching events in chronological order.
    pub fn subscribe_source(&self, source: &str, event_type: &str) -> Vec<&Event> {
        self.streams
            .get(source)
            .map(|stream| {
                stream
                    .events()
                    .iter()
                    .filter(|e| e.event_type == event_type)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Poll for the next event from a source after a given substrate-local offset.
    /// Returns the first event with data["offset"] > after_offset, or the first
    /// event if after_offset is None.
    pub fn poll_source(&self, source: &str, after_offset: Option<i64>) -> Option<&Event> {
        self.streams.get(source).and_then(|stream| {
            match after_offset {
                Some(offset) => stream.events().iter().find(|e| {
                    e.data
                        .get("offset")
                        .and_then(|v| match v {
                            Value::Int(n) => Some(*n),
                            _ => None,
                        })
                        .map(|n| n > offset)
                        .unwrap_or(false)
                }),
                None => stream.events().first(),
            }
        })
    }

    /// Create a broadcast receiver for async event waiting.
    pub fn subscribe_receiver(&self) -> broadcast::Receiver<Event> {
        self.sender.subscribe()
    }

    /// Wait for an event matching source, event_type, and a correlated key value.
    /// Checks existing events first (fast path), then subscribes and waits.
    pub async fn wait_for_event_correlated(
        &self,
        source: &str,
        event_type: &str,
        key_value: &str,
    ) -> Event {
        // Fast path: check existing events
        if let Some(event) = self.match_event_correlated(source, event_type, key_value) {
            return event.clone();
        }
        // Slow path: subscribe and wait
        let mut rx = self.sender.subscribe();
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if event.source == source
                        && event.event_type == event_type
                        && event
                            .data
                            .get("key")
                            .map(|v| format!("{v}") == key_value)
                            .unwrap_or(false)
                    {
                        return event;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    // Fallback: check again before panicking
                    if let Some(event) =
                        self.match_event_correlated(source, event_type, key_value)
                    {
                        return event.clone();
                    }
                    panic!("EventBus closed while waiting for correlated event");
                }
            }
        }
    }

    /// Wait for an event matching the filter, checking existing events first.
    /// If not found in existing events, waits on the broadcast channel.
    pub async fn wait_for_event(&self, filter: &EventFilter) -> Event {
        // Fast path: check existing events
        if let Some(event) = self.match_event(filter) {
            return event.clone();
        }

        // Slow path: wait for broadcast
        let mut rx = self.sender.subscribe();
        loop {
            match rx.recv().await {
                Ok(event) => {
                    if event.source == filter.source && event.event_type == filter.event_type {
                        return event;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => {
                    // Channel closed; return last matching or panic
                    if let Some(event) = self.match_event(filter) {
                        return event.clone();
                    }
                    panic!("EventBus channel closed while waiting for event");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_ids_monotonic() {
        let mut bus = EventBus::new();
        bus.publish("src1".into(), "evt1".into(), HashMap::new());
        bus.publish("src1".into(), "evt2".into(), HashMap::new());
        bus.publish("src2".into(), "evt1".into(), HashMap::new());

        let events = bus.events();
        assert_eq!(events.len(), 3);
        assert!(events[0].id < events[1].id);
        assert!(events[1].id < events[2].id);
    }

    #[test]
    fn test_stream_isolation() {
        let mut bus = EventBus::new();
        bus.publish("log".into(), "append".into(), HashMap::new());
        bus.publish("index".into(), "lookup".into(), HashMap::new());
        bus.publish("log".into(), "commit".into(), HashMap::new());

        assert_eq!(bus.stream_events("log").len(), 2);
        assert_eq!(bus.stream_events("index").len(), 1);
        assert_eq!(bus.stream_events("nonexistent").len(), 0);
    }

    #[test]
    fn test_frontier_advancement() {
        let mut bus = EventBus::new();
        assert!(bus.frontier("log").is_none());

        let mut d1 = HashMap::new();
        d1.insert("offset".to_string(), Value::Int(1));
        bus.publish("log".into(), "append".into(), d1);
        let f1 = bus.frontier("log").unwrap();

        let mut d2 = HashMap::new();
        d2.insert("offset".to_string(), Value::Int(2));
        bus.publish("log".into(), "commit".into(), d2);
        let f2 = bus.frontier("log").unwrap();

        assert!(f2 > f1);
    }

    #[test]
    fn test_match_event_filter() {
        let mut bus = EventBus::new();
        bus.publish("log".into(), "append".into(), HashMap::new());
        bus.publish("log".into(), "commit".into(), HashMap::new());

        let filter = bus.subscribe("log", "commit");
        let matched = bus.match_event(&filter);
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().event_type, "commit");

        let filter2 = bus.subscribe("log", "nonexistent");
        assert!(bus.match_event(&filter2).is_none());
    }

    #[test]
    fn test_event_data_with_values() {
        let mut bus = EventBus::new();
        let mut data = HashMap::new();
        data.insert("key".to_string(), Value::String("hello".to_string()));
        data.insert("offset".to_string(), Value::Int(42));
        bus.publish("log".into(), "append".into(), data);

        let events = bus.stream_events("log");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].data.get("key"),
            Some(&Value::String("hello".to_string()))
        );
        assert_eq!(events[0].data.get("offset"), Some(&Value::Int(42)));
    }

    #[test]
    fn test_timestamps_monotonic() {
        let mut bus = EventBus::new();
        bus.publish("a".into(), "e1".into(), HashMap::new());
        bus.publish("b".into(), "e2".into(), HashMap::new());
        bus.publish("a".into(), "e3".into(), HashMap::new());

        let events = bus.events();
        assert!(events[0].timestamp < events[1].timestamp);
        assert!(events[1].timestamp < events[2].timestamp);
    }

    #[tokio::test]
    async fn test_wait_for_event_immediate() {
        let mut bus = EventBus::new();
        bus.publish("log".into(), "committed".into(), HashMap::new());

        let filter = EventFilter {
            source: "log".to_string(),
            event_type: "committed".to_string(),
        };
        let event = bus.wait_for_event(&filter).await;
        assert_eq!(event.event_type, "committed");
        assert_eq!(event.source, "log");
    }

    #[tokio::test]
    async fn test_wait_for_event_async() {
        let bus = EventBus::new();
        let sender = bus.sender.clone();

        // Spawn a task that publishes after a delay
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let event = Event {
                id: EventId::next(),
                timestamp: MONOTONIC_CLOCK.fetch_add(1, Ordering::Relaxed),
                source: "log".to_string(),
                event_type: "committed".to_string(),
                data: HashMap::new(),
            };
            let _ = sender.send(event);
        });

        let filter = EventFilter {
            source: "log".to_string(),
            event_type: "committed".to_string(),
        };
        let event = bus.wait_for_event(&filter).await;
        assert_eq!(event.event_type, "committed");
    }

    #[test]
    fn test_broadcast_subscribe() {
        let mut bus = EventBus::new();
        let mut rx = bus.subscribe_receiver();
        bus.publish("log".into(), "test".into(), HashMap::new());
        // Receiver should have the event
        let event = rx.try_recv().unwrap();
        assert_eq!(event.event_type, "test");
    }

    #[tokio::test]
    async fn test_wait_for_event_correlated_immediate() {
        let mut bus = EventBus::new();
        let mut data = HashMap::new();
        data.insert("key".to_string(), Value::String("abc".to_string()));
        bus.publish("log".into(), "committed".into(), data);

        let event = bus
            .wait_for_event_correlated("log", "committed", "abc")
            .await;
        assert_eq!(event.event_type, "committed");
        assert_eq!(event.source, "log");
        assert_eq!(
            event.data.get("key"),
            Some(&Value::String("abc".to_string()))
        );
    }

    #[tokio::test]
    async fn test_wait_for_event_correlated_async() {
        let bus = EventBus::new();
        let sender = bus.sender.clone();

        // Spawn a task that publishes a correlated event after a delay
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            let mut data = HashMap::new();
            data.insert("key".to_string(), Value::String("xyz".to_string()));
            let event = Event {
                id: EventId::next(),
                timestamp: MONOTONIC_CLOCK.fetch_add(1, Ordering::Relaxed),
                source: "log".to_string(),
                event_type: "committed".to_string(),
                data,
            };
            let _ = sender.send(event);
        });

        let event = bus
            .wait_for_event_correlated("log", "committed", "xyz")
            .await;
        assert_eq!(event.event_type, "committed");
        assert_eq!(
            event.data.get("key"),
            Some(&Value::String("xyz".to_string()))
        );
    }

    #[tokio::test]
    async fn test_wait_for_event_correlated_skips_non_matching() {
        let bus = EventBus::new();
        let sender = bus.sender.clone();

        tokio::spawn(async move {
            // First: non-matching key
            let mut data1 = HashMap::new();
            data1.insert("key".to_string(), Value::String("wrong".to_string()));
            let event1 = Event {
                id: EventId::next(),
                timestamp: MONOTONIC_CLOCK.fetch_add(1, Ordering::Relaxed),
                source: "log".to_string(),
                event_type: "committed".to_string(),
                data: data1,
            };
            let _ = sender.send(event1);

            // Second: non-matching source
            let mut data2 = HashMap::new();
            data2.insert("key".to_string(), Value::String("target".to_string()));
            let event2 = Event {
                id: EventId::next(),
                timestamp: MONOTONIC_CLOCK.fetch_add(1, Ordering::Relaxed),
                source: "other".to_string(),
                event_type: "committed".to_string(),
                data: data2,
            };
            let _ = sender.send(event2);

            // Third: matching
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            let mut data3 = HashMap::new();
            data3.insert("key".to_string(), Value::String("target".to_string()));
            let event3 = Event {
                id: EventId::next(),
                timestamp: MONOTONIC_CLOCK.fetch_add(1, Ordering::Relaxed),
                source: "log".to_string(),
                event_type: "committed".to_string(),
                data: data3,
            };
            let _ = sender.send(event3);
        });

        let event = bus
            .wait_for_event_correlated("log", "committed", "target")
            .await;
        assert_eq!(event.source, "log");
        assert_eq!(event.event_type, "committed");
        assert_eq!(
            event.data.get("key"),
            Some(&Value::String("target".to_string()))
        );
    }

    #[test]
    fn test_match_event_frontier_immediate() {
        let mut bus = EventBus::new();
        let mut d1 = HashMap::new();
        d1.insert("offset".to_string(), Value::Int(1));
        bus.publish("log".into(), "entry_available".into(), d1);

        let mut d2 = HashMap::new();
        d2.insert("offset".to_string(), Value::Int(2));
        bus.publish("log".into(), "entry_available".into(), d2);

        // Match with min_offset <= first event offset
        let matched = bus.match_event_frontier("log", "entry_available", 1);
        assert!(matched.is_some());
        // Should return the most recent (second) event since both qualify
        assert_eq!(matched.unwrap().data.get("offset"), Some(&Value::Int(2)));

        // Match with min_offset == second event offset
        let matched = bus.match_event_frontier("log", "entry_available", 2);
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().data.get("offset"), Some(&Value::Int(2)));

        // Match with min_offset > second event — no match
        let matched = bus.match_event_frontier("log", "entry_available", 3);
        assert!(matched.is_none());
    }

    #[test]
    fn test_match_event_frontier_wrong_source() {
        let mut bus = EventBus::new();
        let mut d = HashMap::new();
        d.insert("offset".to_string(), Value::Int(1));
        bus.publish("log".into(), "entry_available".into(), d);

        assert!(bus.match_event_frontier("other", "entry_available", 1).is_none());
    }

    #[test]
    fn test_match_event_frontier_wrong_type() {
        let mut bus = EventBus::new();
        let mut d = HashMap::new();
        d.insert("offset".to_string(), Value::Int(1));
        bus.publish("log".into(), "entry_available".into(), d);

        assert!(bus.match_event_frontier("log", "committed", 1).is_none());
    }

    #[test]
    fn test_match_event_frontier_boundary() {
        let mut bus = EventBus::new();
        // Publish events of mixed types with offsets
        let mut d1 = HashMap::new();
        d1.insert("offset".to_string(), Value::Int(1));
        bus.publish("log".into(), "append".into(), d1);

        let mut d2 = HashMap::new();
        d2.insert("offset".to_string(), Value::Int(2));
        bus.publish("log".into(), "entry_available".into(), d2);

        let mut d3 = HashMap::new();
        d3.insert("offset".to_string(), Value::Int(3));
        bus.publish("log".into(), "append".into(), d3);

        // Should only match entry_available, not append
        let matched = bus.match_event_frontier("log", "entry_available", 2);
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().data.get("offset"), Some(&Value::Int(2)));

        // Frontier before all events should still match entry_available
        let matched = bus.match_event_frontier("log", "entry_available", 0);
        assert!(matched.is_some());
    }

    #[test]
    fn test_match_event_frontier_no_offset_field() {
        // Events without data["offset"] should not match frontier queries
        let mut bus = EventBus::new();
        bus.publish("log".into(), "entry_available".into(), HashMap::new());
        assert!(bus.match_event_frontier("log", "entry_available", 1).is_none());
    }

    #[test]
    fn test_lamport_time_in_events() {
        let mut bus = EventBus::new();
        bus.publish("log".into(), "append".into(), HashMap::new());
        bus.publish("log".into(), "commit".into(), HashMap::new());

        let events = bus.events();
        // Each event should have a lamport_time in its data
        let lt1 = match events[0].data.get("lamport_time") {
            Some(Value::Int(n)) => *n,
            _ => panic!("expected lamport_time in event data"),
        };
        let lt2 = match events[1].data.get("lamport_time") {
            Some(Value::Int(n)) => *n,
            _ => panic!("expected lamport_time in event data"),
        };
        assert!(lt2 > lt1, "lamport times should be monotonically increasing");
    }

    #[test]
    fn test_lamport_merge() {
        let mut bus = EventBus::new();
        bus.publish("log".into(), "append".into(), HashMap::new());
        assert_eq!(bus.lamport_time(), 1);

        // Simulate receiving a remote timestamp
        bus.merge_lamport(10);
        assert_eq!(bus.lamport_time(), 11);

        // Next publish should use lamport > 11
        bus.publish("log".into(), "commit".into(), HashMap::new());
        let events = bus.events();
        let lt = match events[1].data.get("lamport_time") {
            Some(Value::Int(n)) => *n,
            _ => panic!("expected lamport_time"),
        };
        assert_eq!(lt, 12);
    }

    #[test]
    fn test_node_id() {
        let mut bus = EventBus::new();
        assert_eq!(bus.node_id(), "");

        bus.set_node_id("node-42");
        assert_eq!(bus.node_id(), "node-42");
    }

    #[test]
    fn test_persistence_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.ndjson");

        // Create bus with persistence and publish events
        {
            let mut bus = EventBus::with_persistence(&log_path, "node1").unwrap();
            bus.publish("log".into(), "append".into(), HashMap::new());

            let mut data = HashMap::new();
            data.insert("key".to_string(), Value::String("hello".to_string()));
            bus.publish("kv".into(), "put_ack".into(), data);

            assert_eq!(bus.events().len(), 2);
        }

        // Create new bus and recover
        {
            let mut bus = EventBus::with_persistence(&log_path, "node1").unwrap();
            let count = bus.recover().unwrap();
            assert_eq!(count, 2);
            assert_eq!(bus.events().len(), 2);
            assert_eq!(bus.events()[0].source, "log");
            assert_eq!(bus.events()[1].source, "kv");
            assert_eq!(
                bus.events()[1].data.get("key"),
                Some(&Value::String("hello".to_string()))
            );
        }
    }

    #[test]
    fn test_persistence_lamport_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.ndjson");

        // First session: publish events, lamport should advance
        {
            let mut bus = EventBus::with_persistence(&log_path, "node1").unwrap();
            bus.publish("log".into(), "e1".into(), HashMap::new());
            bus.publish("log".into(), "e2".into(), HashMap::new());
            bus.publish("log".into(), "e3".into(), HashMap::new());
            assert_eq!(bus.lamport_time(), 3);
        }

        // Second session: recover and verify lamport continues from where it left off
        {
            let mut bus = EventBus::with_persistence(&log_path, "node1").unwrap();
            bus.recover().unwrap();
            // After recovery, lamport should be at least 3
            assert!(bus.lamport_time() >= 3);

            // New events should have lamport > 3
            bus.publish("log".into(), "e4".into(), HashMap::new());
            let events = bus.events();
            let last_lt = match events.last().unwrap().data.get("lamport_time") {
                Some(Value::Int(n)) => *n,
                _ => panic!("expected lamport_time"),
            };
            assert!(last_lt > 3);
        }
    }

    // === Gap 11: frontier_for filters by event type ===

    #[test]
    fn test_frontier_for_filters_by_event_type() {
        let mut bus = EventBus::new();

        // Publish different event types with different offsets
        let mut d1 = HashMap::new();
        d1.insert("offset".to_string(), Value::Int(1));
        bus.publish("kv".into(), "put".into(), d1);

        let mut d2 = HashMap::new();
        d2.insert("offset".to_string(), Value::Int(2));
        bus.publish("kv".into(), "delete".into(), d2);

        let mut d3 = HashMap::new();
        d3.insert("offset".to_string(), Value::Int(3));
        bus.publish("kv".into(), "put".into(), d3);

        // frontier() returns the latest offset regardless of type
        assert_eq!(bus.frontier("kv"), Some(3));

        // frontier_for("put") should return 3 (the latest put)
        assert_eq!(bus.frontier_for("kv", "put"), Some(3));

        // frontier_for("delete") should return 2 (the only delete)
        assert_eq!(bus.frontier_for("kv", "delete"), Some(2));

        // frontier_for("nonexistent") should return None
        assert_eq!(bus.frontier_for("kv", "nonexistent"), None);
    }

    #[test]
    fn test_frontier_for_empty_stream() {
        let bus = EventBus::new();
        assert_eq!(bus.frontier_for("nonexistent", "put"), None);
    }

    // === Gap 5: subscribe_source / poll_source ===

    #[test]
    fn test_subscribe_source_filters_by_type() {
        let mut bus = EventBus::new();
        bus.publish("log".into(), "append".into(), HashMap::new());
        bus.publish("log".into(), "commit".into(), HashMap::new());
        bus.publish("log".into(), "append".into(), HashMap::new());

        let appends = bus.subscribe_source("log", "append");
        assert_eq!(appends.len(), 2);
        assert!(appends.iter().all(|e| e.event_type == "append"));

        let commits = bus.subscribe_source("log", "commit");
        assert_eq!(commits.len(), 1);
    }

    #[test]
    fn test_subscribe_source_empty_stream() {
        let bus = EventBus::new();
        let events = bus.subscribe_source("nonexistent", "append");
        assert!(events.is_empty());
    }

    #[test]
    fn test_poll_source_after_offset() {
        let mut bus = EventBus::new();
        let mut d1 = HashMap::new();
        d1.insert("offset".to_string(), Value::Int(1));
        bus.publish("log".into(), "append".into(), d1);

        let mut d2 = HashMap::new();
        d2.insert("offset".to_string(), Value::Int(2));
        bus.publish("log".into(), "append".into(), d2);

        let mut d3 = HashMap::new();
        d3.insert("offset".to_string(), Value::Int(3));
        bus.publish("log".into(), "append".into(), d3);

        // Poll after offset 1 should return the event with offset 2
        let event = bus.poll_source("log", Some(1));
        assert!(event.is_some());
        assert_eq!(event.unwrap().data.get("offset"), Some(&Value::Int(2)));

        // Poll after offset 3 should return None
        let event = bus.poll_source("log", Some(3));
        assert!(event.is_none());
    }

    #[test]
    fn test_poll_source_none_offset_returns_first() {
        let mut bus = EventBus::new();
        let mut d1 = HashMap::new();
        d1.insert("offset".to_string(), Value::Int(10));
        bus.publish("log".into(), "append".into(), d1);

        let event = bus.poll_source("log", None);
        assert!(event.is_some());
        assert_eq!(event.unwrap().data.get("offset"), Some(&Value::Int(10)));
    }

    // === Gap 2: match_event_predicate / wait_for_event_predicate ===

    #[test]
    fn test_match_event_predicate_basic() {
        let mut bus = EventBus::new();
        let mut d = HashMap::new();
        d.insert("key".to_string(), Value::String("hello".to_string()));
        bus.publish("log".into(), "commit".into(), d);

        let matched = bus.match_event_predicate("log", |e| {
            e.data.get("key") == Some(&Value::String("hello".to_string()))
        });
        assert!(matched.is_some());
        assert_eq!(matched.unwrap().event_type, "commit");
    }

    #[test]
    fn test_match_event_predicate_no_match() {
        let mut bus = EventBus::new();
        bus.publish("log".into(), "commit".into(), HashMap::new());

        let matched = bus.match_event_predicate("log", |e| {
            e.data.get("key") == Some(&Value::String("nonexistent".to_string()))
        });
        assert!(matched.is_none());
    }

    #[tokio::test]
    async fn test_wait_for_event_predicate_immediate() {
        let mut bus = EventBus::new();
        let mut d = HashMap::new();
        d.insert("key".to_string(), Value::String("target".to_string()));
        bus.publish("log".into(), "commit".into(), d);

        let event = bus
            .wait_for_event_predicate("log", |e| {
                e.data.get("key") == Some(&Value::String("target".to_string()))
            })
            .await;
        assert_eq!(event.event_type, "commit");
    }
}
