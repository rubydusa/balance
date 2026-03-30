use super::event::EventBus;
use crate::runtime::value::Value;

/// A comparison operator for field constraints.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ComparisonOp {
    Eq,  // =
    Lt,  // <
    Gt,  // >
    Lte, // <=
    Gte, // >=
}

/// A value constraint for event field matching.
#[derive(Debug, Clone)]
pub enum PredicateValue {
    String(String),
    Int(i64),
    Bool(bool),
}

/// A typed constraint on an event field: field op value.
#[derive(Debug, Clone)]
pub struct FieldConstraint {
    pub field: String,
    pub op: ComparisonOp,
    pub value: PredicateValue,
}

/// A predicate matching events by type, optional key binding, and field constraints.
#[derive(Debug, Clone)]
pub struct EventPredicate {
    pub event_type: String,
    pub key_binding: Option<String>,
    pub field_constraints: Vec<FieldConstraint>,
}

impl EventPredicate {
    pub fn simple(event_type: &str, key: Option<&str>) -> Self {
        Self {
            event_type: event_type.to_string(),
            key_binding: key.map(|s| s.to_string()),
            field_constraints: Vec::new(),
        }
    }
}

/// A law expressed as an implication: if antecedent event exists,
/// then consequent event must exist with the same key.
#[derive(Debug, Clone)]
pub enum GuaranteeLaw {
    /// A(k) => exists B(k)
    Implies {
        antecedent: EventPredicate,
        consequent: EventPredicate,
    },
    /// Conjunction of multiple laws: all must hold.
    Conjunction(Vec<GuaranteeLaw>),
    /// Temporal ordering: A must precede B.
    Ordering {
        before: EventPredicate,
        after: EventPredicate,
    },
    /// A(k) => B(k) within Nms — implication with temporal window.
    TimeBounded {
        antecedent: EventPredicate,
        consequent: EventPredicate,
        window_ms: u64,
    },
    /// NOT A(k) — asserts that no event matching predicate exists.
    Absence {
        predicate: EventPredicate,
    },
}

/// A named guarantee with its laws.
#[derive(Debug, Clone)]
pub struct Guarantee {
    pub name: String,
    pub source: String,
    pub laws: Vec<GuaranteeLaw>,
}

impl Guarantee {
    pub fn new(name: &str, source: &str) -> Self {
        Self {
            name: name.to_string(),
            source: source.to_string(),
            laws: Vec::new(),
        }
    }

    pub fn add_implies(&mut self, antecedent_type: &str, consequent_type: &str, key: &str) {
        self.laws.push(GuaranteeLaw::Implies {
            antecedent: EventPredicate::simple(antecedent_type, Some(key)),
            consequent: EventPredicate::simple(consequent_type, Some(key)),
        });
    }
}

/// Verify a guarantee holds over the current event trace.
/// For each event matching the antecedent, checks that a matching
/// consequent exists with the same key value.
pub fn verify_guarantee(guarantee: &Guarantee, event_bus: &EventBus) -> Result<(), String> {
    let source_events = event_bus.stream_events(&guarantee.source);

    for law in &guarantee.laws {
        verify_law(law, &guarantee.name, &source_events)?;
    }

    Ok(())
}

/// Extract the logical time from an event for guarantee verification.
/// Prefers lamport_time (from distributed causal ordering) when available,
/// falling back to the monotonic event timestamp.
fn event_logical_time(event: &super::event::Event) -> u64 {
    if let Some(Value::Int(t)) = event.data.get("lamport_time") {
        *t as u64
    } else {
        event.timestamp
    }
}

/// Check if an event matches a predicate's field constraints.
fn matches_field_constraints(
    event: &super::event::Event,
    predicate: &EventPredicate,
) -> bool {
    for constraint in &predicate.field_constraints {
        match event.data.get(&constraint.field) {
            Some(val) => {
                let matches = match (&constraint.op, &constraint.value, val) {
                    (ComparisonOp::Eq, PredicateValue::Int(i), v) => v == &Value::Int(*i),
                    (ComparisonOp::Eq, PredicateValue::String(s), v) => v == &Value::String(s.clone()),
                    (ComparisonOp::Eq, PredicateValue::Bool(b), v) => v == &Value::Bool(*b),
                    (ComparisonOp::Lt, PredicateValue::Int(i), Value::Int(v)) => *v < *i,
                    (ComparisonOp::Gt, PredicateValue::Int(i), Value::Int(v)) => *v > *i,
                    (ComparisonOp::Lte, PredicateValue::Int(i), Value::Int(v)) => *v <= *i,
                    (ComparisonOp::Gte, PredicateValue::Int(i), Value::Int(v)) => *v >= *i,
                    _ => false,
                };
                if !matches {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

/// Filter events by type and field constraints.
fn matching_events<'a>(
    events: &'a [super::event::Event],
    predicate: &EventPredicate,
) -> Vec<&'a super::event::Event> {
    events
        .iter()
        .filter(|e| e.event_type == predicate.event_type && matches_field_constraints(e, predicate))
        .collect()
}

fn verify_law(
    law: &GuaranteeLaw,
    guarantee_name: &str,
    source_events: &[super::event::Event],
) -> Result<(), String> {
    match law {
        GuaranteeLaw::Implies {
            antecedent,
            consequent,
        } => {
            let antecedent_events = matching_events(source_events, antecedent);

            for ante_event in &antecedent_events {
                let key_field = antecedent.key_binding.as_deref().unwrap_or("key");
                let ante_key = ante_event.data.get(key_field);

                let consequent_exists = matching_events(source_events, consequent).iter().any(|e| {
                    if event_logical_time(e) < event_logical_time(ante_event) {
                        return false;
                    }
                    let cons_key_field =
                        consequent.key_binding.as_deref().unwrap_or("key");
                    let cons_key = e.data.get(cons_key_field);
                    match (ante_key, cons_key) {
                        (Some(a), Some(b)) => a == b,
                        (None, None) => true,
                        _ => false,
                    }
                });

                if !consequent_exists {
                    let key_display = ante_key
                        .map(|v| format!("{v}"))
                        .unwrap_or_else(|| "<no key>".to_string());
                    return Err(format!(
                        "guarantee '{}' violated: event '{}' with key {} \
                         has no matching '{}' (consequent must follow antecedent temporally)",
                        guarantee_name,
                        antecedent.event_type,
                        key_display,
                        consequent.event_type,
                    ));
                }
            }
        }
        GuaranteeLaw::Conjunction(laws) => {
            for sub_law in laws {
                verify_law(sub_law, guarantee_name, source_events)?;
            }
        }
        GuaranteeLaw::Ordering { before, after } => {
            let after_events = matching_events(source_events, after);

            for after_event in &after_events {
                let key_field = after.key_binding.as_deref().unwrap_or("key");
                let after_key = after_event.data.get(key_field);

                let before_exists = matching_events(source_events, before).iter().any(|e| {
                    if event_logical_time(e) > event_logical_time(after_event) {
                        return false;
                    }
                    let before_key_field =
                        before.key_binding.as_deref().unwrap_or("key");
                    let before_key = e.data.get(before_key_field);
                    match (after_key, before_key) {
                        (Some(a), Some(b)) => a == b,
                        (None, None) => true,
                        _ => false,
                    }
                });

                if !before_exists {
                    let key_display = after_key
                        .map(|v| format!("{v}"))
                        .unwrap_or_else(|| "<no key>".to_string());
                    return Err(format!(
                        "guarantee '{}' violated: event '{}' with key {} \
                         has no preceding '{}'",
                        guarantee_name,
                        after.event_type,
                        key_display,
                        before.event_type,
                    ));
                }
            }
        }
        GuaranteeLaw::TimeBounded {
            antecedent,
            consequent,
            window_ms,
        } => {
            let antecedent_events = matching_events(source_events, antecedent);

            for ante_event in &antecedent_events {
                let key_field = antecedent.key_binding.as_deref().unwrap_or("key");
                let ante_key = ante_event.data.get(key_field);

                let consequent_exists = matching_events(source_events, consequent).iter().any(|e| {
                    if e.timestamp < ante_event.timestamp {
                        return false;
                    }
                    // Check within time window
                    if event_logical_time(e) - event_logical_time(ante_event) > *window_ms {
                        return false;
                    }
                    let cons_key_field =
                        consequent.key_binding.as_deref().unwrap_or("key");
                    let cons_key = e.data.get(cons_key_field);
                    match (ante_key, cons_key) {
                        (Some(a), Some(b)) => a == b,
                        (None, None) => true,
                        _ => false,
                    }
                });

                if !consequent_exists {
                    let key_display = ante_key
                        .map(|v| format!("{v}"))
                        .unwrap_or_else(|| "<no key>".to_string());
                    return Err(format!(
                        "guarantee '{}' violated: event '{}' with key {} \
                         has no matching '{}' within {}ms window",
                        guarantee_name,
                        antecedent.event_type,
                        key_display,
                        consequent.event_type,
                        window_ms,
                    ));
                }
            }
        }
        GuaranteeLaw::Absence { predicate } => {
            let found = matching_events(source_events, predicate);
            if !found.is_empty() {
                return Err(format!(
                    "guarantee '{}' violated: event '{}' must not exist but {} found",
                    guarantee_name,
                    predicate.event_type,
                    found.len(),
                ));
            }
        }
    }
    Ok(())
}

/// Parse a guarantee law from a string.
///
/// Supported formats:
/// - `"A(key) => B(key)"` — implication
/// - `"A(key) => B(key) within 5000ms"` — time-bounded implication
/// - `"A(key) must_precede B(key)"` — temporal ordering
/// - `"A(key) => B(key) AND C(key) => D(key)"` — conjunction
/// - `"NOT error(key)"` — absence (event must not exist)
pub fn parse_law(body: &str) -> Option<GuaranteeLaw> {
    let body = body.trim();

    // Check for absence (NOT predicate)
    if body.starts_with("NOT ") {
        let predicate = parse_predicate(body[4..].trim())?;
        return Some(GuaranteeLaw::Absence { predicate });
    }

    // Check for conjunction (AND)
    if body.contains(" AND ") {
        let parts: Vec<&str> = body.split(" AND ").collect();
        let mut laws = Vec::new();
        for part in parts {
            laws.push(parse_law(part.trim())?);
        }
        return Some(GuaranteeLaw::Conjunction(laws));
    }

    // Check for ordering (must_precede)
    if body.contains("must_precede") {
        let parts: Vec<&str> = body.split("must_precede").collect();
        if parts.len() != 2 {
            return None;
        }
        let before = parse_predicate(parts[0].trim())?;
        let after = parse_predicate(parts[1].trim())?;
        return Some(GuaranteeLaw::Ordering { before, after });
    }

    // Check for implication with optional time window
    let parts: Vec<&str> = body.split("=>").collect();
    if parts.len() != 2 {
        return None;
    }
    let ante = parse_predicate(parts[0].trim())?;

    // Check for "within Nms" suffix on consequent
    let cons_text = parts[1].trim();
    if let Some(within_idx) = cons_text.find("within ") {
        let cons_part = cons_text[..within_idx].trim();
        let window_part = cons_text[within_idx + 7..].trim();
        let window_ms = window_part.trim_end_matches("ms").parse::<u64>().ok()?;
        let cons = parse_predicate(cons_part)?;
        return Some(GuaranteeLaw::TimeBounded {
            antecedent: ante,
            consequent: cons,
            window_ms,
        });
    }

    let cons = parse_predicate(cons_text)?;
    Some(GuaranteeLaw::Implies {
        antecedent: ante,
        consequent: cons,
    })
}

fn parse_predicate_value(s: &str) -> PredicateValue {
    if let Ok(i) = s.parse::<i64>() {
        PredicateValue::Int(i)
    } else if s == "true" {
        PredicateValue::Bool(true)
    } else if s == "false" {
        PredicateValue::Bool(false)
    } else {
        PredicateValue::String(s.to_string())
    }
}

fn parse_predicate(s: &str) -> Option<EventPredicate> {
    // Handle both "event_type(key)" and plain "event_type"
    // Also handle "∃ event_type(key)" or "exists event_type(key)"
    let s = s
        .trim()
        .trim_start_matches("∃")
        .trim_start_matches("exists")
        .trim();

    if let Some(paren_start) = s.find('(') {
        let event_type = s[..paren_start].trim().to_string();
        let inner = s[paren_start + 1..].trim_end_matches(')').trim();

        // Parse field constraints: "key, status=ok, count=3"
        let mut key_binding = None;
        let mut field_constraints = Vec::new();

        for part in inner.split(',') {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            // Parse: field<=val, field>=val, field<val, field>val, field=val
            // Check two-char operators first to avoid partial matches.
            if let Some(idx) = part.find("<=") {
                let field = part[..idx].trim().to_string();
                let value_str = part[idx + 2..].trim();
                let value = parse_predicate_value(value_str);
                field_constraints.push(FieldConstraint { field, op: ComparisonOp::Lte, value });
            } else if let Some(idx) = part.find(">=") {
                let field = part[..idx].trim().to_string();
                let value_str = part[idx + 2..].trim();
                let value = parse_predicate_value(value_str);
                field_constraints.push(FieldConstraint { field, op: ComparisonOp::Gte, value });
            } else if let Some(idx) = part.find('<') {
                let field = part[..idx].trim().to_string();
                let value_str = part[idx + 1..].trim();
                let value = parse_predicate_value(value_str);
                field_constraints.push(FieldConstraint { field, op: ComparisonOp::Lt, value });
            } else if let Some(idx) = part.find('>') {
                let field = part[..idx].trim().to_string();
                let value_str = part[idx + 1..].trim();
                let value = parse_predicate_value(value_str);
                field_constraints.push(FieldConstraint { field, op: ComparisonOp::Gt, value });
            } else if let Some(eq_idx) = part.find('=') {
                let field = part[..eq_idx].trim().to_string();
                let value_str = part[eq_idx + 1..].trim();
                let value = parse_predicate_value(value_str);
                field_constraints.push(FieldConstraint { field, op: ComparisonOp::Eq, value });
            } else if key_binding.is_none() {
                key_binding = Some(part.to_string());
            }
        }

        Some(EventPredicate {
            event_type,
            key_binding,
            field_constraints,
        })
    } else {
        Some(EventPredicate {
            event_type: s.to_string(),
            key_binding: None,
            field_constraints: Vec::new(),
        })
    }
}

/// The built-in "commit_requires_accept" guarantee for ReplicatedLog.
pub fn commit_requires_accept_guarantee(source: &str) -> Guarantee {
    let mut g = Guarantee::new("commit_requires_accept", source);
    g.add_implies("append_accepted", "quorum_committed", "key");
    g
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::event::EventBus;
    use crate::runtime::value::Value;
    use std::collections::HashMap;

    #[test]
    fn test_guarantee_passes_when_both_events_exist() {
        let mut bus = EventBus::new();

        let mut data1 = HashMap::new();
        data1.insert("key".to_string(), Value::String("k1".into()));
        bus.publish("log".into(), "append_accepted".into(), data1);

        let mut data2 = HashMap::new();
        data2.insert("key".to_string(), Value::String("k1".into()));
        bus.publish("log".into(), "quorum_committed".into(), data2);

        let g = commit_requires_accept_guarantee("log");
        assert!(verify_guarantee(&g, &bus).is_ok());
    }

    #[test]
    fn test_guarantee_fails_when_consequent_missing() {
        let mut bus = EventBus::new();

        let mut data = HashMap::new();
        data.insert("key".to_string(), Value::String("k1".into()));
        bus.publish("log".into(), "append_accepted".into(), data);

        // No quorum_committed event

        let g = commit_requires_accept_guarantee("log");
        let result = verify_guarantee(&g, &bus);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("violated"));
    }

    #[test]
    fn test_guarantee_fails_when_keys_dont_match() {
        let mut bus = EventBus::new();

        let mut data1 = HashMap::new();
        data1.insert("key".to_string(), Value::String("k1".into()));
        bus.publish("log".into(), "append_accepted".into(), data1);

        let mut data2 = HashMap::new();
        data2.insert("key".to_string(), Value::String("k2".into()));
        bus.publish("log".into(), "quorum_committed".into(), data2);

        let g = commit_requires_accept_guarantee("log");
        let result = verify_guarantee(&g, &bus);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_law_simple() {
        let law = parse_law("append_accepted(key) => quorum_committed(key)");
        assert!(law.is_some());
        match law.unwrap() {
            GuaranteeLaw::Implies {
                antecedent,
                consequent,
            } => {
                assert_eq!(antecedent.event_type, "append_accepted");
                assert_eq!(antecedent.key_binding, Some("key".to_string()));
                assert_eq!(consequent.event_type, "quorum_committed");
                assert_eq!(consequent.key_binding, Some("key".to_string()));
            }
            other => panic!("expected Implies, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_law_with_exists() {
        let law = parse_law("append_accepted(key) => ∃ quorum_committed(key)");
        assert!(law.is_some());
    }

    #[test]
    fn test_empty_trace_passes() {
        let bus = EventBus::new();
        let g = commit_requires_accept_guarantee("log");
        assert!(verify_guarantee(&g, &bus).is_ok());
    }

    #[test]
    fn test_parse_law_conjunction() {
        let law = parse_law("A(k) => B(k) AND C(k) => D(k)");
        assert!(law.is_some());
        match law.unwrap() {
            GuaranteeLaw::Conjunction(laws) => {
                assert_eq!(laws.len(), 2);
            }
            other => panic!("expected Conjunction, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_law_ordering() {
        let law = parse_law("validate(key) must_precede commit(key)");
        assert!(law.is_some());
        match law.unwrap() {
            GuaranteeLaw::Ordering { before, after } => {
                assert_eq!(before.event_type, "validate");
                assert_eq!(after.event_type, "commit");
            }
            other => panic!("expected Ordering, got {:?}", other),
        }
    }

    #[test]
    fn test_ordering_guarantee_passes() {
        let mut bus = EventBus::new();

        let mut data1 = HashMap::new();
        data1.insert("key".to_string(), Value::String("k1".into()));
        bus.publish("log".into(), "validate".into(), data1);

        let mut data2 = HashMap::new();
        data2.insert("key".to_string(), Value::String("k1".into()));
        bus.publish("log".into(), "commit".into(), data2);

        let mut g = Guarantee::new("order_test", "log");
        g.laws.push(GuaranteeLaw::Ordering {
            before: EventPredicate::simple("validate", Some("key")),
            after: EventPredicate::simple("commit", Some("key")),
        });
        assert!(verify_guarantee(&g, &bus).is_ok());
    }

    #[test]
    fn test_ordering_guarantee_fails() {
        let mut bus = EventBus::new();

        // Only commit, no validate — should fail
        let mut data = HashMap::new();
        data.insert("key".to_string(), Value::String("k1".into()));
        bus.publish("log".into(), "commit".into(), data);

        let mut g = Guarantee::new("order_test", "log");
        g.laws.push(GuaranteeLaw::Ordering {
            before: EventPredicate::simple("validate", Some("key")),
            after: EventPredicate::simple("commit", Some("key")),
        });
        let result = verify_guarantee(&g, &bus);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("violated"));
    }

    #[test]
    fn test_parse_law_time_bounded() {
        let law = parse_law("request(key) => response(key) within 5000ms");
        assert!(law.is_some());
        match law.unwrap() {
            GuaranteeLaw::TimeBounded {
                antecedent,
                consequent,
                window_ms,
            } => {
                assert_eq!(antecedent.event_type, "request");
                assert_eq!(consequent.event_type, "response");
                assert_eq!(window_ms, 5000);
            }
            other => panic!("expected TimeBounded, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_law_absence() {
        let law = parse_law("NOT error(key)");
        assert!(law.is_some());
        match law.unwrap() {
            GuaranteeLaw::Absence { predicate } => {
                assert_eq!(predicate.event_type, "error");
                assert_eq!(predicate.key_binding, Some("key".to_string()));
            }
            other => panic!("expected Absence, got {:?}", other),
        }
    }

    #[test]
    fn test_parse_predicate_field_constraints() {
        let law = parse_law("request(key, status=ok) => response(key)");
        assert!(law.is_some());
        match law.unwrap() {
            GuaranteeLaw::Implies { antecedent, .. } => {
                assert_eq!(antecedent.event_type, "request");
                assert_eq!(antecedent.key_binding, Some("key".to_string()));
                assert_eq!(antecedent.field_constraints.len(), 1);
                assert_eq!(antecedent.field_constraints[0].field, "status");
                assert_eq!(antecedent.field_constraints[0].op, ComparisonOp::Eq);
                assert!(matches!(antecedent.field_constraints[0].value, PredicateValue::String(ref s) if s == "ok"));
            }
            other => panic!("expected Implies, got {:?}", other),
        }
    }

    #[test]
    fn test_time_bounded_passes_within_window() {
        let mut bus = EventBus::new();

        let mut data1 = HashMap::new();
        data1.insert("key".to_string(), Value::String("k1".into()));
        bus.publish("svc".into(), "request".into(), data1);

        let mut data2 = HashMap::new();
        data2.insert("key".to_string(), Value::String("k1".into()));
        bus.publish("svc".into(), "response".into(), data2);

        let mut g = Guarantee::new("timeout_test", "svc");
        g.laws.push(GuaranteeLaw::TimeBounded {
            antecedent: EventPredicate::simple("request", Some("key")),
            consequent: EventPredicate::simple("response", Some("key")),
            window_ms: 10000,
        });
        assert!(verify_guarantee(&g, &bus).is_ok());
    }

    #[test]
    fn test_absence_passes_when_no_events() {
        let bus = EventBus::new();
        let mut g = Guarantee::new("no_errors", "svc");
        g.laws.push(GuaranteeLaw::Absence {
            predicate: EventPredicate::simple("error", None),
        });
        assert!(verify_guarantee(&g, &bus).is_ok());
    }

    #[test]
    fn test_absence_fails_when_event_exists() {
        let mut bus = EventBus::new();
        bus.publish("svc".into(), "error".into(), HashMap::new());

        let mut g = Guarantee::new("no_errors", "svc");
        g.laws.push(GuaranteeLaw::Absence {
            predicate: EventPredicate::simple("error", None),
        });
        let result = verify_guarantee(&g, &bus);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must not exist"));
    }

    #[test]
    fn test_field_constraint_matching() {
        let mut bus = EventBus::new();

        let mut data1 = HashMap::new();
        data1.insert("key".to_string(), Value::String("k1".into()));
        data1.insert("status".to_string(), Value::String("ok".into()));
        bus.publish("svc".into(), "request".into(), data1);

        let mut data2 = HashMap::new();
        data2.insert("key".to_string(), Value::String("k1".into()));
        bus.publish("svc".into(), "response".into(), data2);

        let mut g = Guarantee::new("status_test", "svc");
        g.laws.push(GuaranteeLaw::Implies {
            antecedent: EventPredicate {
                event_type: "request".into(),
                key_binding: Some("key".into()),
                field_constraints: vec![
                    FieldConstraint {
                        field: "status".to_string(),
                        op: ComparisonOp::Eq,
                        value: PredicateValue::String("ok".to_string()),
                    },
                ],
            },
            consequent: EventPredicate::simple("response", Some("key")),
        });
        assert!(verify_guarantee(&g, &bus).is_ok());
    }

    #[test]
    fn test_lamport_time_used_for_ordering() {
        let mut bus = EventBus::new();

        // Publish events with lamport_time that reverses monotonic order
        let mut data1 = HashMap::new();
        data1.insert("key".to_string(), Value::String("k1".into()));
        data1.insert("lamport_time".to_string(), Value::Int(10));
        bus.publish("log".into(), "validate".into(), data1);

        let mut data2 = HashMap::new();
        data2.insert("key".to_string(), Value::String("k1".into()));
        data2.insert("lamport_time".to_string(), Value::Int(20));
        bus.publish("log".into(), "commit".into(), data2);

        let mut g = Guarantee::new("lamport_order", "log");
        g.laws.push(GuaranteeLaw::Ordering {
            before: EventPredicate::simple("validate", Some("key")),
            after: EventPredicate::simple("commit", Some("key")),
        });
        assert!(verify_guarantee(&g, &bus).is_ok());
    }

    #[test]
    fn test_fallback_to_monotonic_when_no_lamport() {
        // Without lamport_time, should use monotonic timestamp
        let mut bus = EventBus::new();

        let mut data1 = HashMap::new();
        data1.insert("key".to_string(), Value::String("k1".into()));
        bus.publish("log".into(), "append_accepted".into(), data1);

        let mut data2 = HashMap::new();
        data2.insert("key".to_string(), Value::String("k1".into()));
        bus.publish("log".into(), "quorum_committed".into(), data2);

        let g = commit_requires_accept_guarantee("log");
        assert!(verify_guarantee(&g, &bus).is_ok());
    }

    #[test]
    fn test_time_bounded_uses_lamport() {
        let mut bus = EventBus::new();

        let mut data1 = HashMap::new();
        data1.insert("key".to_string(), Value::String("k1".into()));
        data1.insert("lamport_time".to_string(), Value::Int(100));
        bus.publish("svc".into(), "request".into(), data1);

        let mut data2 = HashMap::new();
        data2.insert("key".to_string(), Value::String("k1".into()));
        data2.insert("lamport_time".to_string(), Value::Int(150));
        bus.publish("svc".into(), "response".into(), data2);

        // Window of 100 should pass (150 - 100 = 50 <= 100)
        let mut g = Guarantee::new("lamport_window", "svc");
        g.laws.push(GuaranteeLaw::TimeBounded {
            antecedent: EventPredicate::simple("request", Some("key")),
            consequent: EventPredicate::simple("response", Some("key")),
            window_ms: 100,
        });
        assert!(verify_guarantee(&g, &bus).is_ok());

        // Window of 10 should fail (150 - 100 = 50 > 10)
        let mut g2 = Guarantee::new("lamport_window_tight", "svc");
        g2.laws.push(GuaranteeLaw::TimeBounded {
            antecedent: EventPredicate::simple("request", Some("key")),
            consequent: EventPredicate::simple("response", Some("key")),
            window_ms: 10,
        });
        assert!(verify_guarantee(&g2, &bus).is_err());
    }

    #[test]
    fn test_comparison_lt_matches() {
        let mut bus = EventBus::new();
        let mut data = HashMap::new();
        data.insert("key".to_string(), Value::String("k1".into()));
        data.insert("value".to_string(), Value::Int(5));
        bus.publish("svc".into(), "put_accepted".into(), data);

        // value<10 should match (5 < 10)
        let mut g = Guarantee::new("lt_test", "svc");
        g.laws.push(GuaranteeLaw::Absence {
            predicate: EventPredicate {
                event_type: "put_accepted".into(),
                key_binding: Some("key".into()),
                field_constraints: vec![FieldConstraint {
                    field: "value".to_string(),
                    op: ComparisonOp::Lt,
                    value: PredicateValue::Int(10),
                }],
            },
        });
        // Event with value=5 matches value<10, so Absence should FAIL
        let result = verify_guarantee(&g, &bus);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must not exist"));
    }

    #[test]
    fn test_comparison_gt_no_match() {
        let mut bus = EventBus::new();
        let mut data = HashMap::new();
        data.insert("key".to_string(), Value::String("k1".into()));
        data.insert("value".to_string(), Value::Int(5));
        bus.publish("svc".into(), "put_accepted".into(), data);

        // value>10 should NOT match (5 > 10 is false)
        let mut g = Guarantee::new("gt_test", "svc");
        g.laws.push(GuaranteeLaw::Absence {
            predicate: EventPredicate {
                event_type: "put_accepted".into(),
                key_binding: Some("key".into()),
                field_constraints: vec![FieldConstraint {
                    field: "value".to_string(),
                    op: ComparisonOp::Gt,
                    value: PredicateValue::Int(10),
                }],
            },
        });
        // Event with value=5 does NOT match value>10, so Absence passes
        assert!(verify_guarantee(&g, &bus).is_ok());
    }

    #[test]
    fn test_comparison_lte_gte_boundary() {
        let mut bus = EventBus::new();
        let mut data = HashMap::new();
        data.insert("key".to_string(), Value::String("k1".into()));
        data.insert("value".to_string(), Value::Int(10));
        bus.publish("svc".into(), "event".into(), data);

        // value<=10 should match (10 <= 10)
        let mut g = Guarantee::new("lte_test", "svc");
        g.laws.push(GuaranteeLaw::Absence {
            predicate: EventPredicate {
                event_type: "event".into(),
                key_binding: Some("key".into()),
                field_constraints: vec![FieldConstraint {
                    field: "value".to_string(),
                    op: ComparisonOp::Lte,
                    value: PredicateValue::Int(10),
                }],
            },
        });
        assert!(verify_guarantee(&g, &bus).is_err());

        // value>=10 should match (10 >= 10)
        let mut g2 = Guarantee::new("gte_test", "svc");
        g2.laws.push(GuaranteeLaw::Absence {
            predicate: EventPredicate {
                event_type: "event".into(),
                key_binding: Some("key".into()),
                field_constraints: vec![FieldConstraint {
                    field: "value".to_string(),
                    op: ComparisonOp::Gte,
                    value: PredicateValue::Int(10),
                }],
            },
        });
        assert!(verify_guarantee(&g2, &bus).is_err());

        // value>=11 should NOT match (10 >= 11 is false)
        let mut g3 = Guarantee::new("gte_miss", "svc");
        g3.laws.push(GuaranteeLaw::Absence {
            predicate: EventPredicate {
                event_type: "event".into(),
                key_binding: Some("key".into()),
                field_constraints: vec![FieldConstraint {
                    field: "value".to_string(),
                    op: ComparisonOp::Gte,
                    value: PredicateValue::Int(11),
                }],
            },
        });
        assert!(verify_guarantee(&g3, &bus).is_ok());
    }

    #[test]
    fn test_absence_with_comparison_negative_value() {
        let mut bus = EventBus::new();
        let mut data = HashMap::new();
        data.insert("key".to_string(), Value::String("acct-alice".into()));
        data.insert("value".to_string(), Value::Int(-500));
        bus.publish("svc".into(), "put_accepted".into(), data);

        // NOT put_accepted(key, value<0) — should fail because -500 < 0
        let law = parse_law("NOT put_accepted(key, value<0)").unwrap();
        let mut g = Guarantee::new("no_negative", "svc");
        g.laws.push(law);
        let result = verify_guarantee(&g, &bus);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no_negative"));
    }

    #[test]
    fn test_parse_predicate_comparison() {
        let law = parse_law("NOT put_accepted(key, value<0)").unwrap();
        match law {
            GuaranteeLaw::Absence { predicate } => {
                assert_eq!(predicate.event_type, "put_accepted");
                assert_eq!(predicate.key_binding, Some("key".to_string()));
                assert_eq!(predicate.field_constraints.len(), 1);
                assert_eq!(predicate.field_constraints[0].field, "value");
                assert_eq!(predicate.field_constraints[0].op, ComparisonOp::Lt);
                assert!(matches!(predicate.field_constraints[0].value, PredicateValue::Int(0)));
            }
            other => panic!("expected Absence, got {:?}", other),
        }

        // Also test >=
        let law2 = parse_law("NOT event(key, count>=100)").unwrap();
        match law2 {
            GuaranteeLaw::Absence { predicate } => {
                assert_eq!(predicate.field_constraints[0].field, "count");
                assert_eq!(predicate.field_constraints[0].op, ComparisonOp::Gte);
                assert!(matches!(predicate.field_constraints[0].value, PredicateValue::Int(100)));
            }
            other => panic!("expected Absence, got {:?}", other),
        }
    }
}
