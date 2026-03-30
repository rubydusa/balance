use std::collections::HashMap;

/// Lamport logical clock for establishing causal ordering of events
/// across distributed nodes. Each node maintains its own counter that
/// is incremented on local events and merged on receiving remote events.
#[derive(Debug, Clone)]
pub struct LamportClock {
    counter: u64,
}

impl LamportClock {
    pub fn new() -> Self {
        Self { counter: 0 }
    }

    /// Tick the clock (local event) and return the new timestamp.
    pub fn tick(&mut self) -> u64 {
        self.counter += 1;
        self.counter
    }

    /// Merge with a received timestamp from another node.
    /// Sets counter to max(local, received) + 1.
    pub fn merge(&mut self, received: u64) -> u64 {
        self.counter = self.counter.max(received) + 1;
        self.counter
    }

    /// Get the current timestamp without advancing.
    pub fn current(&self) -> u64 {
        self.counter
    }
}

impl Default for LamportClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Vector clock for tracking causal dependencies across multiple nodes.
/// Each entry maps a node ID to its latest known counter value.
/// Infrastructure for future multi-node causal consistency tracking.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorClock {
    entries: HashMap<String, u64>,
}

#[allow(dead_code)]
impl VectorClock {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Tick the clock for the given node (local event).
    pub fn tick(&mut self, node_id: &str) {
        let counter = self.entries.entry(node_id.to_string()).or_insert(0);
        *counter += 1;
    }

    /// Merge with another vector clock (take component-wise max).
    pub fn merge(&mut self, other: &VectorClock) {
        for (node, &count) in &other.entries {
            let entry = self.entries.entry(node.clone()).or_insert(0);
            *entry = (*entry).max(count);
        }
    }

    /// Check if this clock happened before (or is concurrent with) another.
    /// Returns true if every entry in self is <= the corresponding entry in other,
    /// and at least one entry is strictly less.
    pub fn happened_before(&self, other: &VectorClock) -> bool {
        let mut at_least_one_less = false;

        // Check all entries in self are <= other
        for (node, &self_count) in &self.entries {
            let other_count = other.entries.get(node).copied().unwrap_or(0);
            if self_count > other_count {
                return false;
            }
            if self_count < other_count {
                at_least_one_less = true;
            }
        }

        // Check entries in other that self doesn't have (implicit 0 in self)
        for (node, &other_count) in &other.entries {
            if !self.entries.contains_key(node) && other_count > 0 {
                at_least_one_less = true;
            }
        }

        at_least_one_less
    }

    /// Check if two vector clocks are concurrent (neither happened before the other).
    pub fn concurrent(&self, other: &VectorClock) -> bool {
        !self.happened_before(other) && !other.happened_before(self) && self != other
    }

    /// Get the counter value for a specific node.
    pub fn get(&self, node_id: &str) -> u64 {
        self.entries.get(node_id).copied().unwrap_or(0)
    }

    /// Get all entries in the vector clock.
    pub fn entries(&self) -> &HashMap<String, u64> {
        &self.entries
    }
}

#[allow(dead_code)]
impl Default for VectorClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Lamport Clock Tests ---

    #[test]
    fn test_lamport_new() {
        let clock = LamportClock::new();
        assert_eq!(clock.current(), 0);
    }

    #[test]
    fn test_lamport_tick() {
        let mut clock = LamportClock::new();
        assert_eq!(clock.tick(), 1);
        assert_eq!(clock.tick(), 2);
        assert_eq!(clock.tick(), 3);
        assert_eq!(clock.current(), 3);
    }

    #[test]
    fn test_lamport_merge_higher() {
        let mut clock = LamportClock::new();
        clock.tick(); // 1
        clock.tick(); // 2

        // Merge with higher remote value
        let result = clock.merge(10);
        assert_eq!(result, 11); // max(2, 10) + 1
        assert_eq!(clock.current(), 11);
    }

    #[test]
    fn test_lamport_merge_lower() {
        let mut clock = LamportClock::new();
        clock.tick(); // 1
        clock.tick(); // 2
        clock.tick(); // 3
        clock.tick(); // 4
        clock.tick(); // 5

        // Merge with lower remote value
        let result = clock.merge(2);
        assert_eq!(result, 6); // max(5, 2) + 1
    }

    #[test]
    fn test_lamport_merge_equal() {
        let mut clock = LamportClock::new();
        clock.tick(); // 1
        clock.tick(); // 2
        clock.tick(); // 3

        let result = clock.merge(3);
        assert_eq!(result, 4); // max(3, 3) + 1
    }

    // --- Vector Clock Tests ---

    #[test]
    fn test_vector_clock_new() {
        let vc = VectorClock::new();
        assert_eq!(vc.get("a"), 0);
        assert!(vc.entries().is_empty());
    }

    #[test]
    fn test_vector_clock_tick() {
        let mut vc = VectorClock::new();
        vc.tick("a");
        assert_eq!(vc.get("a"), 1);
        vc.tick("a");
        assert_eq!(vc.get("a"), 2);
        vc.tick("b");
        assert_eq!(vc.get("b"), 1);
    }

    #[test]
    fn test_vector_clock_merge() {
        let mut vc1 = VectorClock::new();
        vc1.tick("a"); // a:1
        vc1.tick("a"); // a:2

        let mut vc2 = VectorClock::new();
        vc2.tick("b"); // b:1
        vc2.tick("b"); // b:2
        vc2.tick("a"); // a:1

        vc1.merge(&vc2);
        assert_eq!(vc1.get("a"), 2); // max(2, 1)
        assert_eq!(vc1.get("b"), 2); // max(0, 2)
    }

    #[test]
    fn test_vector_clock_happened_before() {
        let mut vc1 = VectorClock::new();
        vc1.tick("a"); // a:1

        let mut vc2 = VectorClock::new();
        vc2.tick("a"); // a:1
        vc2.tick("a"); // a:2

        assert!(vc1.happened_before(&vc2));
        assert!(!vc2.happened_before(&vc1));
    }

    #[test]
    fn test_vector_clock_happened_before_multinode() {
        let mut vc1 = VectorClock::new();
        vc1.tick("a"); // a:1
        vc1.tick("b"); // b:1

        let mut vc2 = VectorClock::new();
        vc2.tick("a"); // a:1
        vc2.tick("b"); // b:1
        vc2.tick("a"); // a:2

        assert!(vc1.happened_before(&vc2));
        assert!(!vc2.happened_before(&vc1));
    }

    #[test]
    fn test_vector_clock_concurrent() {
        let mut vc1 = VectorClock::new();
        vc1.tick("a"); // a:1

        let mut vc2 = VectorClock::new();
        vc2.tick("b"); // b:1

        // Neither happened before the other
        assert!(!vc1.happened_before(&vc2));
        assert!(!vc2.happened_before(&vc1));
        assert!(vc1.concurrent(&vc2));
    }

    #[test]
    fn test_vector_clock_not_concurrent_when_equal() {
        let mut vc1 = VectorClock::new();
        vc1.tick("a"); // a:1

        let vc2 = vc1.clone();

        // Equal clocks are not concurrent (they represent the same state)
        assert!(!vc1.concurrent(&vc2));
        assert!(!vc1.happened_before(&vc2));
    }

    #[test]
    fn test_vector_clock_concurrent_complex() {
        // a increments a, b increments b, then each merges but also increments their own
        let mut vc1 = VectorClock::new();
        vc1.tick("a"); // a:1
        vc1.tick("a"); // a:2
        vc1.tick("b"); // b:1

        let mut vc2 = VectorClock::new();
        vc2.tick("b"); // b:1
        vc2.tick("b"); // b:2
        vc2.tick("a"); // a:1

        // vc1 has a:2, b:1 — vc2 has a:1, b:2
        // vc1.a > vc2.a, but vc1.b < vc2.b → concurrent
        assert!(vc1.concurrent(&vc2));
    }
}
