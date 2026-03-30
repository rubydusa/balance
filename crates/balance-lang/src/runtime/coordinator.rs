use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::value::Value;

/// A storage backend for substrate state. Abstracts over in-memory and
/// persistent storage to allow substrates to survive process restarts.
pub trait SubstrateStorage {
    fn put(&mut self, key: &str, value: Value) -> Result<(), String>;
    fn get(&self, key: &str) -> Option<&Value>;
    fn delete(&mut self, key: &str) -> Result<(), String>;
    fn list_keys(&self) -> Vec<String>;
}

/// In-memory storage (current behavior wrapped in trait).
pub struct InMemoryStorage {
    data: HashMap<String, Value>,
}

impl InMemoryStorage {
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
        }
    }
}

impl Default for InMemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

impl SubstrateStorage for InMemoryStorage {
    fn put(&mut self, key: &str, value: Value) -> Result<(), String> {
        self.data.insert(key.to_string(), value);
        Ok(())
    }

    fn get(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }

    fn delete(&mut self, key: &str) -> Result<(), String> {
        self.data.remove(key);
        Ok(())
    }

    fn list_keys(&self) -> Vec<String> {
        self.data.keys().cloned().collect()
    }
}

/// NDJSON entry for FileStorage checkpoint.
#[derive(Debug, Serialize, Deserialize)]
struct StorageEntry {
    key: String,
    value: Value,
    #[serde(default)]
    deleted: bool,
}

/// File-backed storage using NDJSON checkpoint with in-memory cache.
/// All mutations are appended to the journal file immediately. On recovery,
/// the journal is replayed to reconstruct state.
pub struct FileStorage {
    data: HashMap<String, Value>,
    journal: File,
    path: PathBuf,
}

impl FileStorage {
    pub fn open(path: &Path) -> Result<Self, String> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create storage dir: {e}"))?;
        }

        // Recover existing entries
        let mut data = HashMap::new();
        if path.exists() {
            let file = File::open(path)
                .map_err(|e| format!("open storage file: {e}"))?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line.map_err(|e| format!("read storage line: {e}"))?;
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(entry) = serde_json::from_str::<StorageEntry>(&line) {
                    if entry.deleted {
                        data.remove(&entry.key);
                    } else {
                        data.insert(entry.key, entry.value);
                    }
                }
            }
        }

        let journal = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| format!("open storage journal: {e}"))?;

        Ok(Self {
            data,
            journal,
            path: path.to_path_buf(),
        })
    }

    fn append_entry(&mut self, entry: &StorageEntry) -> Result<(), String> {
        let json = serde_json::to_string(entry)
            .map_err(|e| format!("serialize storage entry: {e}"))?;
        writeln!(self.journal, "{json}")
            .map_err(|e| format!("write storage entry: {e}"))?;
        self.journal.flush()
            .map_err(|e| format!("flush storage journal: {e}"))?;
        Ok(())
    }

    /// Get the path to the storage file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl SubstrateStorage for FileStorage {
    fn put(&mut self, key: &str, value: Value) -> Result<(), String> {
        let entry = StorageEntry {
            key: key.to_string(),
            value: value.clone(),
            deleted: false,
        };
        self.append_entry(&entry)?;
        self.data.insert(key.to_string(), value);
        Ok(())
    }

    fn get(&self, key: &str) -> Option<&Value> {
        self.data.get(key)
    }

    fn delete(&mut self, key: &str) -> Result<(), String> {
        let entry = StorageEntry {
            key: key.to_string(),
            value: Value::None,
            deleted: true,
        };
        self.append_entry(&entry)?;
        self.data.remove(key);
        Ok(())
    }

    fn list_keys(&self) -> Vec<String> {
        self.data.keys().cloned().collect()
    }
}

/// Coordination trait for substrate replication.
/// In single-node mode, `LocalCoordinator` auto-commits all proposals.
/// In distributed mode, implementations would coordinate quorum.
pub trait Coordinator {
    fn propose(&mut self, key: &str, value: Value) -> Result<(), String>;
    fn read(&self, key: &str) -> Option<&Value>;
    fn replication_factor(&self) -> u32;
}

/// Single-node coordinator that auto-commits all proposals.
pub struct LocalCoordinator {
    storage: Box<dyn SubstrateStorage>,
}

impl LocalCoordinator {
    pub fn new(storage: Box<dyn SubstrateStorage>) -> Self {
        Self { storage }
    }
}

impl Coordinator for LocalCoordinator {
    fn propose(&mut self, key: &str, value: Value) -> Result<(), String> {
        self.storage.put(key, value)
    }

    fn read(&self, key: &str) -> Option<&Value> {
        self.storage.get(key)
    }

    fn replication_factor(&self) -> u32 {
        1
    }
}

/// Replicated coordinator that sends writes to peer replicas via TCP.
/// Uses a simple quorum protocol: proposal succeeds when quorum_size peers ack.
pub struct ReplicatedCoordinator {
    local: Box<dyn SubstrateStorage>,
    replicas: Vec<String>, // TCP endpoints of peer replicas
    quorum_size: usize,
    /// Optional signing key for proposal integrity verification.
    signing_key: Option<Vec<u8>>,
}

/// Request/response for replica proposal forwarding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicaProposal {
    pub key: String,
    pub value: Value,
    /// HMAC-SHA256 signature over `key + serialized(value)` when signing key is set.
    #[serde(default)]
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplicaResponse {
    pub ok: bool,
    pub error: Option<String>,
}

/// Compute HMAC-SHA256 signature for a proposal (key + serialized value).
pub fn compute_proposal_signature(signing_key: &[u8], key: &str, value: &Value) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    type HmacSha256 = Hmac<Sha256>;
    let mut mac = HmacSha256::new_from_slice(signing_key).expect("HMAC can take key of any size");
    mac.update(key.as_bytes());
    let value_json = serde_json::to_string(value).unwrap_or_default();
    mac.update(value_json.as_bytes());
    let result = mac.finalize();
    result.into_bytes().iter().map(|b| format!("{b:02x}")).collect::<String>()
}

/// Verify a proposal signature. Returns Ok(()) if valid, Err if invalid.
pub fn verify_proposal_signature(
    signing_key: &[u8],
    key: &str,
    value: &Value,
    signature: &str,
) -> Result<(), String> {
    let expected = compute_proposal_signature(signing_key, key, value);
    if expected == signature {
        Ok(())
    } else {
        Err("invalid proposal signature".to_string())
    }
}

impl ReplicatedCoordinator {
    pub fn new(local: Box<dyn SubstrateStorage>, replicas: Vec<String>) -> Self {
        let quorum_size = (replicas.len() + 1) / 2 + 1; // majority including self
        Self {
            local,
            replicas,
            quorum_size,
            signing_key: None,
        }
    }

    /// Create a new coordinator with a signing key for proposal verification.
    pub fn with_signing_key(local: Box<dyn SubstrateStorage>, replicas: Vec<String>, signing_key: Vec<u8>) -> Self {
        let quorum_size = (replicas.len() + 1) / 2 + 1;
        Self {
            local,
            replicas,
            quorum_size,
            signing_key: Some(signing_key),
        }
    }

    /// Set the signing key after construction.
    pub fn set_signing_key(&mut self, key: Vec<u8>) {
        self.signing_key = Some(key);
    }

    /// Add a replica endpoint. Recalculates quorum size.
    pub fn add_replica(&mut self, endpoint: String) {
        if !self.replicas.contains(&endpoint) {
            self.replicas.push(endpoint);
            self.recalculate_quorum();
        }
    }

    /// Remove a replica endpoint. Recalculates quorum size.
    pub fn remove_replica(&mut self, endpoint: &str) {
        self.replicas.retain(|e| e != endpoint);
        self.recalculate_quorum();
    }

    fn recalculate_quorum(&mut self) {
        self.quorum_size = (self.replicas.len() + 1) / 2 + 1;
    }

    /// Send a proposal to a single replica via TCP. Returns true on success.
    fn send_proposal_sync(endpoint: &str, key: &str, value: &Value, signature: Option<String>) -> bool {
        use std::io::{Read as IoRead, Write as IoWrite};
        use std::net::TcpStream;

        let proposal = ReplicaProposal {
            key: key.to_string(),
            value: value.clone(),
            signature,
        };
        let data = match serde_json::to_vec(&proposal) {
            Ok(d) => d,
            Err(_) => return false,
        };

        let stream = match TcpStream::connect(endpoint) {
            Ok(s) => s,
            Err(_) => return false,
        };
        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok();
        stream.set_write_timeout(Some(std::time::Duration::from_secs(5))).ok();

        let mut stream = stream;
        // Write length-prefixed frame
        let len = (data.len() as u32).to_be_bytes();
        if stream.write_all(&len).is_err() || stream.write_all(&data).is_err() {
            return false;
        }
        let _ = stream.flush();

        // Read response
        let mut len_buf = [0u8; 4];
        if stream.read_exact(&mut len_buf).is_err() {
            return false;
        }
        let resp_len = u32::from_be_bytes(len_buf) as usize;
        let mut resp_buf = vec![0u8; resp_len];
        if stream.read_exact(&mut resp_buf).is_err() {
            return false;
        }
        match serde_json::from_slice::<ReplicaResponse>(&resp_buf) {
            Ok(resp) => resp.ok,
            Err(_) => false,
        }
    }
}

impl Coordinator for ReplicatedCoordinator {
    fn propose(&mut self, key: &str, value: Value) -> Result<(), String> {
        // Count self as one success
        let mut successes = 1;

        // Store locally first
        self.local.put(key, value.clone())?;

        // Compute signature if signing key is set
        let signature = self.signing_key.as_ref().map(|sk| {
            compute_proposal_signature(sk, key, &value)
        });

        // Send to replicas
        for endpoint in &self.replicas {
            if Self::send_proposal_sync(endpoint, key, &value, signature.clone()) {
                successes += 1;
            }
        }

        if successes >= self.quorum_size {
            Ok(())
        } else {
            Err(format!(
                "quorum not reached: {successes}/{} (need {})",
                self.replicas.len() + 1,
                self.quorum_size
            ))
        }
    }

    fn read(&self, key: &str) -> Option<&Value> {
        self.local.get(key)
    }

    fn replication_factor(&self) -> u32 {
        (self.replicas.len() + 1) as u32
    }
}

/// Verify and accept a proposal from a remote replica.
/// If a signing key is provided, the proposal must carry a valid signature.
/// Returns `Ok(())` if accepted, `Err(message)` if rejected.
pub fn verify_and_accept_proposal(
    signing_key: &[u8],
    proposal: &ReplicaProposal,
) -> Result<(), String> {
    match &proposal.signature {
        Some(sig) => verify_proposal_signature(signing_key, &proposal.key, &proposal.value, sig),
        None => Err("proposal has no signature but signing key is set".to_string()),
    }
}

/// Convenience wrapper: accept a proposal with optional signing key.
/// If no signing key is provided, the proposal is accepted unconditionally.
pub fn accept_proposal(
    proposal: &ReplicaProposal,
    signing_key: Option<&[u8]>,
) -> Result<(), String> {
    match signing_key {
        Some(key) => verify_and_accept_proposal(key, proposal),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_memory_storage_basic() {
        let mut storage = InMemoryStorage::new();
        storage.put("key1", Value::String("val1".into())).unwrap();
        assert_eq!(
            storage.get("key1"),
            Some(&Value::String("val1".to_string()))
        );
        assert_eq!(storage.get("nonexistent"), None);
    }

    #[test]
    fn test_in_memory_storage_delete() {
        let mut storage = InMemoryStorage::new();
        storage.put("key1", Value::Int(42)).unwrap();
        storage.delete("key1").unwrap();
        assert_eq!(storage.get("key1"), None);
    }

    #[test]
    fn test_in_memory_storage_list_keys() {
        let mut storage = InMemoryStorage::new();
        storage.put("a", Value::Int(1)).unwrap();
        storage.put("b", Value::Int(2)).unwrap();
        storage.put("c", Value::Int(3)).unwrap();

        let mut keys = storage.list_keys();
        keys.sort();
        assert_eq!(keys, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_file_storage_write_recover() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("storage.ndjson");

        // Write
        {
            let mut storage = FileStorage::open(&path).unwrap();
            storage.put("key1", Value::String("hello".into())).unwrap();
            storage.put("key2", Value::Int(42)).unwrap();
        }

        // Recover
        {
            let storage = FileStorage::open(&path).unwrap();
            assert_eq!(
                storage.get("key1"),
                Some(&Value::String("hello".to_string()))
            );
            assert_eq!(storage.get("key2"), Some(&Value::Int(42)));
        }
    }

    #[test]
    fn test_file_storage_delete_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("storage.ndjson");

        // Write then delete
        {
            let mut storage = FileStorage::open(&path).unwrap();
            storage.put("key1", Value::String("hello".into())).unwrap();
            storage.put("key2", Value::Int(42)).unwrap();
            storage.delete("key1").unwrap();
        }

        // Recover — key1 should be gone
        {
            let storage = FileStorage::open(&path).unwrap();
            assert_eq!(storage.get("key1"), None);
            assert_eq!(storage.get("key2"), Some(&Value::Int(42)));
        }
    }

    #[test]
    fn test_file_storage_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("storage.ndjson");

        // Write initial value
        {
            let mut storage = FileStorage::open(&path).unwrap();
            storage.put("key1", Value::String("old".into())).unwrap();
        }

        // Overwrite
        {
            let mut storage = FileStorage::open(&path).unwrap();
            storage.put("key1", Value::String("new".into())).unwrap();
        }

        // Recover — should have new value
        {
            let storage = FileStorage::open(&path).unwrap();
            assert_eq!(
                storage.get("key1"),
                Some(&Value::String("new".to_string()))
            );
        }
    }

    #[test]
    fn test_file_storage_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("storage.ndjson");

        let storage = FileStorage::open(&path).unwrap();
        assert!(storage.list_keys().is_empty());
    }

    #[test]
    fn test_local_coordinator() {
        let storage = Box::new(InMemoryStorage::new());
        let mut coord = LocalCoordinator::new(storage);

        coord.propose("k1", Value::Int(1)).unwrap();
        assert_eq!(coord.read("k1"), Some(&Value::Int(1)));
        assert_eq!(coord.replication_factor(), 1);
    }

    #[test]
    fn test_local_coordinator_with_file_storage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("coord.ndjson");

        {
            let storage = Box::new(FileStorage::open(&path).unwrap());
            let mut coord = LocalCoordinator::new(storage);
            coord.propose("k1", Value::Int(100)).unwrap();
            assert_eq!(coord.read("k1"), Some(&Value::Int(100)));
        }

        // Verify persistence after coordinator drop
        {
            let storage = FileStorage::open(&path).unwrap();
            assert_eq!(storage.get("k1"), Some(&Value::Int(100)));
        }
    }

    #[test]
    fn test_replicated_coordinator_no_replicas() {
        // With zero replicas, quorum is 1 (just self), so always succeeds
        let storage = Box::new(InMemoryStorage::new());
        let mut coord = ReplicatedCoordinator::new(storage, vec![]);
        coord.propose("k1", Value::Int(42)).unwrap();
        assert_eq!(coord.read("k1"), Some(&Value::Int(42)));
        assert_eq!(coord.replication_factor(), 1);
    }

    #[test]
    fn test_replicated_coordinator_unreachable_replicas() {
        // With 2 replicas that can't be reached, quorum (2 of 3) is not met
        let storage = Box::new(InMemoryStorage::new());
        let replicas = vec![
            "127.0.0.1:59991".to_string(),
            "127.0.0.1:59992".to_string(),
        ];
        let mut coord = ReplicatedCoordinator::new(storage, replicas);
        // Quorum = (2+1)/2+1 = 2, we only have 1 success (self)
        let result = coord.propose("k1", Value::Int(42));
        assert!(result.is_err(), "should fail: quorum not reached");
        assert!(result.unwrap_err().contains("quorum not reached"));
    }

    #[test]
    fn test_replicated_coordinator_with_mock_replica() {
        use std::net::TcpListener;
        use std::io::{Read, Write as IoWrite};

        // Start a mock replica server
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            // Read proposal
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).unwrap();
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut data = vec![0u8; len];
            stream.read_exact(&mut data).unwrap();

            let proposal: ReplicaProposal = serde_json::from_slice(&data).unwrap();
            assert_eq!(proposal.key, "k1");

            // Send success response
            let resp = ReplicaResponse { ok: true, error: None };
            let resp_data = serde_json::to_vec(&resp).unwrap();
            let resp_len = (resp_data.len() as u32).to_be_bytes();
            stream.write_all(&resp_len).unwrap();
            stream.write_all(&resp_data).unwrap();
        });

        let storage = Box::new(InMemoryStorage::new());
        let replicas = vec![addr.to_string()];
        let mut coord = ReplicatedCoordinator::new(storage, replicas);

        // With 1 replica, quorum = (1+1)/2+1 = 2. Self + 1 replica = 2 = quorum met
        coord.propose("k1", Value::Int(42)).unwrap();
        assert_eq!(coord.read("k1"), Some(&Value::Int(42)));
        assert_eq!(coord.replication_factor(), 2);

        server.join().unwrap();
    }

    #[test]
    fn test_replicated_coordinator_majority_failure() {
        use std::net::TcpListener;
        use std::io::{Read, Write as IoWrite};

        // Start one working replica and use an unreachable address for another
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).unwrap();
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut data = vec![0u8; len];
            stream.read_exact(&mut data).unwrap();

            let resp = ReplicaResponse { ok: true, error: None };
            let resp_data = serde_json::to_vec(&resp).unwrap();
            let resp_len = (resp_data.len() as u32).to_be_bytes();
            stream.write_all(&resp_len).unwrap();
            stream.write_all(&resp_data).unwrap();
        });

        let storage = Box::new(InMemoryStorage::new());
        let replicas = vec![
            addr.to_string(),
            "127.0.0.1:59993".to_string(), // unreachable
            "127.0.0.1:59994".to_string(), // unreachable
        ];
        let mut coord = ReplicatedCoordinator::new(storage, replicas);
        // quorum = (3+1)/2+1 = 3, we get 2 (self + working replica)
        let result = coord.propose("k1", Value::Int(42));
        assert!(result.is_err(), "should fail: only 2 of 4 ack'd, need 3");

        server.join().unwrap();
    }

    #[test]
    fn test_replicated_coordinator_minority_failure() {
        use std::net::TcpListener;
        use std::io::{Read, Write as IoWrite};

        // Start 2 working replicas and 1 unreachable
        fn mock_replica() -> (std::net::SocketAddr, std::thread::JoinHandle<()>) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let handle = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut len_buf = [0u8; 4];
                stream.read_exact(&mut len_buf).unwrap();
                let len = u32::from_be_bytes(len_buf) as usize;
                let mut data = vec![0u8; len];
                stream.read_exact(&mut data).unwrap();

                let resp = ReplicaResponse { ok: true, error: None };
                let resp_data = serde_json::to_vec(&resp).unwrap();
                let resp_len = (resp_data.len() as u32).to_be_bytes();
                stream.write_all(&resp_len).unwrap();
                stream.write_all(&resp_data).unwrap();
            });
            (addr, handle)
        }

        let (addr1, h1) = mock_replica();
        let (addr2, h2) = mock_replica();

        let storage = Box::new(InMemoryStorage::new());
        let replicas = vec![
            addr1.to_string(),
            addr2.to_string(),
            "127.0.0.1:59995".to_string(), // unreachable
        ];
        let mut coord = ReplicatedCoordinator::new(storage, replicas);
        // quorum = (3+1)/2+1 = 3, we get 3 (self + 2 replicas) — should succeed
        coord.propose("k1", Value::Int(42)).unwrap();
        assert_eq!(coord.read("k1"), Some(&Value::Int(42)));

        h1.join().unwrap();
        h2.join().unwrap();
    }

    #[test]
    fn test_replica_proposal_serde() {
        let proposal = ReplicaProposal {
            key: "test_key".to_string(),
            value: Value::Int(42),
            signature: None,
        };
        let json = serde_json::to_string(&proposal).unwrap();
        let decoded: ReplicaProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.key, "test_key");
        assert_eq!(decoded.value, Value::Int(42));
        assert!(decoded.signature.is_none());
    }

    #[test]
    fn test_proposal_signature_round_trip() {
        let key = b"proposal-secret";
        let sig = compute_proposal_signature(key, "k1", &Value::Int(42));
        assert_eq!(sig.len(), 64);
        assert!(verify_proposal_signature(key, "k1", &Value::Int(42), &sig).is_ok());
    }

    #[test]
    fn test_bad_proposal_signature_rejected() {
        let key = b"proposal-secret";
        let result = verify_proposal_signature(
            key,
            "k1",
            &Value::Int(42),
            "0000000000000000000000000000000000000000000000000000000000000000",
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_signed_proposal_serde() {
        let proposal = ReplicaProposal {
            key: "k1".to_string(),
            value: Value::String("data".into()),
            signature: Some("abcd1234".to_string()),
        };
        let json = serde_json::to_string(&proposal).unwrap();
        let decoded: ReplicaProposal = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.signature, Some("abcd1234".to_string()));
    }

    #[test]
    fn test_proposal_signature_backward_compat() {
        // Old proposals without signature should deserialize with None
        let proposal = ReplicaProposal {
            key: "k1".to_string(),
            value: Value::Int(42),
            signature: None,
        };
        let json = serde_json::to_string(&proposal).unwrap();
        // Remove signature field to simulate old format
        let json_no_sig = json.replace(r#","signature":null"#, "");
        let decoded: ReplicaProposal = serde_json::from_str(&json_no_sig).unwrap();
        assert!(decoded.signature.is_none());
        assert_eq!(decoded.key, "k1");
    }

    #[test]
    fn test_add_replica_and_quorum_recalculation() {
        let storage = Box::new(InMemoryStorage::new());
        let mut coord = ReplicatedCoordinator::new(storage, vec![]);
        assert_eq!(coord.replication_factor(), 1);
        // quorum = (0+1)/2+1 = 1

        coord.add_replica("127.0.0.1:9001".to_string());
        assert_eq!(coord.replication_factor(), 2);
        // quorum = (1+1)/2+1 = 2

        coord.add_replica("127.0.0.1:9002".to_string());
        assert_eq!(coord.replication_factor(), 3);
        // quorum = (2+1)/2+1 = 2

        // Adding duplicate should not change anything
        coord.add_replica("127.0.0.1:9001".to_string());
        assert_eq!(coord.replication_factor(), 3);
    }

    #[test]
    fn test_remove_replica_and_quorum_recalculation() {
        let storage = Box::new(InMemoryStorage::new());
        let replicas = vec![
            "127.0.0.1:9001".to_string(),
            "127.0.0.1:9002".to_string(),
        ];
        let mut coord = ReplicatedCoordinator::new(storage, replicas);
        assert_eq!(coord.replication_factor(), 3);

        coord.remove_replica("127.0.0.1:9001");
        assert_eq!(coord.replication_factor(), 2);

        coord.remove_replica("127.0.0.1:9002");
        assert_eq!(coord.replication_factor(), 1);

        // Removing non-existent replica is a no-op
        coord.remove_replica("127.0.0.1:9999");
        assert_eq!(coord.replication_factor(), 1);
    }

    #[test]
    fn test_dynamic_replica_addition_with_proposal() {
        use std::net::TcpListener;
        use std::io::{Read, Write as IoWrite};

        // Start with no replicas
        let storage = Box::new(InMemoryStorage::new());
        let mut coord = ReplicatedCoordinator::new(storage, vec![]);

        // Propose should succeed with just self
        coord.propose("k1", Value::Int(1)).unwrap();
        assert_eq!(coord.read("k1"), Some(&Value::Int(1)));

        // Start a mock replica
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut len_buf = [0u8; 4];
            stream.read_exact(&mut len_buf).unwrap();
            let len = u32::from_be_bytes(len_buf) as usize;
            let mut data = vec![0u8; len];
            stream.read_exact(&mut data).unwrap();

            let resp = ReplicaResponse { ok: true, error: None };
            let resp_data = serde_json::to_vec(&resp).unwrap();
            let resp_len = (resp_data.len() as u32).to_be_bytes();
            stream.write_all(&resp_len).unwrap();
            stream.write_all(&resp_data).unwrap();
        });

        // Add replica dynamically
        coord.add_replica(addr.to_string());
        assert_eq!(coord.replication_factor(), 2);

        // Propose should still succeed (quorum 2: self + 1 replica)
        coord.propose("k2", Value::Int(2)).unwrap();
        assert_eq!(coord.read("k2"), Some(&Value::Int(2)));

        server.join().unwrap();
    }

    // === Gap 10: Proposal signature verification on receiver ===

    #[test]
    fn test_verify_and_accept_valid_proposal() {
        let key = b"shared_secret";
        let proposal_key = "test_key";
        let proposal_value = Value::String("test_value".into());
        let sig = compute_proposal_signature(key, proposal_key, &proposal_value);
        let proposal = ReplicaProposal {
            key: proposal_key.to_string(),
            value: proposal_value,
            signature: Some(sig),
        };
        assert!(verify_and_accept_proposal(key, &proposal).is_ok());
    }

    #[test]
    fn test_verify_and_accept_invalid_signature() {
        let key = b"shared_secret";
        let proposal = ReplicaProposal {
            key: "test_key".to_string(),
            value: Value::Int(42),
            signature: Some("bad_signature".to_string()),
        };
        let result = verify_and_accept_proposal(key, &proposal);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid"));
    }

    #[test]
    fn test_verify_and_accept_missing_signature() {
        let key = b"shared_secret";
        let proposal = ReplicaProposal {
            key: "test_key".to_string(),
            value: Value::Int(42),
            signature: None,
        };
        let result = verify_and_accept_proposal(key, &proposal);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no signature"));
    }

    #[test]
    fn test_accept_proposal_no_key() {
        let proposal = ReplicaProposal {
            key: "test_key".to_string(),
            value: Value::Int(42),
            signature: None,
        };
        // No signing key → accept unconditionally
        assert!(accept_proposal(&proposal, None).is_ok());
    }
}
