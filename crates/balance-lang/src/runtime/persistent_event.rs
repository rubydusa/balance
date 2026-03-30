use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use super::event::Event;

/// Append-only persistent event log using NDJSON (one JSON event per line).
/// Events are appended and fsynced immediately for durability.
pub struct PersistentEventLog {
    path: PathBuf,
    node_id: String,
    file: File,
    /// Count of events written, used for recovery verification.
    event_count: u64,
}

impl PersistentEventLog {
    /// Open or create a persistent event log at the given path.
    pub fn open(path: &Path, node_id: &str) -> Result<Self, String> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create event log dir: {e}"))?;
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("open event log {}: {e}", path.display()))?;

        // Count existing lines to know how many events are already persisted
        let event_count = if path.exists() {
            let reader = BufReader::new(
                File::open(path)
                    .map_err(|e| format!("read event log for count: {e}"))?,
            );
            reader.lines().count() as u64
        } else {
            0
        };

        Ok(Self {
            path: path.to_path_buf(),
            node_id: node_id.to_string(),
            file,
            event_count,
        })
    }

    /// Append an event to the persistent log.
    pub fn append(&mut self, event: &Event) -> Result<(), String> {
        let json = serde_json::to_string(event)
            .map_err(|e| format!("serialize event: {e}"))?;
        writeln!(self.file, "{json}")
            .map_err(|e| format!("write event: {e}"))?;
        self.file.flush()
            .map_err(|e| format!("flush event log: {e}"))?;
        // fsync for durability
        self.file.sync_data()
            .map_err(|e| format!("fsync event log: {e}"))?;
        self.event_count += 1;
        Ok(())
    }

    /// Recover all events from the persistent log.
    pub fn recover(&self) -> Result<Vec<Event>, String> {
        let file = File::open(&self.path)
            .map_err(|e| format!("open event log for recovery: {e}"))?;
        let reader = BufReader::new(file);
        let mut events = Vec::new();

        for (line_num, line) in reader.lines().enumerate() {
            let line = line.map_err(|e| format!("read line {}: {e}", line_num + 1))?;
            if line.trim().is_empty() {
                continue;
            }
            let event: Event = serde_json::from_str(&line)
                .map_err(|e| format!("deserialize event at line {}: {e}", line_num + 1))?;
            events.push(event);
        }

        Ok(events)
    }

    /// Get the path to the log file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Get the node ID associated with this log.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Get the count of events written to this log.
    pub fn event_count(&self) -> u64 {
        self.event_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::value::Value;
    use crate::runtime::event::EventId;
    use std::collections::HashMap;

    fn make_event(id: u64, source: &str, event_type: &str) -> Event {
        Event {
            id: EventId(id),
            timestamp: id * 10,
            source: source.to_string(),
            event_type: event_type.to_string(),
            data: HashMap::new(),
        }
    }

    #[test]
    fn test_persistent_event_log_write_recover() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.ndjson");

        // Write events
        {
            let mut log = PersistentEventLog::open(&log_path, "node1").unwrap();
            assert_eq!(log.event_count(), 0);

            log.append(&make_event(1, "log/main", "append")).unwrap();
            log.append(&make_event(2, "log/main", "committed")).unwrap();
            log.append(&make_event(3, "kv/data", "put_ack")).unwrap();
            assert_eq!(log.event_count(), 3);
        }

        // Recover events
        {
            let log = PersistentEventLog::open(&log_path, "node1").unwrap();
            assert_eq!(log.event_count(), 3);

            let events = log.recover().unwrap();
            assert_eq!(events.len(), 3);
            assert_eq!(events[0].source, "log/main");
            assert_eq!(events[0].event_type, "append");
            assert_eq!(events[1].event_type, "committed");
            assert_eq!(events[2].source, "kv/data");
        }
    }

    #[test]
    fn test_persistent_event_log_append_across_opens() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.ndjson");

        // First session
        {
            let mut log = PersistentEventLog::open(&log_path, "node1").unwrap();
            log.append(&make_event(1, "log/main", "append")).unwrap();
        }

        // Second session
        {
            let mut log = PersistentEventLog::open(&log_path, "node1").unwrap();
            assert_eq!(log.event_count(), 1);
            log.append(&make_event(2, "log/main", "committed")).unwrap();
        }

        // Verify all events
        {
            let log = PersistentEventLog::open(&log_path, "node1").unwrap();
            let events = log.recover().unwrap();
            assert_eq!(events.len(), 2);
            assert_eq!(events[0].id, EventId(1));
            assert_eq!(events[1].id, EventId(2));
        }
    }

    #[test]
    fn test_persistent_event_log_with_data() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.ndjson");

        let mut data = HashMap::new();
        data.insert("key".to_string(), Value::String("hello".to_string()));
        data.insert("offset".to_string(), Value::Int(42));

        let event = Event {
            id: EventId(1),
            timestamp: 100,
            source: "kv/data".to_string(),
            event_type: "put_ack".to_string(),
            data,
        };

        {
            let mut log = PersistentEventLog::open(&log_path, "node1").unwrap();
            log.append(&event).unwrap();
        }

        {
            let log = PersistentEventLog::open(&log_path, "node1").unwrap();
            let events = log.recover().unwrap();
            assert_eq!(events.len(), 1);
            assert_eq!(
                events[0].data.get("key"),
                Some(&Value::String("hello".to_string()))
            );
            assert_eq!(events[0].data.get("offset"), Some(&Value::Int(42)));
        }
    }

    #[test]
    fn test_persistent_event_log_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.ndjson");

        let log = PersistentEventLog::open(&log_path, "node1").unwrap();
        let events = log.recover().unwrap();
        assert!(events.is_empty());
        assert_eq!(log.event_count(), 0);
    }

    #[test]
    fn test_persistent_event_log_node_id() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("events.ndjson");

        let log = PersistentEventLog::open(&log_path, "mynode").unwrap();
        assert_eq!(log.node_id(), "mynode");
    }
}
