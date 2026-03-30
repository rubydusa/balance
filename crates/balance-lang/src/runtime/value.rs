use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::capability::CapabilityRef;
use super::interaction::InteractionHandle;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum Value {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    None,
    List(Vec<Value>),
    Map(HashMap<String, Value>),
    Struct {
        name: String,
        fields: HashMap<String, Value>,
    },
    Bytes(#[serde(with = "serde_bytes_vec")] Vec<u8>),
    Ok(Box<Value>),
    Err(Box<Value>),
    Capability(CapabilityRef),
    Interaction(InteractionHandle),
    ClosureRef(u64),
    Unit,
}

/// Serde helper to serialize Vec<u8> as base64 or byte array.
mod serde_bytes_vec {
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Accept both byte arrays and sequences of integers
        let v: Vec<u8> = Deserialize::deserialize(deserializer)?;
        Ok(v)
    }
}

impl Value {
    /// Create an Ack value as a normalized Struct with name "Ack" and field "key".
    pub fn ack(key: String) -> Value {
        let mut fields = HashMap::new();
        fields.insert("key".to_string(), Value::String(key));
        Value::Struct {
            name: "Ack".to_string(),
            fields,
        }
    }

    /// Check if this value is an Ack struct.
    pub fn is_ack(&self) -> bool {
        matches!(self, Value::Struct { name, .. } if name == "Ack")
    }

    /// Extract the key from an Ack struct, if this is one.
    pub fn ack_key(&self) -> Option<&str> {
        if let Value::Struct { name, fields } = self {
            if name == "Ack" {
                if let Some(Value::String(k)) = fields.get("key") {
                    return Some(k.as_str());
                }
            }
        }
        None
    }

    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::None => false,
            Value::Int(0) => false,
            Value::String(s) => !s.is_empty(),
            Value::Bytes(b) => !b.is_empty(),
            Value::Ok(_) => true,
            Value::Err(_) => false,
            _ => true,
        }
    }

    /// Readiness check for `select` arms. Unlike `is_truthy()`, an empty list
    /// is NOT ready — this is critical for I/O multiplexing where poll() returns
    /// an empty list when no events are available.
    pub fn is_select_ready(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::None => false,
            Value::Unit => false,
            Value::Int(0) => false,
            Value::List(items) => !items.is_empty(),
            Value::Err(_) => false,
            _ => true,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::String(_) => "String",
            Value::Int(_) => "Int",
            Value::Float(_) => "Float",
            Value::Bool(_) => "Bool",
            Value::None => "None",
            Value::List(_) => "List",
            Value::Map(_) => "Map",
            Value::Bytes(_) => "Bytes",
            Value::Ok(_) => "Ok",
            Value::Err(_) => "Err",
            Value::Struct { name, .. } => {
                if name == "Ack" { "Ack" } else { "Struct" }
            }
            Value::Capability(_) => "Capability",
            Value::Interaction(_) => "Interaction",
            Value::ClosureRef(_) => "Closure",
            Value::Unit => "Unit",
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::String(s) => write!(f, "{s}"),
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(n) => write!(f, "{n}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::None => write!(f, "none"),
            Value::Ok(v) => write!(f, "Ok({v})"),
            Value::Err(v) => write!(f, "Err({v})"),
            Value::Bytes(bytes) => {
                write!(f, "b\"")?;
                for b in bytes {
                    write!(f, "{:02x}", b)?;
                }
                write!(f, "\"")
            }
            Value::List(items) => {
                write!(f, "[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
            Value::Map(map) => {
                write!(f, "{{")?;
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            Value::Struct { name, fields } => {
                write!(f, "{name} {{")?;
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                write!(f, "}}")
            }
            Value::Capability(cap) => write!(f, "<cap {}/{}>", cap.port_name(), cap.service_id()),
            Value::Interaction(h) => write!(f, "<interaction {}.{}>", h.service_id, h.method),
            Value::ClosureRef(id) => write!(f, "<closure {id}>"),
            Value::Unit => write!(f, "()"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ack_as_struct() {
        let ack = Value::ack("x".to_string());
        assert!(ack.is_ack());
        assert_eq!(ack.ack_key(), Some("x"));
        // Field access via struct path works
        if let Value::Struct { fields, .. } = &ack {
            assert_eq!(fields.get("key"), Some(&Value::String("x".to_string())));
        } else {
            panic!("expected Struct");
        }
    }

    #[test]
    fn test_ack_equality() {
        assert_eq!(Value::ack("x".to_string()), Value::ack("x".to_string()));
        assert_ne!(Value::ack("x".to_string()), Value::ack("y".to_string()));
    }

    #[test]
    fn test_list_equality() {
        let a = Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let b = Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let c = Value::List(vec![Value::Int(1), Value::Int(3)]);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn test_map_equality() {
        let mut m1 = HashMap::new();
        m1.insert("a".to_string(), Value::Int(1));
        let mut m2 = HashMap::new();
        m2.insert("a".to_string(), Value::Int(1));
        let mut m3 = HashMap::new();
        m3.insert("a".to_string(), Value::Int(2));
        assert_eq!(Value::Map(m1.clone()), Value::Map(m2));
        assert_ne!(Value::Map(m1), Value::Map(m3));
    }

    #[test]
    fn test_struct_equality() {
        let mut f1 = HashMap::new();
        f1.insert("x".to_string(), Value::Int(1));
        f1.insert("y".to_string(), Value::Int(2));
        let mut f2 = HashMap::new();
        f2.insert("x".to_string(), Value::Int(1));
        f2.insert("y".to_string(), Value::Int(2));
        let s1 = Value::Struct { name: "Point".into(), fields: f1 };
        let s2 = Value::Struct { name: "Point".into(), fields: f2 };
        assert_eq!(s1, s2);

        // Different name
        let mut f3 = HashMap::new();
        f3.insert("x".to_string(), Value::Int(1));
        f3.insert("y".to_string(), Value::Int(2));
        let s3 = Value::Struct { name: "Vec2".into(), fields: f3 };
        assert_ne!(s1, s3);
    }

    #[test]
    fn test_nested_equality() {
        let inner1 = Value::Struct {
            name: "P".into(),
            fields: {
                let mut f = HashMap::new();
                f.insert("v".to_string(), Value::Int(1));
                f
            },
        };
        let inner2 = Value::Struct {
            name: "P".into(),
            fields: {
                let mut f = HashMap::new();
                f.insert("v".to_string(), Value::Int(1));
                f
            },
        };
        let list1 = Value::List(vec![inner1]);
        let list2 = Value::List(vec![inner2]);
        assert_eq!(list1, list2);
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::None, Value::None) => true,
            (Value::Bytes(a), Value::Bytes(b)) => a == b,
            (Value::Ok(a), Value::Ok(b)) => a == b,
            (Value::Err(a), Value::Err(b)) => a == b,
            (Value::Unit, Value::Unit) => true,
            (Value::ClosureRef(a), Value::ClosureRef(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Map(a), Value::Map(b)) => a == b,
            (
                Value::Struct { name: n1, fields: f1 },
                Value::Struct { name: n2, fields: f2 },
            ) => n1 == n2 && f1 == f2,
            _ => false,
        }
    }
}
