use std::collections::{HashMap, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::balance_substrate::BalanceSubstrate;
use super::coordinator::{Coordinator, SubstrateStorage, InMemoryStorage};
use super::event::EventBus;
use super::interaction::InteractionKind;
use super::value::Value;

// No global statics: each substrate instance maintains its own monotonic
// offset counter, ensuring offsets are substrate-local and portable across nodes.

/// A substrate is a mechanism that produces events. Services use substrates
/// to provide durability, replication, and ordering guarantees.
pub trait Substrate: std::any::Any {
    fn name(&self) -> &str;
    fn event_source(&self) -> &str;
    fn execute_op(
        &mut self,
        op: &str,
        args: Vec<Value>,
        event_bus: &mut EventBus,
    ) -> Result<Value, String>;
    fn guarantees(&self) -> &[String];
    /// Classify an operation as Command, Query, or Pure.
    fn op_kind(&self, _op: &str) -> InteractionKind {
        InteractionKind::Pure
    }
    /// Set the next offset for this substrate (used for recovery).
    fn set_next_offset(&mut self, _offset: u64) {}
    /// Set a coordinator for replicated writes. Only meaningful for substrates
    /// that support replication (ReplicatedLog, KeyValueStore).
    fn set_coordinator(&mut self, _coordinator: Box<dyn Coordinator>) {}
    /// Compensate (revert) a previously executed operation.
    /// Used by implicit block-level atomicity to undo writes on failure.
    /// Default is no-op for read-only substrates (Clock, Crypto).
    fn compensate_op(
        &mut self,
        _op: &str,
        _args: Vec<Value>,
        _event_bus: &mut EventBus,
    ) -> Result<(), String> {
        Ok(())
    }
    /// Peek at a stored value without emitting events. Used for snapshotting
    /// before writes so compensation can restore the previous value.
    fn peek_value(&self, _key: &str) -> Option<Value> {
        None
    }
    /// Downcast support for substrate composition.
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
}

/// Replicated log substrate. In the current runtime, append is immediately
/// committed. In a distributed deployment, the substrate implementation would
/// coordinate quorum before emitting `quorum_committed`.
pub struct ReplicatedLog {
    instance_name: String,
    event_source: String,
    storage: Box<dyn SubstrateStorage>,
    log_len: usize,
    guarantees: Vec<String>,
    /// Monotonic offset counter local to this substrate instance.
    /// Each event published by this substrate gets a unique, increasing offset.
    next_offset: u64,
    /// Optional coordinator for replicated writes.
    coordinator: Option<Box<dyn Coordinator>>,
}

impl ReplicatedLog {
    pub fn new(instance_name: &str, event_source: &str) -> Self {
        Self {
            instance_name: instance_name.to_string(),
            event_source: event_source.to_string(),
            storage: Box::new(InMemoryStorage::new()),
            log_len: 0,
            guarantees: vec![
                "commit_requires_accept".to_string(),
                "committed".to_string(),
            ],
            next_offset: 1,
            coordinator: None,
        }
    }

    /// Create a replicated log backed by the given storage implementation.
    pub fn with_storage(instance_name: &str, event_source: &str, storage: Box<dyn SubstrateStorage>) -> Self {
        let log_len = storage.list_keys().len();
        Self {
            instance_name: instance_name.to_string(),
            event_source: event_source.to_string(),
            storage,
            log_len,
            guarantees: vec![
                "commit_requires_accept".to_string(),
                "committed".to_string(),
            ],
            next_offset: 1,
            coordinator: None,
        }
    }

    /// Create a replicated log with a coordinator for distributed writes.
    pub fn with_coordinator(instance_name: &str, event_source: &str, coordinator: Box<dyn Coordinator>) -> Self {
        Self {
            instance_name: instance_name.to_string(),
            event_source: event_source.to_string(),
            storage: Box::new(InMemoryStorage::new()),
            log_len: 0,
            guarantees: vec![
                "commit_requires_accept".to_string(),
                "committed".to_string(),
            ],
            next_offset: 1,
            coordinator: Some(coordinator),
        }
    }

    /// Set the coordinator after construction.
    pub fn set_coordinator(&mut self, coordinator: Box<dyn Coordinator>) {
        self.coordinator = Some(coordinator);
    }

    fn append(&mut self, value: Value, event_bus: &mut EventBus) -> Value {
        // If coordinator is present, propose through it for replicated writes
        if let Some(ref mut coord) = self.coordinator {
            let key = format!("log_{}", self.log_len);
            if let Err(e) = coord.propose(&key, value.clone()) {
                return Value::String(format!("replication error: {e}"));
            }
        }

        let accept_offset = self.next_offset;
        self.next_offset += 1;
        let idx = self.log_len;
        self.log_len += 1;
        let _ = self.storage.put(&idx.to_string(), value.clone());

        let ack_key = format!("offset_{accept_offset}");

        // Emit append_accepted event
        let mut accept_data = HashMap::new();
        accept_data.insert("key".to_string(), Value::String(ack_key.clone()));
        accept_data.insert("offset".to_string(), Value::Int(accept_offset as i64));
        accept_data.insert("entry".to_string(), value.clone());
        event_bus.publish(
            self.event_source.clone(),
            "append_accepted".to_string(),
            accept_data,
        );

        // Emit quorum_committed (in distributed deployment, this would
        // follow actual quorum coordination)
        let commit_offset = self.next_offset;
        self.next_offset += 1;
        let mut commit_data = HashMap::new();
        commit_data.insert("key".to_string(), Value::String(ack_key.clone()));
        commit_data.insert("offset".to_string(), Value::Int(commit_offset as i64));
        commit_data.insert("entry".to_string(), value);
        event_bus.publish(
            self.event_source.clone(),
            "quorum_committed".to_string(),
            commit_data,
        );

        Value::ack(ack_key)
    }

    fn read(&self, offset: usize, event_bus: &mut EventBus) -> Value {
        let value = self.storage.get(&offset.to_string()).cloned().unwrap_or(Value::None);

        // Emit entry_available event with substrate-local offset (current frontier)
        let mut data = HashMap::new();
        data.insert("offset".to_string(), Value::Int(self.next_offset as i64));
        data.insert("entry".to_string(), value.clone());
        event_bus.publish(
            self.event_source.clone(),
            "entry_available".to_string(),
            data,
        );

        value
    }
}

impl Substrate for ReplicatedLog {
    fn name(&self) -> &str {
        &self.instance_name
    }

    fn event_source(&self) -> &str {
        &self.event_source
    }

    fn execute_op(
        &mut self,
        op: &str,
        args: Vec<Value>,
        event_bus: &mut EventBus,
    ) -> Result<Value, String> {
        match op {
            "append" => {
                let value = args.into_iter().next().unwrap_or(Value::None);
                Ok(self.append(value, event_bus))
            }
            "read" => {
                let offset = match args.first() {
                    Some(Value::Int(n)) => *n as usize,
                    _ => return Err("read requires an Int offset argument".to_string()),
                };
                Ok(self.read(offset, event_bus))
            }
            _ => Err(format!(
                "substrate '{}' has no operation '{op}'",
                self.instance_name
            )),
        }
    }

    fn guarantees(&self) -> &[String] {
        &self.guarantees
    }

    fn op_kind(&self, op: &str) -> InteractionKind {
        match op {
            "append" => InteractionKind::Command,
            "read" => InteractionKind::Query,
            _ => InteractionKind::Pure,
        }
    }

    fn set_next_offset(&mut self, offset: u64) {
        self.next_offset = offset;
    }

    fn set_coordinator(&mut self, coordinator: Box<dyn Coordinator>) {
        self.coordinator = Some(coordinator);
    }

    fn compensate_op(
        &mut self,
        op: &str,
        args: Vec<Value>,
        event_bus: &mut EventBus,
    ) -> Result<(), String> {
        match op {
            "append" => {
                // Append-only log: structural undo not possible.
                // Emit append_reverted event with the ack key from args.
                let ack_key = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    _ => "unknown".to_string(),
                };
                let mut data = HashMap::new();
                data.insert("key".to_string(), Value::String(ack_key));
                event_bus.publish(
                    self.event_source.clone(),
                    "append_reverted".to_string(),
                    data,
                );
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

/// Key-value store substrate. Provides put, get, and delete operations
/// with settlement events for commands and observation events for queries.
pub struct KeyValueStore {
    instance_name: String,
    event_source: String,
    storage: Box<dyn SubstrateStorage>,
    guarantees: Vec<String>,
    /// Monotonic offset counter local to this substrate instance.
    next_offset: u64,
    /// Optional coordinator for replicated writes.
    coordinator: Option<Box<dyn Coordinator>>,
}

impl KeyValueStore {
    pub fn new(instance_name: &str, event_source: &str) -> Self {
        Self {
            instance_name: instance_name.to_string(),
            event_source: event_source.to_string(),
            storage: Box::new(InMemoryStorage::new()),
            guarantees: vec!["put_requires_accept".to_string()],
            next_offset: 1,
            coordinator: None,
        }
    }

    /// Create a KV store backed by the given storage implementation.
    pub fn with_storage(instance_name: &str, event_source: &str, storage: Box<dyn SubstrateStorage>) -> Self {
        Self {
            instance_name: instance_name.to_string(),
            event_source: event_source.to_string(),
            storage,
            guarantees: vec!["put_requires_accept".to_string()],
            next_offset: 1,
            coordinator: None,
        }
    }

    /// Create a KV store with a coordinator for distributed writes.
    pub fn with_coordinator(instance_name: &str, event_source: &str, coordinator: Box<dyn Coordinator>) -> Self {
        Self {
            instance_name: instance_name.to_string(),
            event_source: event_source.to_string(),
            storage: Box::new(InMemoryStorage::new()),
            guarantees: vec!["put_requires_accept".to_string()],
            next_offset: 1,
            coordinator: Some(coordinator),
        }
    }

    /// Set the coordinator after construction.
    pub fn set_coordinator(&mut self, coordinator: Box<dyn Coordinator>) {
        self.coordinator = Some(coordinator);
    }

    fn put(&mut self, key: String, value: Value, event_bus: &mut EventBus) -> Value {
        // If coordinator is present, propose through it for replicated writes
        if let Some(ref mut coord) = self.coordinator {
            if let Err(e) = coord.propose(&key, value.clone()) {
                return Value::String(format!("replication error: {e}"));
            }
        }

        let accept_offset = self.next_offset;
        self.next_offset += 1;
        let ack_key = format!("kv_put_{accept_offset}_{key}");
        let _ = self.storage.put(&key, value.clone());

        // Emit put_accepted event
        // data["key"] = ack key (for settlement correlation), data["kv_key"] = original key
        let mut accept_data = HashMap::new();
        accept_data.insert("offset".to_string(), Value::Int(accept_offset as i64));
        accept_data.insert("key".to_string(), Value::String(ack_key.clone()));
        accept_data.insert("kv_key".to_string(), Value::String(key.clone()));
        accept_data.insert("value".to_string(), value);
        event_bus.publish(
            self.event_source.clone(),
            "put_accepted".to_string(),
            accept_data,
        );

        // Emit put_committed (in distributed deployment, this would
        // follow actual quorum coordination)
        let commit_offset = self.next_offset;
        self.next_offset += 1;
        let mut commit_data = HashMap::new();
        commit_data.insert("offset".to_string(), Value::Int(commit_offset as i64));
        commit_data.insert("key".to_string(), Value::String(ack_key.clone()));
        commit_data.insert("kv_key".to_string(), Value::String(key));
        event_bus.publish(
            self.event_source.clone(),
            "put_committed".to_string(),
            commit_data,
        );

        Value::ack(ack_key)
    }

    fn get(&self, key: &str, event_bus: &mut EventBus) -> Value {
        let value = self.storage.get(key).cloned().unwrap_or(Value::None);

        let mut data = HashMap::new();
        data.insert("offset".to_string(), Value::Int(self.next_offset as i64));
        data.insert("key".to_string(), Value::String(key.to_string()));
        data.insert("value".to_string(), value.clone());
        event_bus.publish(
            self.event_source.clone(),
            "entry_available".to_string(),
            data,
        );

        value
    }

    fn delete(&mut self, key: String, event_bus: &mut EventBus) -> Value {
        // If coordinator is present, propose deletion through it
        if let Some(ref mut coord) = self.coordinator {
            if let Err(e) = coord.propose(&format!("__delete_{key}"), Value::None) {
                return Value::String(format!("replication error: {e}"));
            }
        }

        let del_offset = self.next_offset;
        self.next_offset += 1;
        let ack_key = format!("kv_del_{del_offset}_{key}");
        let _ = self.storage.delete(&key);

        let mut data = HashMap::new();
        data.insert("offset".to_string(), Value::Int(del_offset as i64));
        data.insert("key".to_string(), Value::String(ack_key.clone()));
        data.insert("kv_key".to_string(), Value::String(key));
        event_bus.publish(
            self.event_source.clone(),
            "delete_committed".to_string(),
            data,
        );

        Value::ack(ack_key)
    }
}

impl Substrate for KeyValueStore {
    fn name(&self) -> &str {
        &self.instance_name
    }

    fn event_source(&self) -> &str {
        &self.event_source
    }

    fn execute_op(
        &mut self,
        op: &str,
        args: Vec<Value>,
        event_bus: &mut EventBus,
    ) -> Result<Value, String> {
        match op {
            "put" => {
                let key = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    _ => return Err("put requires a String key as first argument".to_string()),
                };
                let value = args.into_iter().nth(1).unwrap_or(Value::None);
                Ok(self.put(key, value, event_bus))
            }
            "get" => {
                let key = match args.first() {
                    Some(Value::String(s)) => s.as_str(),
                    _ => return Err("get requires a String key argument".to_string()),
                };
                Ok(self.get(key, event_bus))
            }
            "delete" => {
                let key = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    _ => return Err("delete requires a String key argument".to_string()),
                };
                Ok(self.delete(key, event_bus))
            }
            _ => Err(format!(
                "substrate '{}' has no operation '{op}'",
                self.instance_name
            )),
        }
    }

    fn guarantees(&self) -> &[String] {
        &self.guarantees
    }

    fn op_kind(&self, op: &str) -> InteractionKind {
        match op {
            "put" | "delete" => InteractionKind::Command,
            "get" => InteractionKind::Query,
            _ => InteractionKind::Pure,
        }
    }

    fn set_next_offset(&mut self, offset: u64) {
        self.next_offset = offset;
    }

    fn set_coordinator(&mut self, coordinator: Box<dyn Coordinator>) {
        self.coordinator = Some(coordinator);
    }

    fn compensate_op(
        &mut self,
        op: &str,
        args: Vec<Value>,
        event_bus: &mut EventBus,
    ) -> Result<(), String> {
        match op {
            "put" => {
                // args: [key, Option<prev_value>]
                let key = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    _ => return Err("compensate put: missing key".to_string()),
                };
                let prev_value = args.get(1).cloned();
                match prev_value {
                    Some(Value::None) | None => {
                        // No previous value — delete to revert
                        let _ = self.storage.delete(&key);
                    }
                    Some(val) => {
                        // Restore previous value
                        let _ = self.storage.put(&key, val);
                    }
                }
                let mut data = HashMap::new();
                data.insert("kv_key".to_string(), Value::String(key));
                event_bus.publish(
                    self.event_source.clone(),
                    "put_reverted".to_string(),
                    data,
                );
                Ok(())
            }
            "delete" => {
                // args: [key, saved_value]
                let key = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    _ => return Err("compensate delete: missing key".to_string()),
                };
                if let Some(saved_val) = args.get(1).cloned() {
                    let _ = self.storage.put(&key, saved_val);
                }
                let mut data = HashMap::new();
                data.insert("kv_key".to_string(), Value::String(key));
                event_bus.publish(
                    self.event_source.clone(),
                    "delete_reverted".to_string(),
                    data,
                );
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn peek_value(&self, key: &str) -> Option<Value> {
        self.storage.get(key).cloned()
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

/// Queue substrate. Provides enqueue, dequeue, and peek operations.
/// Uses a VecDeque for in-memory queue ordering, with SubstrateStorage for
/// persistence of the backing data.
pub struct Queue {
    instance_name: String,
    event_source: String,
    queue: VecDeque<Value>,
    guarantees: Vec<String>,
    /// Monotonic offset counter local to this substrate instance.
    next_offset: u64,
}

impl Queue {
    pub fn new(instance_name: &str, event_source: &str) -> Self {
        Self {
            instance_name: instance_name.to_string(),
            event_source: event_source.to_string(),
            queue: VecDeque::new(),
            guarantees: vec!["enqueue_requires_accept".to_string()],
            next_offset: 1,
        }
    }

    fn enqueue(&mut self, value: Value, event_bus: &mut EventBus) -> Value {
        let accept_offset = self.next_offset;
        self.next_offset += 1;
        let ack_key = format!("queue_item_{accept_offset}");
        self.queue.push_back(value.clone());

        let mut accept_data = HashMap::new();
        accept_data.insert("offset".to_string(), Value::Int(accept_offset as i64));
        accept_data.insert("key".to_string(), Value::String(ack_key.clone()));
        accept_data.insert("value".to_string(), value);
        event_bus.publish(
            self.event_source.clone(),
            "enqueue_accepted".to_string(),
            accept_data,
        );

        let commit_offset = self.next_offset;
        self.next_offset += 1;
        let mut commit_data = HashMap::new();
        commit_data.insert("offset".to_string(), Value::Int(commit_offset as i64));
        commit_data.insert("key".to_string(), Value::String(ack_key.clone()));
        event_bus.publish(
            self.event_source.clone(),
            "enqueue_committed".to_string(),
            commit_data,
        );

        Value::ack(ack_key)
    }

    fn dequeue(&mut self, event_bus: &mut EventBus) -> Value {
        let value = self.queue.pop_front().unwrap_or(Value::None);

        let mut data = HashMap::new();
        data.insert("offset".to_string(), Value::Int(self.next_offset as i64));
        data.insert("value".to_string(), value.clone());
        event_bus.publish(
            self.event_source.clone(),
            "dequeue_available".to_string(),
            data,
        );

        value
    }

    fn peek(&self, event_bus: &mut EventBus) -> Value {
        let value = self.queue.front().cloned().unwrap_or(Value::None);

        let mut data = HashMap::new();
        data.insert("offset".to_string(), Value::Int(self.next_offset as i64));
        data.insert("value".to_string(), value.clone());
        event_bus.publish(
            self.event_source.clone(),
            "dequeue_available".to_string(),
            data,
        );

        value
    }
}

impl Substrate for Queue {
    fn name(&self) -> &str {
        &self.instance_name
    }

    fn event_source(&self) -> &str {
        &self.event_source
    }

    fn execute_op(
        &mut self,
        op: &str,
        args: Vec<Value>,
        event_bus: &mut EventBus,
    ) -> Result<Value, String> {
        match op {
            "enqueue" => {
                let value = args.into_iter().next().unwrap_or(Value::None);
                Ok(self.enqueue(value, event_bus))
            }
            "dequeue" => Ok(self.dequeue(event_bus)),
            "peek" => Ok(self.peek(event_bus)),
            _ => Err(format!(
                "substrate '{}' has no operation '{op}'",
                self.instance_name
            )),
        }
    }

    fn guarantees(&self) -> &[String] {
        &self.guarantees
    }

    fn op_kind(&self, op: &str) -> InteractionKind {
        match op {
            "enqueue" => InteractionKind::Command,
            "dequeue" | "peek" => InteractionKind::Query,
            _ => InteractionKind::Pure,
        }
    }

    fn set_next_offset(&mut self, offset: u64) {
        self.next_offset = offset;
    }

    fn compensate_op(
        &mut self,
        op: &str,
        _args: Vec<Value>,
        event_bus: &mut EventBus,
    ) -> Result<(), String> {
        match op {
            "enqueue" => {
                // Compensate by dequeuing the last enqueued item
                let _ = self.queue.pop_back();
                let mut data = HashMap::new();
                data.insert("op".to_string(), Value::String("enqueue".to_string()));
                event_bus.publish(
                    self.event_source.clone(),
                    "enqueue_reverted".to_string(),
                    data,
                );
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

/// Clock substrate. Provides wall-clock time and a monotonic counter.
pub struct Clock {
    instance_name: String,
    event_source: String,
    guarantees: Vec<String>,
    counter: u64,
    next_offset: u64,
    pending_timers: Vec<(String, u64)>, // (tag, fire_at_epoch_ms)
    /// When true, skip the blocking std::thread::sleep (evaluator handles async sleep).
    pub skip_next_sleep: bool,
}

impl Clock {
    pub fn new(instance_name: &str, event_source: &str) -> Self {
        Self {
            instance_name: instance_name.to_string(),
            event_source: event_source.to_string(),
            guarantees: vec!["monotonic_time".to_string()],
            counter: 0,
            next_offset: 1,
            pending_timers: Vec::new(),
            skip_next_sleep: false,
        }
    }

    fn now(&mut self, event_bus: &mut EventBus) -> Value {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let offset = self.next_offset;
        self.next_offset += 1;
        let mut data = HashMap::new();
        data.insert("offset".to_string(), Value::Int(offset as i64));
        data.insert("timestamp".to_string(), Value::Int(timestamp));
        event_bus.publish(
            self.event_source.clone(),
            "time_read".to_string(),
            data,
        );

        Value::Int(timestamp)
    }

    fn monotonic(&mut self, event_bus: &mut EventBus) -> Value {
        let tick = self.counter;
        self.counter += 1;

        let offset = self.next_offset;
        self.next_offset += 1;
        let mut data = HashMap::new();
        data.insert("offset".to_string(), Value::Int(offset as i64));
        data.insert("tick".to_string(), Value::Int(tick as i64));
        event_bus.publish(
            self.event_source.clone(),
            "time_read".to_string(),
            data,
        );

        Value::Int(tick as i64)
    }
}

impl Substrate for Clock {
    fn name(&self) -> &str {
        &self.instance_name
    }

    fn event_source(&self) -> &str {
        &self.event_source
    }

    fn execute_op(
        &mut self,
        op: &str,
        args: Vec<Value>,
        event_bus: &mut EventBus,
    ) -> Result<Value, String> {
        match op {
            "now" => Ok(self.now(event_bus)),
            "monotonic" => Ok(self.monotonic(event_bus)),
            "sleep" => {
                let ms = match args.first() {
                    Some(Value::Int(n)) => *n,
                    _ => return Err("sleep requires an Int argument (milliseconds)".to_string()),
                };
                if ms > 0 && !self.skip_next_sleep {
                    std::thread::sleep(Duration::from_millis(ms as u64));
                }
                self.skip_next_sleep = false;

                let offset = self.next_offset;
                self.next_offset += 1;
                let mut data = HashMap::new();
                data.insert("offset".to_string(), Value::Int(offset as i64));
                data.insert("duration_ms".to_string(), Value::Int(ms));
                event_bus.publish(
                    self.event_source.clone(),
                    "timer_fired".to_string(),
                    data,
                );

                Ok(Value::Unit)
            }
            "elapsed" => {
                let start_ms = match args.first() {
                    Some(Value::Int(n)) => *n,
                    _ => return Err("elapsed requires an Int argument (start_ms)".to_string()),
                };
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64;

                let offset = self.next_offset;
                self.next_offset += 1;
                let mut data = HashMap::new();
                data.insert("offset".to_string(), Value::Int(offset as i64));
                data.insert("elapsed_ms".to_string(), Value::Int(now - start_ms));
                event_bus.publish(
                    self.event_source.clone(),
                    "time_read".to_string(),
                    data,
                );

                Ok(Value::Int(now - start_ms))
            }
            "set_timer" => {
                let ms = match args.first() {
                    Some(Value::Int(n)) => *n,
                    _ => return Err("set_timer requires an Int argument (ms)".to_string()),
                };
                let tag = match args.get(1) {
                    Some(Value::String(s)) => s.clone(),
                    _ => return Err("set_timer requires a String argument (tag)".to_string()),
                };
                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let fire_at = now_ms + ms as u64;
                self.pending_timers.push((tag, fire_at));
                Ok(Value::Unit)
            }
            "check_timers" => {
                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let mut expired = Vec::new();
                let mut remaining = Vec::new();
                for (tag, fire_at) in self.pending_timers.drain(..) {
                    if now_ms >= fire_at {
                        expired.push(tag);
                    } else {
                        remaining.push((tag, fire_at));
                    }
                }
                self.pending_timers = remaining;
                // Emit timer_fired event for each expired timer
                for tag in &expired {
                    let offset = self.next_offset;
                    self.next_offset += 1;
                    let mut data = HashMap::new();
                    data.insert("offset".to_string(), Value::Int(offset as i64));
                    data.insert("tag".to_string(), Value::String(tag.clone()));
                    event_bus.publish(
                        self.event_source.clone(),
                        "timer_fired".to_string(),
                        data,
                    );
                }
                Ok(Value::List(expired.into_iter().map(Value::String).collect()))
            }
            _ => Err(format!(
                "substrate '{}' has no operation '{op}'",
                self.instance_name
            )),
        }
    }

    fn guarantees(&self) -> &[String] {
        &self.guarantees
    }

    fn op_kind(&self, op: &str) -> InteractionKind {
        match op {
            "sleep" | "set_timer" => InteractionKind::Command,
            _ => InteractionKind::Query,
        }
    }

    fn set_next_offset(&mut self, offset: u64) {
        self.next_offset = offset;
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

/// Crypto substrate. Provides SHA-256 and HMAC-SHA-256 operations.
pub struct Crypto {
    instance_name: String,
    event_source: String,
    next_offset: u64,
}

impl Crypto {
    pub fn new(instance_name: &str, event_source: &str) -> Self {
        Self {
            instance_name: instance_name.to_string(),
            event_source: event_source.to_string(),
            next_offset: 1,
        }
    }

    fn emit_crypto_event(&mut self, op_name: &str, input_len: usize, event_bus: &mut EventBus) {
        let offset = self.next_offset;
        self.next_offset += 1;
        let mut event_data = HashMap::new();
        event_data.insert("offset".to_string(), Value::Int(offset as i64));
        event_data.insert("op".to_string(), Value::String(op_name.to_string()));
        event_data.insert("input_len".to_string(), Value::Int(input_len as i64));
        event_bus.publish(
            self.event_source.clone(),
            "crypto_op".to_string(),
            event_data,
        );
    }

    fn sha256(&mut self, data: &str, event_bus: &mut EventBus) -> Value {
        use sha2::{Sha256, Digest};
        let mut hasher = Sha256::new();
        hasher.update(data.as_bytes());
        let result = hasher.finalize();
        let hex = result.iter().map(|b| format!("{b:02x}")).collect::<String>();

        let offset = self.next_offset;
        self.next_offset += 1;
        let mut event_data = HashMap::new();
        event_data.insert("offset".to_string(), Value::Int(offset as i64));
        event_data.insert("op".to_string(), Value::String("sha256".to_string()));
        event_data.insert("input_len".to_string(), Value::Int(data.len() as i64));
        event_bus.publish(
            self.event_source.clone(),
            "crypto_op".to_string(),
            event_data,
        );

        Value::String(hex)
    }

    fn hmac_sha256(&mut self, data: &str, key: &str, event_bus: &mut EventBus) -> Value {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let mut mac = HmacSha256::new_from_slice(key.as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(data.as_bytes());
        let result = mac.finalize().into_bytes();
        let hex = result.iter().map(|b| format!("{b:02x}")).collect::<String>();

        let offset = self.next_offset;
        self.next_offset += 1;
        let mut event_data = HashMap::new();
        event_data.insert("offset".to_string(), Value::Int(offset as i64));
        event_data.insert("op".to_string(), Value::String("hmac_sha256".to_string()));
        event_data.insert("input_len".to_string(), Value::Int(data.len() as i64));
        event_bus.publish(
            self.event_source.clone(),
            "crypto_op".to_string(),
            event_data,
        );

        Value::String(hex)
    }
}

impl Substrate for Crypto {
    fn name(&self) -> &str {
        &self.instance_name
    }

    fn event_source(&self) -> &str {
        &self.event_source
    }

    fn execute_op(
        &mut self,
        op: &str,
        args: Vec<Value>,
        event_bus: &mut EventBus,
    ) -> Result<Value, String> {
        match op {
            "sha256" => {
                let data = match args.first() {
                    Some(Value::String(s)) => s.as_str(),
                    _ => return Err("sha256 requires a String argument".to_string()),
                };
                Ok(self.sha256(data, event_bus))
            }
            "hmac_sha256" => {
                let data = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    _ => return Err("hmac_sha256 requires a String data argument".to_string()),
                };
                let key = match args.get(1) {
                    Some(Value::String(s)) => s.clone(),
                    _ => return Err("hmac_sha256 requires a String key as second argument".to_string()),
                };
                Ok(self.hmac_sha256(&data, &key, event_bus))
            }
            "sha256_bytes" => {
                let data = match args.first() {
                    Some(Value::Bytes(b)) => b.clone(),
                    _ => return Err("sha256_bytes requires a Bytes argument".to_string()),
                };
                use sha2::{Sha256, Digest};
                let mut hasher = Sha256::new();
                hasher.update(&data);
                let result = hasher.finalize();
                self.emit_crypto_event("sha256_bytes", data.len(), event_bus);
                Ok(Value::Bytes(result.to_vec()))
            }
            "hmac_sha256_bytes" => {
                let data = match args.first() {
                    Some(Value::Bytes(b)) => b.clone(),
                    _ => return Err("hmac_sha256_bytes requires Bytes data".to_string()),
                };
                let key = match args.get(1) {
                    Some(Value::Bytes(b)) => b.clone(),
                    _ => return Err("hmac_sha256_bytes requires Bytes key".to_string()),
                };
                use hmac::{Hmac, Mac};
                use sha2::Sha256;
                type HmacSha256 = Hmac<Sha256>;
                let mut mac = HmacSha256::new_from_slice(&key)
                    .expect("HMAC can take key of any size");
                mac.update(&data);
                let result = mac.finalize().into_bytes();
                self.emit_crypto_event("hmac_sha256_bytes", data.len(), event_bus);
                Ok(Value::Bytes(result.to_vec()))
            }
            "aes_gcm_encrypt" => {
                use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
                use aes_gcm::aead::Payload;
                let key = get_bytes_arg(&args, 0, "aes_gcm_encrypt", "key")?;
                let nonce_bytes = get_bytes_arg(&args, 1, "aes_gcm_encrypt", "nonce")?;
                let plaintext = get_bytes_arg(&args, 2, "aes_gcm_encrypt", "plaintext")?;
                let aad = get_bytes_arg(&args, 3, "aes_gcm_encrypt", "aad")?;
                if key.len() != 32 { return Err("aes_gcm_encrypt: key must be 32 bytes".to_string()); }
                if nonce_bytes.len() != 12 { return Err("aes_gcm_encrypt: nonce must be 12 bytes".to_string()); }
                let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("AES key error: {e}"))?;
                let nonce = aes_gcm::Nonce::from_slice(&nonce_bytes);
                let payload = Payload { msg: &plaintext, aad: &aad };
                let ciphertext = cipher.encrypt(nonce, payload)
                    .map_err(|e| format!("aes_gcm_encrypt failed: {e}"))?;
                self.emit_crypto_event("aes_gcm_encrypt", plaintext.len(), event_bus);
                Ok(Value::Bytes(ciphertext))
            }
            "aes_gcm_decrypt" => {
                use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
                use aes_gcm::aead::Payload;
                let key = get_bytes_arg(&args, 0, "aes_gcm_decrypt", "key")?;
                let nonce_bytes = get_bytes_arg(&args, 1, "aes_gcm_decrypt", "nonce")?;
                let ciphertext = get_bytes_arg(&args, 2, "aes_gcm_decrypt", "ciphertext")?;
                let aad = get_bytes_arg(&args, 3, "aes_gcm_decrypt", "aad")?;
                if key.len() != 32 { return Err("aes_gcm_decrypt: key must be 32 bytes".to_string()); }
                if nonce_bytes.len() != 12 { return Err("aes_gcm_decrypt: nonce must be 12 bytes".to_string()); }
                let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("AES key error: {e}"))?;
                let nonce = aes_gcm::Nonce::from_slice(&nonce_bytes);
                let payload = Payload { msg: &ciphertext, aad: &aad };
                let plaintext = cipher.decrypt(nonce, payload)
                    .map_err(|e| format!("aes_gcm_decrypt failed: {e}"))?;
                self.emit_crypto_event("aes_gcm_decrypt", ciphertext.len(), event_bus);
                Ok(Value::Bytes(plaintext))
            }
            "chacha20_encrypt" => {
                use chacha20poly1305::{ChaCha20Poly1305, KeyInit, aead::Aead};
                use chacha20poly1305::aead::Payload;
                let key = get_bytes_arg(&args, 0, "chacha20_encrypt", "key")?;
                let nonce_bytes = get_bytes_arg(&args, 1, "chacha20_encrypt", "nonce")?;
                let plaintext = get_bytes_arg(&args, 2, "chacha20_encrypt", "plaintext")?;
                let aad = get_bytes_arg(&args, 3, "chacha20_encrypt", "aad")?;
                if key.len() != 32 { return Err("chacha20_encrypt: key must be 32 bytes".to_string()); }
                if nonce_bytes.len() != 12 { return Err("chacha20_encrypt: nonce must be 12 bytes".to_string()); }
                let cipher = ChaCha20Poly1305::new_from_slice(&key).map_err(|e| format!("ChaCha20 key error: {e}"))?;
                let nonce = chacha20poly1305::Nonce::from_slice(&nonce_bytes);
                let payload = Payload { msg: &plaintext, aad: &aad };
                let ciphertext = cipher.encrypt(nonce, payload)
                    .map_err(|e| format!("chacha20_encrypt failed: {e}"))?;
                self.emit_crypto_event("chacha20_encrypt", plaintext.len(), event_bus);
                Ok(Value::Bytes(ciphertext))
            }
            "chacha20_decrypt" => {
                use chacha20poly1305::{ChaCha20Poly1305, KeyInit, aead::Aead};
                use chacha20poly1305::aead::Payload;
                let key = get_bytes_arg(&args, 0, "chacha20_decrypt", "key")?;
                let nonce_bytes = get_bytes_arg(&args, 1, "chacha20_decrypt", "nonce")?;
                let ciphertext = get_bytes_arg(&args, 2, "chacha20_decrypt", "ciphertext")?;
                let aad = get_bytes_arg(&args, 3, "chacha20_decrypt", "aad")?;
                if key.len() != 32 { return Err("chacha20_decrypt: key must be 32 bytes".to_string()); }
                if nonce_bytes.len() != 12 { return Err("chacha20_decrypt: nonce must be 12 bytes".to_string()); }
                let cipher = ChaCha20Poly1305::new_from_slice(&key).map_err(|e| format!("ChaCha20 key error: {e}"))?;
                let nonce = chacha20poly1305::Nonce::from_slice(&nonce_bytes);
                let payload = Payload { msg: &ciphertext, aad: &aad };
                let plaintext = cipher.decrypt(nonce, payload)
                    .map_err(|e| format!("chacha20_decrypt failed: {e}"))?;
                self.emit_crypto_event("chacha20_decrypt", ciphertext.len(), event_bus);
                Ok(Value::Bytes(plaintext))
            }
            "hkdf_extract" => {
                use hkdf::Hkdf;
                use sha2::Sha256;
                let salt = get_bytes_arg(&args, 0, "hkdf_extract", "salt")?;
                let ikm = get_bytes_arg(&args, 1, "hkdf_extract", "ikm")?;
                let (prk, _) = Hkdf::<Sha256>::extract(Some(&salt), &ikm);
                self.emit_crypto_event("hkdf_extract", ikm.len(), event_bus);
                Ok(Value::Bytes(prk.to_vec()))
            }
            "hkdf_expand" => {
                use hkdf::Hkdf;
                use sha2::Sha256;
                let prk = get_bytes_arg(&args, 0, "hkdf_expand", "prk")?;
                let info = get_bytes_arg(&args, 1, "hkdf_expand", "info")?;
                let length = match args.get(2) {
                    Some(Value::Int(n)) => *n as usize,
                    _ => return Err("hkdf_expand: length must be an Int".to_string()),
                };
                // Reconstruct Hkdf from PRK (no extraction needed)
                let hkdf = Hkdf::<Sha256>::from_prk(&prk)
                    .map_err(|e| format!("hkdf_expand: invalid PRK: {e}"))?;
                let mut okm = vec![0u8; length];
                hkdf.expand(&info, &mut okm)
                    .map_err(|e| format!("hkdf_expand failed: {e}"))?;
                self.emit_crypto_event("hkdf_expand", prk.len(), event_bus);
                Ok(Value::Bytes(okm))
            }
            "x25519_keypair" => {
                use x25519_dalek::{StaticSecret, PublicKey};
                let secret = StaticSecret::random_from_rng(rand_core_06::OsRng);
                let public = PublicKey::from(&secret);
                let mut fields = HashMap::new();
                fields.insert("public".to_string(), Value::Bytes(public.as_bytes().to_vec()));
                fields.insert("secret".to_string(), Value::Bytes(secret.to_bytes().to_vec()));
                self.emit_crypto_event("x25519_keypair", 0, event_bus);
                Ok(Value::Struct { name: "KeyPair".to_string(), fields })
            }
            "x25519_dh" => {
                use x25519_dalek::{StaticSecret, PublicKey};
                let secret_bytes = get_bytes_arg(&args, 0, "x25519_dh", "secret")?;
                let public_bytes = get_bytes_arg(&args, 1, "x25519_dh", "their_public")?;
                if secret_bytes.len() != 32 { return Err("x25519_dh: secret must be 32 bytes".to_string()); }
                if public_bytes.len() != 32 { return Err("x25519_dh: their_public must be 32 bytes".to_string()); }
                let secret: [u8; 32] = secret_bytes.try_into().unwrap();
                let public: [u8; 32] = public_bytes.try_into().unwrap();
                let secret = StaticSecret::from(secret);
                let public = PublicKey::from(public);
                let shared = secret.diffie_hellman(&public);
                self.emit_crypto_event("x25519_dh", 32, event_bus);
                Ok(Value::Bytes(shared.as_bytes().to_vec()))
            }
            "ed25519_keypair" => {
                use ed25519_dalek::SigningKey;
                let signing = SigningKey::generate(&mut rand_core_06::OsRng);
                let verifying = signing.verifying_key();
                let mut fields = HashMap::new();
                fields.insert("public".to_string(), Value::Bytes(verifying.to_bytes().to_vec()));
                fields.insert("secret".to_string(), Value::Bytes(signing.to_bytes().to_vec()));
                self.emit_crypto_event("ed25519_keypair", 0, event_bus);
                Ok(Value::Struct { name: "KeyPair".to_string(), fields })
            }
            "ed25519_sign" => {
                use ed25519_dalek::{SigningKey, Signer};
                let secret_bytes = get_bytes_arg(&args, 0, "ed25519_sign", "secret")?;
                let message = get_bytes_arg(&args, 1, "ed25519_sign", "message")?;
                if secret_bytes.len() != 32 { return Err("ed25519_sign: secret must be 32 bytes".to_string()); }
                let secret: [u8; 32] = secret_bytes.try_into().unwrap();
                let signing = SigningKey::from_bytes(&secret);
                let signature = signing.sign(&message);
                self.emit_crypto_event("ed25519_sign", message.len(), event_bus);
                Ok(Value::Bytes(signature.to_bytes().to_vec()))
            }
            "ed25519_verify" => {
                use ed25519_dalek::{VerifyingKey, Verifier, Signature};
                let public_bytes = get_bytes_arg(&args, 0, "ed25519_verify", "public")?;
                let message = get_bytes_arg(&args, 1, "ed25519_verify", "message")?;
                let sig_bytes = get_bytes_arg(&args, 2, "ed25519_verify", "signature")?;
                if public_bytes.len() != 32 { return Err("ed25519_verify: public must be 32 bytes".to_string()); }
                if sig_bytes.len() != 64 { return Err("ed25519_verify: signature must be 64 bytes".to_string()); }
                let public: [u8; 32] = public_bytes.try_into().unwrap();
                let verifying = VerifyingKey::from_bytes(&public)
                    .map_err(|e| format!("ed25519_verify: invalid public key: {e}"))?;
                let sig: [u8; 64] = sig_bytes.try_into().unwrap();
                let signature = Signature::from_bytes(&sig);
                let valid = verifying.verify(&message, &signature).is_ok();
                self.emit_crypto_event("ed25519_verify", message.len(), event_bus);
                Ok(Value::Bool(valid))
            }
            "random_bytes" => {
                use rand::RngCore;
                let length = match args.first() {
                    Some(Value::Int(n)) => *n as usize,
                    _ => return Err("random_bytes requires an Int length argument".to_string()),
                };
                let mut buf = vec![0u8; length];
                rand::rng().fill_bytes(&mut buf);
                self.emit_crypto_event("random_bytes", length, event_bus);
                Ok(Value::Bytes(buf))
            }
            _ => Err(format!(
                "substrate '{}' has no operation '{op}'",
                self.instance_name
            )),
        }
    }

    fn guarantees(&self) -> &[String] {
        &[]
    }

    fn op_kind(&self, op: &str) -> InteractionKind {
        match op {
            "x25519_keypair" | "ed25519_keypair" | "random_bytes" => InteractionKind::Command,
            _ => InteractionKind::Query,
        }
    }

    fn set_next_offset(&mut self, offset: u64) {
        self.next_offset = offset;
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

/// Registry for looking up substrates by name.
pub struct SubstrateRegistry {
    substrates: HashMap<String, Box<dyn Substrate>>,
}

impl SubstrateRegistry {
    pub fn new() -> Self {
        Self {
            substrates: HashMap::new(),
        }
    }

    pub fn register(&mut self, name: String, substrate: Box<dyn Substrate>) {
        self.substrates.insert(name, substrate);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Substrate> {
        self.substrates.get(name).map(|s| s.as_ref())
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Box<dyn Substrate>> {
        self.substrates.get_mut(name)
    }

    /// Iterate over all substrates mutably.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&String, &mut Box<dyn Substrate>)> {
        self.substrates.iter_mut()
    }

    /// Remove a substrate by name, returning it. Used for the remove-dispatch-reinsert
    /// pattern when a BalanceSubstrate needs to call sibling substrates.
    pub fn remove(&mut self, name: &str) -> Option<Box<dyn Substrate>> {
        self.substrates.remove(name)
    }

    /// Insert a substrate back by name. Used after remove-dispatch-reinsert.
    pub fn insert(&mut self, name: String, substrate: Box<dyn Substrate>) {
        self.substrates.insert(name, substrate);
    }

    /// Set the next offset for a named substrate (used for recovery).
    pub fn set_offset(&mut self, name: &str, offset: u64) {
        if let Some(substrate) = self.substrates.get_mut(name) {
            substrate.set_next_offset(offset);
        }
    }

    pub fn execute_op(
        &mut self,
        substrate_name: &str,
        op: &str,
        args: Vec<Value>,
        event_bus: &mut EventBus,
    ) -> Result<Value, String> {
        match self.substrates.get_mut(substrate_name) {
            Some(substrate) => substrate.execute_op(op, args, event_bus),
            None => Err(format!("no substrate named '{substrate_name}'")),
        }
    }

    /// Execute an op on a BalanceSubstrate that has deps, using the
    /// remove-dispatch-reinsert pattern to avoid &mut self + &mut registry conflict.
    /// Returns None if the substrate is not a BalanceSubstrate with deps (caller
    /// should fall back to normal execute_op).
    pub fn execute_op_composed(
        &mut self,
        substrate_name: &str,
        op: &str,
        args: Vec<Value>,
        event_bus: &mut EventBus,
    ) -> Option<Result<Value, String>> {
        // Remove the substrate temporarily
        let mut substrate_box = match self.substrates.remove(substrate_name) {
            Some(s) => s,
            None => return Some(Err(format!("no substrate named '{substrate_name}'"))),
        };

        // Check if it's a BalanceSubstrate with deps via as_any_mut() downcasting
        let has_deps = substrate_box.as_any_mut()
            .downcast_ref::<BalanceSubstrate>()
            .map(|bs| bs.has_deps())
            .unwrap_or(false);

        if !has_deps {
            // Not a composed substrate — put it back and return None
            self.substrates.insert(substrate_name.to_string(), substrate_box);
            return None;
        }

        // Downcast to BalanceSubstrate and execute with dep dispatch.
        // The dep_dispatch closure gets exclusive access to self.substrates and event_bus.
        // The BalanceSubstrate collects its own pending events and returns them.
        let bal_sub = substrate_box.as_any_mut()
            .downcast_mut::<BalanceSubstrate>().unwrap();

        let result = bal_sub.execute_op_with_deps(op, args, &mut |dep_instance, dep_op, dep_args| {
            match self.substrates.get_mut(dep_instance) {
                Some(dep_substrate) => dep_substrate.execute_op(dep_op, dep_args, event_bus),
                None => Err(format!("dep substrate '{}' not found", dep_instance)),
            }
        });

        // Re-insert the substrate
        self.substrates.insert(substrate_name.to_string(), substrate_box);

        // Publish pending events from the composed substrate
        match result {
            Ok((val, event_source, pending_events)) => {
                for (event_type, data) in pending_events {
                    event_bus.publish(event_source.clone(), event_type, data);
                }
                Some(Ok(val))
            }
            Err(e) => Some(Err(e)),
        }
    }

    /// Compensate a previously executed operation on a named substrate.
    pub fn compensate_op(
        &mut self,
        name: &str,
        op: &str,
        args: Vec<Value>,
        event_bus: &mut EventBus,
    ) -> Result<(), String> {
        match self.substrates.get_mut(name) {
            Some(substrate) => substrate.compensate_op(op, args, event_bus),
            None => Err(format!("no substrate named '{name}'")),
        }
    }

    /// Peek at a stored value without emitting events (for atomic snapshotting).
    pub fn peek_value(&self, substrate_name: &str, key: &str) -> Option<Value> {
        self.substrates.get(substrate_name)
            .and_then(|s| s.peek_value(key))
    }
}

fn get_bytes_arg(args: &[Value], idx: usize, op: &str, name: &str) -> Result<Vec<u8>, String> {
    match args.get(idx) {
        Some(Value::Bytes(b)) => Ok(b.clone()),
        _ => Err(format!("{op}: argument '{name}' must be Bytes")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::coordinator::FileStorage;

    #[test]
    fn test_replicated_log_append_read() {
        let mut log = ReplicatedLog::new("log/main", "log");
        let mut bus = EventBus::new();

        let ack = log
            .execute_op("append", vec![Value::String("hello".into())], &mut bus)
            .unwrap();
        assert!(ack.is_ack(), "expected Ack, got {ack}");
        assert!(ack.ack_key().unwrap().starts_with("offset_"));

        let val = log
            .execute_op("read", vec![Value::Int(0)], &mut bus)
            .unwrap();
        assert_eq!(val, Value::String("hello".to_string()));
    }

    #[test]
    fn test_replicated_log_emits_events() {
        let mut log = ReplicatedLog::new("log/main", "log");
        let mut bus = EventBus::new();

        log.execute_op("append", vec![Value::String("data".into())], &mut bus)
            .unwrap();

        let events = bus.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "append_accepted");
        assert_eq!(events[0].source, "log");
        assert_eq!(events[1].event_type, "quorum_committed");
        assert_eq!(events[1].source, "log");
    }

    #[test]
    fn test_replicated_log_offset_monotonic() {
        let mut log = ReplicatedLog::new("log/main", "log");
        let mut bus = EventBus::new();

        let ack1 = log
            .execute_op("append", vec![Value::Int(1)], &mut bus)
            .unwrap();
        let ack2 = log
            .execute_op("append", vec![Value::Int(2)], &mut bus)
            .unwrap();

        let key1 = ack1.ack_key().expect("expected Ack").to_string();
        let key2 = ack2.ack_key().expect("expected Ack").to_string();
        assert_ne!(key1, key2);
    }

    #[test]
    fn test_substrate_registry() {
        let mut registry = SubstrateRegistry::new();
        let mut bus = EventBus::new();

        registry.register(
            "log/main".to_string(),
            Box::new(ReplicatedLog::new("log/main", "log")),
        );

        let ack = registry
            .execute_op("log/main", "append", vec![Value::String("x".into())], &mut bus)
            .unwrap();
        assert!(ack.is_ack());

        let err = registry.execute_op("nonexistent", "append", vec![], &mut bus);
        assert!(err.is_err());
    }

    #[test]
    fn test_replicated_log_guarantees() {
        let log = ReplicatedLog::new("log/main", "log");
        let guarantees = log.guarantees();
        assert!(guarantees.contains(&"commit_requires_accept".to_string()));
        assert!(guarantees.contains(&"committed".to_string()));
    }

    #[test]
    fn test_kv_store_put_get() {
        let mut kv = KeyValueStore::new("kv/main", "kv");
        let mut bus = EventBus::new();

        let ack = kv
            .execute_op(
                "put",
                vec![Value::String("key1".into()), Value::String("val1".into())],
                &mut bus,
            )
            .unwrap();
        assert!(ack.is_ack());

        let val = kv
            .execute_op("get", vec![Value::String("key1".into())], &mut bus)
            .unwrap();
        assert_eq!(val, Value::String("val1".to_string()));
    }

    #[test]
    fn test_kv_store_delete() {
        let mut kv = KeyValueStore::new("kv/main", "kv");
        let mut bus = EventBus::new();

        kv.execute_op(
            "put",
            vec![Value::String("key1".into()), Value::String("val1".into())],
            &mut bus,
        )
        .unwrap();

        let ack = kv
            .execute_op("delete", vec![Value::String("key1".into())], &mut bus)
            .unwrap();
        assert!(ack.is_ack());

        let val = kv
            .execute_op("get", vec![Value::String("key1".into())], &mut bus)
            .unwrap();
        assert_eq!(val, Value::None);
    }

    #[test]
    fn test_kv_store_emits_events() {
        let mut kv = KeyValueStore::new("kv/main", "kv");
        let mut bus = EventBus::new();

        kv.execute_op(
            "put",
            vec![Value::String("k".into()), Value::String("v".into())],
            &mut bus,
        )
        .unwrap();

        let events = bus.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "put_accepted");
        assert_eq!(events[1].event_type, "put_committed");
    }

    #[test]
    fn test_queue_enqueue_dequeue() {
        let mut q = Queue::new("q/main", "queue");
        let mut bus = EventBus::new();

        let ack = q
            .execute_op("enqueue", vec![Value::String("item1".into())], &mut bus)
            .unwrap();
        assert!(ack.is_ack());

        q.execute_op("enqueue", vec![Value::String("item2".into())], &mut bus)
            .unwrap();

        let val1 = q.execute_op("dequeue", vec![], &mut bus).unwrap();
        assert_eq!(val1, Value::String("item1".to_string()));

        let val2 = q.execute_op("dequeue", vec![], &mut bus).unwrap();
        assert_eq!(val2, Value::String("item2".to_string()));

        let val3 = q.execute_op("dequeue", vec![], &mut bus).unwrap();
        assert_eq!(val3, Value::None);
    }

    #[test]
    fn test_queue_peek() {
        let mut q = Queue::new("q/main", "queue");
        let mut bus = EventBus::new();

        q.execute_op("enqueue", vec![Value::Int(42)], &mut bus)
            .unwrap();

        let val = q.execute_op("peek", vec![], &mut bus).unwrap();
        assert_eq!(val, Value::Int(42));

        // Peek doesn't remove
        let val2 = q.execute_op("peek", vec![], &mut bus).unwrap();
        assert_eq!(val2, Value::Int(42));
    }

    #[test]
    fn test_queue_emits_events() {
        let mut q = Queue::new("q/main", "queue");
        let mut bus = EventBus::new();

        q.execute_op("enqueue", vec![Value::String("x".into())], &mut bus)
            .unwrap();

        let events = bus.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "enqueue_accepted");
        assert_eq!(events[1].event_type, "enqueue_committed");
    }

    #[test]
    fn test_kv_store_op_kind() {
        let kv = KeyValueStore::new("kv/main", "kv");
        assert_eq!(kv.op_kind("put"), InteractionKind::Command);
        assert_eq!(kv.op_kind("get"), InteractionKind::Query);
        assert_eq!(kv.op_kind("delete"), InteractionKind::Command);
    }

    #[test]
    fn test_queue_op_kind() {
        let q = Queue::new("q/main", "queue");
        assert_eq!(q.op_kind("enqueue"), InteractionKind::Command);
        assert_eq!(q.op_kind("dequeue"), InteractionKind::Query);
        assert_eq!(q.op_kind("peek"), InteractionKind::Query);
    }

    #[test]
    fn test_clock_now() {
        let mut clock = Clock::new("clock/main", "clock");
        let mut bus = EventBus::new();

        let val = clock.execute_op("now", vec![], &mut bus).unwrap();
        match val {
            Value::Int(ts) => assert!(ts > 0, "timestamp should be positive"),
            other => panic!("expected Int, got {other}"),
        }

        let events = bus.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "time_read");
        assert_eq!(events[0].source, "clock");
    }

    #[test]
    fn test_clock_monotonic() {
        let mut clock = Clock::new("clock/main", "clock");
        let mut bus = EventBus::new();

        let v1 = clock.execute_op("monotonic", vec![], &mut bus).unwrap();
        let v2 = clock.execute_op("monotonic", vec![], &mut bus).unwrap();
        let v3 = clock.execute_op("monotonic", vec![], &mut bus).unwrap();

        assert_eq!(v1, Value::Int(0));
        assert_eq!(v2, Value::Int(1));
        assert_eq!(v3, Value::Int(2));
    }

    #[test]
    fn test_clock_monotonic_guarantee() {
        let clock = Clock::new("clock/main", "clock");
        assert!(clock.guarantees().contains(&"monotonic_time".to_string()));
    }

    #[test]
    fn test_clock_op_kind() {
        let clock = Clock::new("clock/main", "clock");
        assert_eq!(clock.op_kind("now"), InteractionKind::Query);
        assert_eq!(clock.op_kind("monotonic"), InteractionKind::Query);
    }

    #[test]
    fn test_clock_invalid_op() {
        let mut clock = Clock::new("clock/main", "clock");
        let mut bus = EventBus::new();
        let err = clock.execute_op("invalid", vec![], &mut bus);
        assert!(err.is_err());
    }

    #[test]
    fn test_crypto_sha256() {
        let mut crypto = Crypto::new("crypto/main", "crypto");
        let mut bus = EventBus::new();

        let val = crypto
            .execute_op("sha256", vec![Value::String("hello".into())], &mut bus)
            .unwrap();
        // SHA-256 of "hello" is well-known
        assert_eq!(
            val,
            Value::String("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_string())
        );

        // Determinism: same input → same output
        let val2 = crypto
            .execute_op("sha256", vec![Value::String("hello".into())], &mut bus)
            .unwrap();
        assert_eq!(val, val2);
    }

    #[test]
    fn test_crypto_hmac_sha256() {
        let mut crypto = Crypto::new("crypto/main", "crypto");
        let mut bus = EventBus::new();

        let val = crypto
            .execute_op(
                "hmac_sha256",
                vec![
                    Value::String("hello".into()),
                    Value::String("secret".into()),
                ],
                &mut bus,
            )
            .unwrap();
        match &val {
            Value::String(hex) => assert_eq!(hex.len(), 64, "HMAC-SHA256 hex should be 64 chars"),
            other => panic!("expected String, got {other}"),
        }

        // Determinism
        let val2 = crypto
            .execute_op(
                "hmac_sha256",
                vec![
                    Value::String("hello".into()),
                    Value::String("secret".into()),
                ],
                &mut bus,
            )
            .unwrap();
        assert_eq!(val, val2);
    }

    #[test]
    fn test_crypto_emits_events() {
        let mut crypto = Crypto::new("crypto/main", "crypto");
        let mut bus = EventBus::new();

        crypto
            .execute_op("sha256", vec![Value::String("data".into())], &mut bus)
            .unwrap();

        let events = bus.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "crypto_op");
        assert_eq!(events[0].source, "crypto");
    }

    #[test]
    fn test_crypto_no_guarantees() {
        let crypto = Crypto::new("crypto/main", "crypto");
        assert!(crypto.guarantees().is_empty());
    }

    #[test]
    fn test_crypto_op_kind() {
        let crypto = Crypto::new("crypto/main", "crypto");
        assert_eq!(crypto.op_kind("sha256"), InteractionKind::Query);
        assert_eq!(crypto.op_kind("hmac_sha256"), InteractionKind::Query);
    }

    #[test]
    fn test_crypto_invalid_op() {
        let mut crypto = Crypto::new("crypto/main", "crypto");
        let mut bus = EventBus::new();
        let err = crypto.execute_op("invalid", vec![], &mut bus);
        assert!(err.is_err());
    }

    #[test]
    fn test_crypto_sha256_missing_arg() {
        let mut crypto = Crypto::new("crypto/main", "crypto");
        let mut bus = EventBus::new();
        let err = crypto.execute_op("sha256", vec![], &mut bus);
        assert!(err.is_err());
    }

    #[test]
    fn test_crypto_hmac_missing_args() {
        let mut crypto = Crypto::new("crypto/main", "crypto");
        let mut bus = EventBus::new();
        let err = crypto.execute_op("hmac_sha256", vec![Value::String("data".into())], &mut bus);
        assert!(err.is_err());
    }

    #[test]
    fn test_set_next_offset_replicated_log() {
        let mut log = ReplicatedLog::new("log/main", "log");
        let mut bus = EventBus::new();

        // Default starts at 1
        log.execute_op("append", vec![Value::String("a".into())], &mut bus).unwrap();
        let events = bus.events();
        // First append_accepted should have offset 1
        assert_eq!(events[0].data.get("offset"), Some(&Value::Int(1)));

        // Set offset to 10
        log.set_next_offset(10);
        let mut bus2 = EventBus::new();
        log.execute_op("append", vec![Value::String("b".into())], &mut bus2).unwrap();
        let events2 = bus2.events();
        assert_eq!(events2[0].data.get("offset"), Some(&Value::Int(10)));
    }

    #[test]
    fn test_set_next_offset_kv() {
        let mut kv = KeyValueStore::new("kv/main", "kv");
        kv.set_next_offset(5);
        let mut bus = EventBus::new();
        kv.execute_op("put", vec![Value::String("k".into()), Value::Int(1)], &mut bus).unwrap();
        let events = bus.events();
        assert_eq!(events[0].data.get("offset"), Some(&Value::Int(5)));
    }

    #[test]
    fn test_set_next_offset_queue() {
        let mut q = Queue::new("q/main", "queue");
        q.set_next_offset(7);
        let mut bus = EventBus::new();
        q.execute_op("enqueue", vec![Value::Int(1)], &mut bus).unwrap();
        let events = bus.events();
        assert_eq!(events[0].data.get("offset"), Some(&Value::Int(7)));
    }

    #[test]
    fn test_set_next_offset_clock() {
        let mut clock = Clock::new("clock/main", "clock");
        clock.set_next_offset(3);
        let mut bus = EventBus::new();
        clock.execute_op("now", vec![], &mut bus).unwrap();
        let events = bus.events();
        assert_eq!(events[0].data.get("offset"), Some(&Value::Int(3)));
    }

    #[test]
    fn test_set_next_offset_crypto() {
        let mut crypto = Crypto::new("crypto/main", "crypto");
        crypto.set_next_offset(4);
        let mut bus = EventBus::new();
        crypto.execute_op("sha256", vec![Value::String("data".into())], &mut bus).unwrap();
        let events = bus.events();
        assert_eq!(events[0].data.get("offset"), Some(&Value::Int(4)));
    }

    #[test]
    fn test_substrate_registry_set_offset() {
        let mut registry = SubstrateRegistry::new();
        registry.register("kv/main".to_string(), Box::new(KeyValueStore::new("kv/main", "kv")));
        registry.set_offset("kv/main", 10);

        let mut bus = EventBus::new();
        registry.execute_op("kv/main", "put", vec![Value::String("k".into()), Value::Int(1)], &mut bus).unwrap();
        let events = bus.events();
        assert_eq!(events[0].data.get("offset"), Some(&Value::Int(10)));
    }

    #[test]
    fn test_replicated_log_with_local_coordinator() {
        use super::super::coordinator::LocalCoordinator;
        let coord = LocalCoordinator::new(Box::new(InMemoryStorage::new()));
        let mut log = ReplicatedLog::with_coordinator("log/main", "log", Box::new(coord));
        let mut bus = EventBus::new();

        let ack = log.execute_op("append", vec![Value::String("hello".into())], &mut bus).unwrap();
        assert!(ack.is_ack());

        let val = log.execute_op("read", vec![Value::Int(0)], &mut bus).unwrap();
        assert_eq!(val, Value::String("hello".to_string()));
    }

    #[test]
    fn test_kv_with_local_coordinator() {
        use super::super::coordinator::LocalCoordinator;
        let coord = LocalCoordinator::new(Box::new(InMemoryStorage::new()));
        let mut kv = KeyValueStore::with_coordinator("kv/main", "kv", Box::new(coord));
        let mut bus = EventBus::new();

        kv.execute_op("put", vec![Value::String("k".into()), Value::String("v".into())], &mut bus).unwrap();
        let val = kv.execute_op("get", vec![Value::String("k".into())], &mut bus).unwrap();
        assert_eq!(val, Value::String("v".to_string()));

        kv.execute_op("delete", vec![Value::String("k".into())], &mut bus).unwrap();
        let val2 = kv.execute_op("get", vec![Value::String("k".into())], &mut bus).unwrap();
        assert_eq!(val2, Value::None);
    }

    #[test]
    fn test_replicated_log_coordinator_proposal_called() {
        use super::super::coordinator::{ReplicatedCoordinator};
        // ReplicatedCoordinator with no replicas — quorum is just self, should always succeed
        let coord = ReplicatedCoordinator::new(Box::new(InMemoryStorage::new()), vec![]);
        let mut log = ReplicatedLog::with_coordinator("log/main", "log", Box::new(coord));
        let mut bus = EventBus::new();

        // append should succeed (coordinator auto-commits with 0 replicas)
        let ack = log.execute_op("append", vec![Value::Int(42)], &mut bus).unwrap();
        assert!(ack.is_ack());

        let val = log.execute_op("read", vec![Value::Int(0)], &mut bus).unwrap();
        assert_eq!(val, Value::Int(42));
    }

    #[test]
    fn test_factor_1_uses_no_coordinator() {
        // With replication_factor=1 (default), no coordinator should be set
        let log = ReplicatedLog::new("log/main", "log");
        let mut bus = EventBus::new();
        let mut log = log; // rebind as mutable
        let ack = log.execute_op("append", vec![Value::String("data".into())], &mut bus).unwrap();
        assert!(ack.is_ack());
        // No coordinator means no proposal overhead — just direct storage write
    }

    #[test]
    fn test_set_coordinator_on_existing_substrate() {
        use super::super::coordinator::LocalCoordinator;
        let mut log = ReplicatedLog::new("log/main", "log");
        // Initially no coordinator
        let mut bus = EventBus::new();
        log.execute_op("append", vec![Value::Int(1)], &mut bus).unwrap();

        // Set coordinator after construction
        let coord = LocalCoordinator::new(Box::new(InMemoryStorage::new()));
        log.set_coordinator(Box::new(coord));

        // Should still work with coordinator
        let ack = log.execute_op("append", vec![Value::Int(2)], &mut bus).unwrap();
        assert!(ack.is_ack());

        let val = log.execute_op("read", vec![Value::Int(1)], &mut bus).unwrap();
        assert_eq!(val, Value::Int(2));
    }

    #[test]
    fn test_kv_with_file_storage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kv.ndjson");

        let storage = Box::new(FileStorage::open(&path).unwrap());
        let mut kv = KeyValueStore::with_storage("kv/main", "kv", storage);
        let mut bus = EventBus::new();

        kv.execute_op(
            "put",
            vec![Value::String("key1".into()), Value::String("val1".into())],
            &mut bus,
        )
        .unwrap();

        let val = kv
            .execute_op("get", vec![Value::String("key1".into())], &mut bus)
            .unwrap();
        assert_eq!(val, Value::String("val1".to_string()));

        // Verify persistence: create new KV with same file
        drop(kv);
        let storage2 = Box::new(FileStorage::open(&path).unwrap());
        let mut kv2 = KeyValueStore::with_storage("kv/main", "kv", storage2);
        let mut bus2 = EventBus::new();
        let val2 = kv2
            .execute_op("get", vec![Value::String("key1".into())], &mut bus2)
            .unwrap();
        assert_eq!(val2, Value::String("val1".to_string()));
    }

    #[test]
    fn test_replicated_log_with_file_storage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.ndjson");

        let storage = Box::new(FileStorage::open(&path).unwrap());
        let mut log = ReplicatedLog::with_storage("log/main", "log", storage);
        let mut bus = EventBus::new();

        log.execute_op("append", vec![Value::String("hello".into())], &mut bus)
            .unwrap();
        log.execute_op("append", vec![Value::String("world".into())], &mut bus)
            .unwrap();

        let val = log
            .execute_op("read", vec![Value::Int(0)], &mut bus)
            .unwrap();
        assert_eq!(val, Value::String("hello".to_string()));

        let val = log
            .execute_op("read", vec![Value::Int(1)], &mut bus)
            .unwrap();
        assert_eq!(val, Value::String("world".to_string()));

        // Verify persistence
        drop(log);
        let storage2 = Box::new(FileStorage::open(&path).unwrap());
        let mut log2 = ReplicatedLog::with_storage("log/main", "log", storage2);
        let mut bus2 = EventBus::new();
        let val2 = log2
            .execute_op("read", vec![Value::Int(0)], &mut bus2)
            .unwrap();
        assert_eq!(val2, Value::String("hello".to_string()));
    }

    // ── Compensation tests ─────────────────────────────────────

    #[test]
    fn test_kv_compensate_put_restores_previous() {
        let mut kv = KeyValueStore::new("kv/main", "kv");
        let mut bus = EventBus::new();

        // Put initial value
        kv.execute_op("put", vec![Value::String("k1".into()), Value::String("old".into())], &mut bus).unwrap();

        // Overwrite
        kv.execute_op("put", vec![Value::String("k1".into()), Value::String("new".into())], &mut bus).unwrap();

        // Compensate: restore previous value
        kv.compensate_op("put", vec![Value::String("k1".into()), Value::String("old".into())], &mut bus).unwrap();

        let val = kv.execute_op("get", vec![Value::String("k1".into())], &mut bus).unwrap();
        assert_eq!(val, Value::String("old".to_string()));

        // put_reverted event should be emitted
        let events = bus.events();
        assert!(events.iter().any(|e| e.event_type == "put_reverted"));
    }

    #[test]
    fn test_kv_compensate_put_deletes_if_no_previous() {
        let mut kv = KeyValueStore::new("kv/main", "kv");
        let mut bus = EventBus::new();

        // Put new key
        kv.execute_op("put", vec![Value::String("k1".into()), Value::String("val".into())], &mut bus).unwrap();

        // Compensate with no previous value (None) → should delete
        kv.compensate_op("put", vec![Value::String("k1".into()), Value::None], &mut bus).unwrap();

        let val = kv.execute_op("get", vec![Value::String("k1".into())], &mut bus).unwrap();
        assert_eq!(val, Value::None);
    }

    #[test]
    fn test_kv_compensate_delete_restores_value() {
        let mut kv = KeyValueStore::new("kv/main", "kv");
        let mut bus = EventBus::new();

        // Put then delete
        kv.execute_op("put", vec![Value::String("k1".into()), Value::Int(42)], &mut bus).unwrap();
        kv.execute_op("delete", vec![Value::String("k1".into())], &mut bus).unwrap();

        // Compensate delete: restore value
        kv.compensate_op("delete", vec![Value::String("k1".into()), Value::Int(42)], &mut bus).unwrap();

        let val = kv.execute_op("get", vec![Value::String("k1".into())], &mut bus).unwrap();
        assert_eq!(val, Value::Int(42));

        let events = bus.events();
        assert!(events.iter().any(|e| e.event_type == "delete_reverted"));
    }

    #[test]
    fn test_log_compensate_append_emits_reverted() {
        let mut log = ReplicatedLog::new("log/main", "log");
        let mut bus = EventBus::new();

        log.execute_op("append", vec![Value::String("data".into())], &mut bus).unwrap();

        // Compensate append: emits append_reverted (log is append-only)
        log.compensate_op("append", vec![Value::String("offset_1".into())], &mut bus).unwrap();

        let events = bus.events();
        assert!(events.iter().any(|e| e.event_type == "append_reverted"));
    }

    #[test]
    fn test_queue_compensate_enqueue() {
        let mut q = Queue::new("q/main", "queue");
        let mut bus = EventBus::new();

        q.execute_op("enqueue", vec![Value::String("item".into())], &mut bus).unwrap();

        // Compensate: pop from back
        q.compensate_op("enqueue", vec![], &mut bus).unwrap();

        // Queue should be empty
        let val = q.execute_op("dequeue", vec![], &mut bus).unwrap();
        assert_eq!(val, Value::None);

        let events = bus.events();
        assert!(events.iter().any(|e| e.event_type == "enqueue_reverted"));
    }

    #[test]
    fn test_clock_compensate_is_noop() {
        let mut clock = Clock::new("clock/main", "clock");
        let mut bus = EventBus::new();
        // Compensate on read-only substrate is no-op
        clock.compensate_op("now", vec![], &mut bus).unwrap();
        // No reverted events
        assert!(bus.events().is_empty());
    }

    #[test]
    fn test_kv_peek_value() {
        let mut kv = KeyValueStore::new("kv/main", "kv");
        let mut bus = EventBus::new();

        assert_eq!(kv.peek_value("k1"), None);

        kv.execute_op("put", vec![Value::String("k1".into()), Value::Int(42)], &mut bus).unwrap();

        assert_eq!(kv.peek_value("k1"), Some(Value::Int(42)));
    }

    #[test]
    fn test_substrate_registry_compensate_op() {
        let mut registry = SubstrateRegistry::new();
        registry.register("kv/main".to_string(), Box::new(KeyValueStore::new("kv/main", "kv")));

        let mut bus = EventBus::new();
        // Put a value
        registry.execute_op("kv/main", "put", vec![Value::String("k1".into()), Value::Int(1)], &mut bus).unwrap();

        // Compensate via registry
        registry.compensate_op("kv/main", "put", vec![Value::String("k1".into()), Value::None], &mut bus).unwrap();

        // Value should be deleted
        let val = registry.execute_op("kv/main", "get", vec![Value::String("k1".into())], &mut bus).unwrap();
        assert_eq!(val, Value::None);

        let events = bus.events();
        assert!(events.iter().any(|e| e.event_type == "put_reverted"));
    }

    #[test]
    fn test_substrate_registry_peek_value() {
        let mut registry = SubstrateRegistry::new();
        registry.register("kv/main".to_string(), Box::new(KeyValueStore::new("kv/main", "kv")));

        let mut bus = EventBus::new();
        registry.execute_op("kv/main", "put", vec![Value::String("k1".into()), Value::Int(5)], &mut bus).unwrap();

        assert_eq!(registry.peek_value("kv/main", "k1"), Some(Value::Int(5)));
        assert_eq!(registry.peek_value("kv/main", "missing"), None);
        assert_eq!(registry.peek_value("nonexistent", "k1"), None);
    }
}
