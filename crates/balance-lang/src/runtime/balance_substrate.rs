//! User-defined substrate implementation.
//!
//! When a Balance program declares a substrate with `state` and op bodies,
//! `BalanceSubstrate` implements the `Substrate` trait by running the op
//! bodies via a synchronous mini-evaluator (no async, no service dispatch).

use std::collections::HashMap;

use crate::ast::*;
use crate::lexer::span::Spanned;
use crate::runtime::event::EventBus;
use crate::runtime::interaction::InteractionKind;
use crate::runtime::substrate::Substrate;
use crate::runtime::value::Value;

/// A user-defined substrate backed by Balance code.
pub struct BalanceSubstrate {
    instance_name: String,
    event_source: String,
    /// Mutable state bindings.
    state: HashMap<String, Value>,
    /// Which state variables are mutable.
    mutable_state: std::collections::HashSet<String>,
    /// Op AST indexed by op name.
    ops: HashMap<String, SubstrateOp>,
    /// Event types this substrate emits.
    emitted_events: Vec<String>,
    next_offset: u64,
    /// Maps local dep name (in `uses` clause) → substrate instance name in registry.
    pub dep_names: HashMap<String, String>,
    /// Function declarations available in substrate ops.
    fn_decls: HashMap<String, FnDecl>,
    /// On-clause event handlers for this substrate.
    pub on_clauses: Vec<OnClauseDecl>,
    /// Maps local dep name → (substrate_type, substrate_service_id) for capability binding.
    pub dep_service_ids: HashMap<String, (String, String)>,
}

impl BalanceSubstrate {
    pub fn new(
        instance_name: &str,
        event_source: &str,
        state: HashMap<String, Value>,
        mutable_state: std::collections::HashSet<String>,
        ops: HashMap<String, SubstrateOp>,
        emitted_events: Vec<String>,
        dep_names: HashMap<String, String>,
        fn_decls: HashMap<String, FnDecl>,
        on_clauses: Vec<OnClauseDecl>,
        dep_service_ids: HashMap<String, (String, String)>,
    ) -> Self {
        Self {
            instance_name: instance_name.to_string(),
            event_source: event_source.to_string(),
            state,
            mutable_state,
            ops,
            emitted_events,
            next_offset: 0,
            dep_names,
            fn_decls,
            on_clauses,
            dep_service_ids,
        }
    }

    /// Access substrate state (for on-clause execution in evaluator).
    pub fn state(&self) -> &HashMap<String, Value> {
        &self.state
    }

    /// Set a state variable (for on-clause writeback in evaluator).
    pub fn set_state(&mut self, name: &str, value: Value) {
        self.state.insert(name.to_string(), value);
    }

    /// Get the set of mutable state variable names.
    pub fn mutable_state_names(&self) -> &std::collections::HashSet<String> {
        &self.mutable_state
    }

    /// Returns true if this substrate has dependencies on other substrates.
    pub fn has_deps(&self) -> bool {
        !self.dep_names.is_empty()
    }

    /// Access op declarations.
    pub fn ops(&self) -> &HashMap<String, SubstrateOp> {
        &self.ops
    }

    /// Access function declarations.
    pub fn fn_decls(&self) -> &HashMap<String, FnDecl> {
        &self.fn_decls
    }

    /// Access event source string.
    pub fn event_source_str(&self) -> &str {
        &self.event_source
    }

    /// Execute an op with access to sibling substrates via a dep dispatch callback.
    /// Returns (result, event_source, pending_events) — caller must publish the events.
    pub fn execute_op_with_deps(
        &mut self,
        op: &str,
        args: Vec<Value>,
        dep_dispatch: &mut dyn FnMut(&str, &str, Vec<Value>) -> Result<Value, String>,
    ) -> Result<(Value, String, Vec<(String, HashMap<String, Value>)>), String> {
        let op_decl = self.ops.get(op).cloned()
            .ok_or_else(|| format!("substrate '{}': unknown op '{}'", self.instance_name, op))?;

        let body = op_decl.body.as_ref()
            .ok_or_else(|| format!("substrate '{}': op '{}' has no implementation", self.instance_name, op))?;

        let mut env = MiniEnv::new();

        for (i, param) in op_decl.params.iter().enumerate() {
            let val = args.get(i).cloned().unwrap_or(Value::None);
            env.define(param.name.clone(), val, false);
        }

        for (name, val) in &self.state {
            let is_mut = self.mutable_state.contains(name);
            env.define(name.clone(), val.clone(), is_mut);
        }

        let mut pending_events: Vec<(String, HashMap<String, Value>)> = Vec::new();

        let result = eval_stmts_sync_with_deps(body, &mut env, &mut pending_events, &self.dep_names, dep_dispatch, &self.fn_decls);

        for name in &self.mutable_state {
            if let Some(val) = env.lookup(name) {
                self.state.insert(name.clone(), val.clone());
            }
        }

        let offset = self.next_offset;
        for (_, data) in &mut pending_events {
            data.insert("offset".to_string(), Value::Int(offset as i64));
        }
        self.next_offset += 1;

        let event_source = self.event_source.clone();
        match result {
            Ok(val) => Ok((val, event_source, pending_events)),
            Err(MiniError::Return(val)) => Ok((val, event_source, pending_events)),
            Err(MiniError::Error(msg)) => Err(msg),
            Err(MiniError::Break) | Err(MiniError::Continue) => {
                Err("break/continue outside loop in substrate op".to_string())
            }
        }
    }
}

impl Substrate for BalanceSubstrate {
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
        let op_decl = self.ops.get(op).cloned()
            .ok_or_else(|| format!("substrate '{}': unknown op '{}'", self.instance_name, op))?;

        let body = op_decl.body.as_ref()
            .ok_or_else(|| format!("substrate '{}': op '{}' has no implementation", self.instance_name, op))?;

        // Build local scope: params + state
        let mut env = MiniEnv::new();

        // Bind parameters
        for (i, param) in op_decl.params.iter().enumerate() {
            let val = args.get(i).cloned().unwrap_or(Value::None);
            env.define(param.name.clone(), val, false);
        }

        // Bind state variables
        for (name, val) in &self.state {
            let is_mut = self.mutable_state.contains(name);
            env.define(name.clone(), val.clone(), is_mut);
        }

        // Pending events to publish
        let mut pending_events: Vec<(String, HashMap<String, Value>)> = Vec::new();

        // Execute body
        let result = eval_stmts_sync(body, &mut env, &mut pending_events, &self.fn_decls);

        // Write back mutated state
        for name in &self.mutable_state {
            if let Some(val) = env.lookup(name) {
                self.state.insert(name.clone(), val.clone());
            }
        }

        // Publish pending events
        let offset = self.next_offset;
        for (event_type, mut data) in pending_events {
            data.insert("offset".to_string(), Value::Int(offset as i64));
            event_bus.publish(
                self.event_source.clone(),
                event_type,
                data,
            );
        }
        self.next_offset += 1;

        match result {
            Ok(val) => Ok(val),
            Err(MiniError::Return(val)) => Ok(val),
            Err(MiniError::Error(msg)) => Err(msg),
            Err(MiniError::Break) | Err(MiniError::Continue) => {
                Err("break/continue outside loop in substrate op".to_string())
            }
        }
    }

    fn guarantees(&self) -> &[String] {
        &[]
    }

    fn op_kind(&self, _op: &str) -> InteractionKind {
        InteractionKind::Command
    }

    fn set_next_offset(&mut self, offset: u64) {
        self.next_offset = offset;
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
}

// ─── Mini-evaluator ────────────────────────────────────────────────────────

enum MiniError {
    Error(String),
    Return(Value),
    Break,
    Continue,
}

impl From<String> for MiniError {
    fn from(s: String) -> Self {
        MiniError::Error(s)
    }
}

struct MiniEnv {
    vars: Vec<HashMap<String, Value>>,
    mutables: std::collections::HashSet<String>,
}

impl MiniEnv {
    fn new() -> Self {
        Self {
            vars: vec![HashMap::new()],
            mutables: std::collections::HashSet::new(),
        }
    }

    fn define(&mut self, name: String, value: Value, mutable: bool) {
        if let Some(scope) = self.vars.last_mut() {
            scope.insert(name.clone(), value);
        }
        if mutable {
            self.mutables.insert(name);
        }
    }

    fn lookup(&self, name: &str) -> Option<&Value> {
        for scope in self.vars.iter().rev() {
            if let Some(val) = scope.get(name) {
                return Some(val);
            }
        }
        None
    }

    fn set(&mut self, name: &str, value: Value) -> bool {
        for scope in self.vars.iter_mut().rev() {
            if scope.contains_key(name) {
                scope.insert(name.to_string(), value);
                return true;
            }
        }
        false
    }

    fn push_scope(&mut self) {
        self.vars.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        if self.vars.len() > 1 {
            self.vars.pop();
        }
    }
}

fn eval_stmts_sync(
    stmts: &[Spanned<Stmt>],
    env: &mut MiniEnv,
    events: &mut Vec<(String, HashMap<String, Value>)>,
    fn_decls: &HashMap<String, FnDecl>,
) -> Result<Value, MiniError> {
    let mut last = Value::Unit;
    for stmt in stmts {
        last = eval_stmt_sync(&stmt.node, env, events, fn_decls)?;
    }
    Ok(last)
}

fn eval_stmt_sync(
    stmt: &Stmt,
    env: &mut MiniEnv,
    events: &mut Vec<(String, HashMap<String, Value>)>,
    fn_decls: &HashMap<String, FnDecl>,
) -> Result<Value, MiniError> {
    match stmt {
        Stmt::Let { name, value, mutable, .. } => {
            let val = eval_expr_sync(&value.node, env, fn_decls)?;
            env.define(name.clone(), val, *mutable);
            Ok(Value::Unit)
        }
        Stmt::Assign { name, value } => {
            if !env.mutables.contains(name.as_str()) {
                return Err(MiniError::Error(format!(
                    "cannot assign to immutable variable '{name}'"
                )));
            }
            let val = eval_expr_sync(&value.node, env, fn_decls)?;
            if !env.set(name, val) {
                return Err(MiniError::Error(format!("variable '{name}' not found")));
            }
            Ok(Value::Unit)
        }
        Stmt::Return(Some(expr)) => {
            let val = eval_expr_sync(&expr.node, env, fn_decls)?;
            Err(MiniError::Return(val))
        }
        Stmt::Return(None) => Err(MiniError::Return(Value::Unit)),
        Stmt::Expr(expr) => eval_expr_sync(&expr.node, env, fn_decls),
        Stmt::If { condition, then_block, else_block } => {
            let cond = eval_expr_sync(&condition.node, env, fn_decls)?;
            if cond.is_truthy() {
                env.push_scope();
                let r = eval_stmts_sync(then_block, env, events, fn_decls);
                env.pop_scope();
                r
            } else if let Some(else_stmts) = else_block {
                env.push_scope();
                let r = eval_stmts_sync(else_stmts, env, events, fn_decls);
                env.pop_scope();
                r
            } else {
                Ok(Value::Unit)
            }
        }
        Stmt::For { variable, iterable, body } => {
            let iter_val = eval_expr_sync(&iterable.node, env, fn_decls)?;
            match iter_val {
                Value::List(items) => {
                    env.push_scope();
                    for item in items {
                        env.define(variable.clone(), item, false);
                        match eval_stmts_sync(body, env, events, fn_decls) {
                            Ok(_) => {}
                            Err(MiniError::Break) => break,
                            Err(MiniError::Continue) => continue,
                            Err(e) => { env.pop_scope(); return Err(e); }
                        }
                    }
                    env.pop_scope();
                    Ok(Value::Unit)
                }
                _ => Err(MiniError::Error(format!("cannot iterate over {}", iter_val.type_name()))),
            }
        }
        Stmt::While { condition, body } => {
            env.push_scope();
            loop {
                let cond = eval_expr_sync(&condition.node, env, fn_decls)?;
                if !cond.is_truthy() { break; }
                match eval_stmts_sync(body, env, events, fn_decls) {
                    Ok(_) => {}
                    Err(MiniError::Break) => break,
                    Err(MiniError::Continue) => continue,
                    Err(e) => { env.pop_scope(); return Err(e); }
                }
            }
            env.pop_scope();
            Ok(Value::Unit)
        }
        Stmt::Match { expr, arms } => {
            let val = eval_expr_sync(&expr.node, env, fn_decls)?;
            for arm in arms {
                if let Some(bindings) = pattern_matches_sync(&arm.pattern.node, &val) {
                    env.push_scope();
                    for (name, bound_val) in bindings {
                        env.define(name, bound_val, false);
                    }
                    if let Some(ref guard) = arm.guard {
                        let guard_val = eval_expr_sync(&guard.node, env, fn_decls)?;
                        if !guard_val.is_truthy() {
                            env.pop_scope();
                            continue;
                        }
                    }
                    let result = eval_stmts_sync(&arm.body, env, events, fn_decls);
                    env.pop_scope();
                    return result;
                }
            }
            Ok(Value::Unit)
        }
        Stmt::Break => Err(MiniError::Break),
        Stmt::Continue => Err(MiniError::Continue),
        Stmt::Emit { event_type, fields } => {
            let mut data = HashMap::new();
            for (name, expr) in fields {
                let val = eval_expr_sync(&expr.node, env, fn_decls)?;
                data.insert(name.clone(), val);
            }
            events.push((event_type.clone(), data));
            Ok(Value::Unit)
        }
    }
}

fn eval_expr_sync(expr: &Expr, env: &mut MiniEnv, fn_decls: &HashMap<String, FnDecl>) -> Result<Value, MiniError> {
    match expr {
        Expr::Literal(lit) => Ok(literal_to_value_sync(lit)),
        Expr::None => Ok(Value::None),
        Expr::Ident(name) => {
            if name == "ack" {
                // Return a placeholder — ack is called as a function
                return Ok(Value::String("__ack_fn__".to_string()));
            }
            env.lookup(name).cloned().ok_or_else(|| {
                MiniError::Error(format!("undefined variable '{name}'"))
            })
        }
        Expr::Binary { op, left, right } => {
            let l = eval_expr_sync(&left.node, env, fn_decls)?;
            let r = eval_expr_sync(&right.node, env, fn_decls)?;
            apply_binary_op_sync(*op, &l, &r)
        }
        Expr::Unary { op, operand } => {
            let val = eval_expr_sync(&operand.node, env, fn_decls)?;
            apply_unary_op_sync(*op, &val)
        }
        Expr::FnCall { func, args } => {
            let mut eval_args = Vec::new();
            for arg in args {
                eval_args.push(eval_expr_sync(&arg.node, env, fn_decls)?);
            }
            // Handle built-in functions
            if let Expr::Ident(name) = &func.node {
                if name == "ack" {
                    let key = match eval_args.first() {
                        Some(Value::String(k)) => k.clone(),
                        _ => "ack".to_string(),
                    };
                    return Ok(Value::ack(key));
                }
                if name == "ok" {
                    let val = eval_args.into_iter().next().unwrap_or(Value::Unit);
                    return Ok(Value::Ok(Box::new(val)));
                }
                if name == "err" {
                    let val = eval_args.into_iter().next().unwrap_or(Value::Unit);
                    return Ok(Value::Err(Box::new(val)));
                }
                // Look up user-defined functions
                if let Some(fn_decl) = fn_decls.get(name) {
                    env.push_scope();
                    for (i, param) in fn_decl.params.iter().enumerate() {
                        let val = eval_args.get(i).cloned().unwrap_or(Value::None);
                        env.define(param.name.clone(), val, false);
                    }
                    let mut events = Vec::new();
                    let result = eval_stmts_sync(&fn_decl.body, env, &mut events, fn_decls);
                    env.pop_scope();
                    return match result {
                        Ok(val) => Ok(val),
                        Err(MiniError::Return(val)) => Ok(val),
                        Err(e) => Err(e),
                    };
                }
            }
            Err(MiniError::Error(format!("function call not supported in substrate op: {:?}", func.node)))
        }
        Expr::MethodCall { receiver, method, args } => {
            let recv = eval_expr_sync(&receiver.node, env, fn_decls)?;
            let mut eval_args = Vec::new();
            for arg in args {
                eval_args.push(eval_expr_sync(&arg.node, env, fn_decls)?);
            }
            eval_method_sync(&recv, method, &eval_args)
        }
        Expr::FieldAccess { receiver, field } => {
            let val = eval_expr_sync(&receiver.node, env, fn_decls)?;
            match &val {
                Value::Struct { fields, .. } => fields.get(field).cloned()
                    .ok_or_else(|| MiniError::Error(format!("no field '{field}' on struct"))),
                Value::Map(map) => map.get(field).cloned()
                    .ok_or_else(|| MiniError::Error(format!("no key '{field}' in map"))),
                _ => Err(MiniError::Error(format!("cannot access field '{field}' on {}", val.type_name()))),
            }
        }
        Expr::Index { receiver, index } => {
            let recv = eval_expr_sync(&receiver.node, env, fn_decls)?;
            let idx = eval_expr_sync(&index.node, env, fn_decls)?;
            match (&recv, &idx) {
                (Value::List(items), Value::Int(i)) => {
                    let i = *i;
                    if i < 0 || i as usize >= items.len() {
                        Err(MiniError::Error(format!("index {i} out of bounds")))
                    } else {
                        Ok(items[i as usize].clone())
                    }
                }
                (Value::Map(map), Value::String(k)) => {
                    Ok(map.get(k).cloned().unwrap_or(Value::None))
                }
                (Value::Bytes(bytes), Value::Int(i)) => {
                    let i = *i;
                    if i < 0 || i as usize >= bytes.len() {
                        Err(MiniError::Error(format!("bytes index {i} out of bounds")))
                    } else {
                        Ok(Value::Int(bytes[i as usize] as i64))
                    }
                }
                _ => Err(MiniError::Error(format!(
                    "cannot index {} with {}", recv.type_name(), idx.type_name()
                ))),
            }
        }
        Expr::ListLiteral { elements } => {
            let mut items = Vec::new();
            for elem in elements {
                items.push(eval_expr_sync(&elem.node, env, fn_decls)?);
            }
            Ok(Value::List(items))
        }
        Expr::MapLiteral { entries } => {
            let mut map = HashMap::new();
            for (key, val) in entries {
                let k = eval_expr_sync(&key.node, env, fn_decls)?;
                let v = eval_expr_sync(&val.node, env, fn_decls)?;
                if let Value::String(k) = k {
                    map.insert(k, v);
                }
            }
            Ok(Value::Map(map))
        }
        Expr::StructLiteral { name, fields } => {
            let mut field_map = HashMap::new();
            for (fname, fexpr) in fields {
                let val = eval_expr_sync(&fexpr.node, env, fn_decls)?;
                field_map.insert(fname.clone(), val);
            }
            Ok(Value::Struct { name: name.clone(), fields: field_map })
        }
        Expr::Block(stmts) => {
            let mut events = Vec::new();
            let mut last = Value::Unit;
            for stmt in stmts {
                last = eval_stmt_sync(&stmt.node, env, &mut events, fn_decls)?;
            }
            Ok(last)
        }
        Expr::Try { expr: inner } => {
            let val = eval_expr_sync(&inner.node, env, fn_decls)?;
            match val {
                Value::Ok(v) => Ok(*v),
                Value::Err(_) => Err(MiniError::Return(val)),
                _ => Err(MiniError::Error(format!("? operator requires Ok or Err, got {}", val.type_name()))),
            }
        }
        _ => Err(MiniError::Error(format!("expression not supported in substrate op: {:?}", std::mem::discriminant(expr)))),
    }
}

fn literal_to_value_sync(lit: &Literal) -> Value {
    match lit {
        Literal::String(s) => Value::String(s.clone()),
        Literal::Int(n) => Value::Int(*n),
        Literal::Float(f) => Value::Float(*f),
        Literal::Bool(b) => Value::Bool(*b),
        Literal::Bytes(b) => Value::Bytes(b.clone()),
    }
}

fn apply_binary_op_sync(op: BinaryOp, left: &Value, right: &Value) -> Result<Value, MiniError> {
    match op {
        BinaryOp::Add => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
            (Value::String(a), Value::String(b)) => Ok(Value::String(format!("{a}{b}"))),
            (Value::String(a), other) => Ok(Value::String(format!("{a}{other}"))),
            (other, Value::String(b)) => Ok(Value::String(format!("{other}{b}"))),
            _ => Err(MiniError::Error(format!("cannot add {} and {}", left.type_name(), right.type_name()))),
        },
        BinaryOp::Sub => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a - b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
            _ => Err(MiniError::Error(format!("cannot subtract {} and {}", left.type_name(), right.type_name()))),
        },
        BinaryOp::Mul => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a * b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
            _ => Err(MiniError::Error(format!("cannot multiply {} and {}", left.type_name(), right.type_name()))),
        },
        BinaryOp::Div => match (left, right) {
            (Value::Int(a), Value::Int(b)) if *b != 0 => Ok(Value::Int(a / b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
            _ => Err(MiniError::Error("division error".to_string())),
        },
        BinaryOp::Mod => match (left, right) {
            (Value::Int(a), Value::Int(b)) if *b != 0 => Ok(Value::Int(a % b)),
            _ => Err(MiniError::Error("modulo error".to_string())),
        },
        BinaryOp::Eq => Ok(Value::Bool(left == right)),
        BinaryOp::Neq => Ok(Value::Bool(left != right)),
        BinaryOp::Lt => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
            _ => Err(MiniError::Error(format!("cannot compare {} and {}", left.type_name(), right.type_name()))),
        },
        BinaryOp::Gt => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
            _ => Err(MiniError::Error(format!("cannot compare {} and {}", left.type_name(), right.type_name()))),
        },
        BinaryOp::LtEq => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a <= b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a <= b)),
            _ => Err(MiniError::Error(format!("cannot compare {} and {}", left.type_name(), right.type_name()))),
        },
        BinaryOp::GtEq => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a >= b)),
            (Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a >= b)),
            _ => Err(MiniError::Error(format!("cannot compare {} and {}", left.type_name(), right.type_name()))),
        },
        BinaryOp::And => Ok(Value::Bool(left.is_truthy() && right.is_truthy())),
        BinaryOp::Or => Ok(Value::Bool(left.is_truthy() || right.is_truthy())),
        BinaryOp::BitAnd => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a & b)),
            _ => Err(MiniError::Error("bitwise AND requires Int".to_string())),
        },
        BinaryOp::BitOr => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a | b)),
            _ => Err(MiniError::Error("bitwise OR requires Int".to_string())),
        },
        BinaryOp::BitXor => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a ^ b)),
            _ => Err(MiniError::Error("bitwise XOR requires Int".to_string())),
        },
        BinaryOp::Shl => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a << b)),
            _ => Err(MiniError::Error("shift left requires Int".to_string())),
        },
        BinaryOp::Shr => match (left, right) {
            (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a >> b)),
            _ => Err(MiniError::Error("shift right requires Int".to_string())),
        },
    }
}

fn apply_unary_op_sync(op: UnaryOp, val: &Value) -> Result<Value, MiniError> {
    match op {
        UnaryOp::Neg => match val {
            Value::Int(n) => Ok(Value::Int(-n)),
            Value::Float(f) => Ok(Value::Float(-f)),
            _ => Err(MiniError::Error(format!("cannot negate {}", val.type_name()))),
        },
        UnaryOp::Not => Ok(Value::Bool(!val.is_truthy())),
        UnaryOp::BitNot => match val {
            Value::Int(n) => Ok(Value::Int(!n)),
            _ => Err(MiniError::Error(format!("cannot bitwise NOT {}", val.type_name()))),
        },
    }
}

fn eval_method_sync(recv: &Value, method: &str, args: &[Value]) -> Result<Value, MiniError> {
    match recv {
        Value::String(s) => match method {
            "len" => Ok(Value::Int(s.len() as i64)),
            "contains" => match args.first() {
                Some(Value::String(sub)) => Ok(Value::Bool(s.contains(sub.as_str()))),
                _ => Err(MiniError::Error("contains requires String arg".to_string())),
            },
            "to_bytes" => Ok(Value::Bytes(s.as_bytes().to_vec())),
            _ => Err(MiniError::Error(format!("String has no method '{method}'"))),
        },
        Value::List(items) => match method {
            "len" => Ok(Value::Int(items.len() as i64)),
            "push" => {
                let val = args.first().cloned().unwrap_or(Value::None);
                let mut new = items.clone();
                new.push(val);
                Ok(Value::List(new))
            }
            "to_bytes" => {
                let bytes: Result<Vec<u8>, MiniError> = items
                    .iter()
                    .map(|v| match v {
                        Value::Int(n) => Ok((*n & 0xFF) as u8),
                        _ => Err(MiniError::Error("to_bytes: all list elements must be Int".to_string())),
                    })
                    .collect();
                Ok(Value::Bytes(bytes?))
            }
            _ => Err(MiniError::Error(format!("List has no method '{method}'"))),
        },
        Value::Bytes(bytes) => match method {
            "len" => Ok(Value::Int(bytes.len() as i64)),
            "at" => {
                let i = match args.first() { Some(Value::Int(i)) => *i, _ => return Err(MiniError::Error("at requires Int".to_string())) };
                if i < 0 || i as usize >= bytes.len() { Err(MiniError::Error("bytes index out of bounds".to_string())) }
                else { Ok(Value::Int(bytes[i as usize] as i64)) }
            }
            "slice" => {
                let start = match args.first() { Some(Value::Int(i)) => *i as usize, _ => return Err(MiniError::Error("slice requires Int start".to_string())) };
                let end = match args.get(1) { Some(Value::Int(i)) => *i as usize, _ => return Err(MiniError::Error("slice requires Int end".to_string())) };
                if start > bytes.len() || end > bytes.len() || start > end { Err(MiniError::Error("bytes slice out of bounds".to_string())) }
                else { Ok(Value::Bytes(bytes[start..end].to_vec())) }
            }
            "concat" => {
                let other = match args.first() { Some(Value::Bytes(b)) => b, _ => return Err(MiniError::Error("concat requires Bytes".to_string())) };
                let mut result = bytes.clone();
                result.extend_from_slice(other);
                Ok(Value::Bytes(result))
            }
            "hex" => {
                let hex: String = bytes.iter().map(|b| format!("{:02x}", b)).collect();
                Ok(Value::String(hex))
            }
            "to_list" => Ok(Value::List(bytes.iter().map(|b| Value::Int(*b as i64)).collect())),
            "to_string" => Ok(Value::String(String::from_utf8_lossy(bytes).to_string())),
            "to_int" => {
                if bytes.len() > 8 {
                    return Err(MiniError::Error("to_int: Bytes length must be <= 8".to_string()));
                }
                let mut arr = [0u8; 8];
                arr[8 - bytes.len()..].copy_from_slice(bytes);
                Ok(Value::Int(i64::from_be_bytes(arr)))
            }
            _ => Err(MiniError::Error(format!("Bytes has no method '{method}'"))),
        },
        Value::Int(n) => match method {
            "to_bytes" => {
                let width = match args.first() {
                    Some(Value::Int(w)) => *w,
                    _ => return Err(MiniError::Error("to_bytes requires an Int width argument".to_string())),
                };
                if width < 1 || width > 8 {
                    return Err(MiniError::Error("to_bytes width must be 1-8".to_string()));
                }
                let bytes = n.to_be_bytes();
                Ok(Value::Bytes(bytes[8 - width as usize..].to_vec()))
            }
            "to_float" => Ok(Value::Float(*n as f64)),
            "to_string" => Ok(Value::String(n.to_string())),
            _ => Err(MiniError::Error(format!("Int has no method '{method}'"))),
        },
        Value::Float(f) => match method {
            "floor" => Ok(Value::Float(f.floor())),
            "ceil" => Ok(Value::Float(f.ceil())),
            "round" => Ok(Value::Float(f.round())),
            "abs" => Ok(Value::Float(f.abs())),
            "sqrt" => Ok(Value::Float(f.sqrt())),
            "to_int" => Ok(Value::Int(*f as i64)),
            "to_string" => Ok(Value::String(f.to_string())),
            _ => Err(MiniError::Error(format!("Float has no method '{method}'"))),
        },
        Value::Ok(ref inner) => match method {
            "is_ok" => Ok(Value::Bool(true)),
            "is_err" => Ok(Value::Bool(false)),
            "unwrap" => Ok(*inner.clone()),
            "unwrap_err" => Err(MiniError::Error("called unwrap_err on Ok value".to_string())),
            "unwrap_or" => Ok(*inner.clone()),
            _ => Err(MiniError::Error(format!("Ok has no method '{method}'"))),
        },
        Value::Err(ref inner) => match method {
            "is_ok" => Ok(Value::Bool(false)),
            "is_err" => Ok(Value::Bool(true)),
            "unwrap" => Err(MiniError::Error(format!("called unwrap on Err: {}", inner))),
            "unwrap_err" => Ok(*inner.clone()),
            "unwrap_or" => {
                let default = args.first().cloned().unwrap_or(Value::None);
                Ok(default)
            }
            _ => Err(MiniError::Error(format!("Err has no method '{method}'"))),
        },
        Value::Map(map) => match method {
            "len" => Ok(Value::Int(map.len() as i64)),
            "keys" => Ok(Value::List(map.keys().map(|k| Value::String(k.clone())).collect())),
            "get" => {
                let key = match args.first() { Some(Value::String(k)) => k, _ => return Err(MiniError::Error("get requires String key".to_string())) };
                Ok(map.get(key).cloned().unwrap_or(Value::None))
            }
            _ => Err(MiniError::Error(format!("Map has no method '{method}'"))),
        },
        _ => Err(MiniError::Error(format!("cannot call method '{method}' on {}", recv.type_name()))),
    }
}

// ─── Dep-aware mini-evaluator variants ─────────────────────────────────────

type DepDispatch<'a> = &'a mut dyn FnMut(&str, &str, Vec<Value>) -> Result<Value, String>;

fn eval_stmts_sync_with_deps(
    stmts: &[Spanned<Stmt>],
    env: &mut MiniEnv,
    events: &mut Vec<(String, HashMap<String, Value>)>,
    dep_names: &HashMap<String, String>,
    dep_dispatch: DepDispatch,
    fn_decls: &HashMap<String, FnDecl>,
) -> Result<Value, MiniError> {
    let mut last = Value::Unit;
    for stmt in stmts {
        last = eval_stmt_sync_with_deps(&stmt.node, env, events, dep_names, dep_dispatch, fn_decls)?;
    }
    Ok(last)
}

fn eval_stmt_sync_with_deps(
    stmt: &Stmt,
    env: &mut MiniEnv,
    events: &mut Vec<(String, HashMap<String, Value>)>,
    dep_names: &HashMap<String, String>,
    dep_dispatch: DepDispatch,
    fn_decls: &HashMap<String, FnDecl>,
) -> Result<Value, MiniError> {
    match stmt {
        Stmt::Let { name, value, mutable, .. } => {
            let val = eval_expr_sync_with_deps(&value.node, env, dep_names, dep_dispatch, fn_decls)?;
            env.define(name.clone(), val, *mutable);
            Ok(Value::Unit)
        }
        Stmt::Assign { name, value } => {
            if !env.mutables.contains(name.as_str()) {
                return Err(MiniError::Error(format!(
                    "cannot assign to immutable variable '{name}'"
                )));
            }
            let val = eval_expr_sync_with_deps(&value.node, env, dep_names, dep_dispatch, fn_decls)?;
            if !env.set(name, val) {
                return Err(MiniError::Error(format!("variable '{name}' not found")));
            }
            Ok(Value::Unit)
        }
        Stmt::Return(Some(expr)) => {
            let val = eval_expr_sync_with_deps(&expr.node, env, dep_names, dep_dispatch, fn_decls)?;
            Err(MiniError::Return(val))
        }
        Stmt::Return(None) => Err(MiniError::Return(Value::Unit)),
        Stmt::Expr(expr) => eval_expr_sync_with_deps(&expr.node, env, dep_names, dep_dispatch, fn_decls),
        Stmt::If { condition, then_block, else_block } => {
            let cond = eval_expr_sync_with_deps(&condition.node, env, dep_names, dep_dispatch, fn_decls)?;
            if cond.is_truthy() {
                env.push_scope();
                let r = eval_stmts_sync_with_deps(then_block, env, events, dep_names, dep_dispatch, fn_decls);
                env.pop_scope();
                r
            } else if let Some(else_stmts) = else_block {
                env.push_scope();
                let r = eval_stmts_sync_with_deps(else_stmts, env, events, dep_names, dep_dispatch, fn_decls);
                env.pop_scope();
                r
            } else {
                Ok(Value::Unit)
            }
        }
        Stmt::For { variable, iterable, body } => {
            let iter_val = eval_expr_sync_with_deps(&iterable.node, env, dep_names, dep_dispatch, fn_decls)?;
            match iter_val {
                Value::List(items) => {
                    env.push_scope();
                    for item in items {
                        env.define(variable.clone(), item, false);
                        match eval_stmts_sync_with_deps(body, env, events, dep_names, dep_dispatch, fn_decls) {
                            Ok(_) => {}
                            Err(MiniError::Break) => break,
                            Err(MiniError::Continue) => continue,
                            Err(e) => { env.pop_scope(); return Err(e); }
                        }
                    }
                    env.pop_scope();
                    Ok(Value::Unit)
                }
                _ => Err(MiniError::Error(format!("cannot iterate over {}", iter_val.type_name()))),
            }
        }
        Stmt::While { condition, body } => {
            env.push_scope();
            loop {
                let cond = eval_expr_sync_with_deps(&condition.node, env, dep_names, dep_dispatch, fn_decls)?;
                if !cond.is_truthy() { break; }
                match eval_stmts_sync_with_deps(body, env, events, dep_names, dep_dispatch, fn_decls) {
                    Ok(_) => {}
                    Err(MiniError::Break) => break,
                    Err(MiniError::Continue) => continue,
                    Err(e) => { env.pop_scope(); return Err(e); }
                }
            }
            env.pop_scope();
            Ok(Value::Unit)
        }
        Stmt::Match { expr, arms } => {
            let val = eval_expr_sync_with_deps(&expr.node, env, dep_names, dep_dispatch, fn_decls)?;
            for arm in arms {
                if let Some(bindings) = pattern_matches_sync(&arm.pattern.node, &val) {
                    env.push_scope();
                    for (k, v) in bindings {
                        env.define(k, v, false);
                    }
                    let result = eval_stmts_sync_with_deps(&arm.body, env, events, dep_names, dep_dispatch, fn_decls);
                    env.pop_scope();
                    return result;
                }
            }
            Ok(Value::Unit)
        }
        Stmt::Break => Err(MiniError::Break),
        Stmt::Continue => Err(MiniError::Continue),
        Stmt::Emit { event_type, fields } => {
            let mut data = HashMap::new();
            for (name, expr) in fields {
                let val = eval_expr_sync_with_deps(&expr.node, env, dep_names, dep_dispatch, fn_decls)?;
                data.insert(name.clone(), val);
            }
            events.push((event_type.clone(), data));
            Ok(Value::Unit)
        }
    }
}

fn eval_expr_sync_with_deps(
    expr: &Expr,
    env: &mut MiniEnv,
    dep_names: &HashMap<String, String>,
    dep_dispatch: DepDispatch,
    fn_decls: &HashMap<String, FnDecl>,
) -> Result<Value, MiniError> {
    match expr {
        Expr::Literal(lit) => Ok(literal_to_value_sync(lit)),
        Expr::None => Ok(Value::None),
        Expr::Ident(name) => {
            if name == "ack" {
                return Ok(Value::String("__ack_fn__".to_string()));
            }
            env.lookup(name).cloned().ok_or_else(|| {
                MiniError::Error(format!("undefined variable '{name}'"))
            })
        }
        Expr::Binary { op, left, right } => {
            let l = eval_expr_sync_with_deps(&left.node, env, dep_names, dep_dispatch, fn_decls)?;
            let r = eval_expr_sync_with_deps(&right.node, env, dep_names, dep_dispatch, fn_decls)?;
            apply_binary_op_sync(*op, &l, &r)
        }
        Expr::Unary { op, operand } => {
            let val = eval_expr_sync_with_deps(&operand.node, env, dep_names, dep_dispatch, fn_decls)?;
            apply_unary_op_sync(*op, &val)
        }
        Expr::FnCall { func, args } => {
            let mut eval_args = Vec::new();
            for arg in args {
                eval_args.push(eval_expr_sync_with_deps(&arg.node, env, dep_names, dep_dispatch, fn_decls)?);
            }
            if let Expr::Ident(name) = &func.node {
                if name == "ack" {
                    let key = match eval_args.first() {
                        Some(Value::String(k)) => k.clone(),
                        _ => "ack".to_string(),
                    };
                    return Ok(Value::ack(key));
                }
                if name == "ok" {
                    let val = eval_args.into_iter().next().unwrap_or(Value::Unit);
                    return Ok(Value::Ok(Box::new(val)));
                }
                if name == "err" {
                    let val = eval_args.into_iter().next().unwrap_or(Value::Unit);
                    return Ok(Value::Err(Box::new(val)));
                }
                // Look up user-defined functions
                if let Some(fn_decl) = fn_decls.get(name) {
                    env.push_scope();
                    for (i, param) in fn_decl.params.iter().enumerate() {
                        let val = eval_args.get(i).cloned().unwrap_or(Value::None);
                        env.define(param.name.clone(), val, false);
                    }
                    let mut events = Vec::new();
                    let result = eval_stmts_sync_with_deps(&fn_decl.body, env, &mut events, dep_names, dep_dispatch, fn_decls);
                    env.pop_scope();
                    return match result {
                        Ok(val) => Ok(val),
                        Err(MiniError::Return(val)) => Ok(val),
                        Err(e) => Err(e),
                    };
                }
            }
            Err(MiniError::Error(format!("function call not supported in substrate op: {:?}", func.node)))
        }
        Expr::MethodCall { receiver, method, args } => {
            // Check if receiver is a dep name — dispatch to sibling substrate
            if let Expr::Ident(recv_name) = &receiver.node {
                if let Some(instance_name) = dep_names.get(recv_name) {
                    let mut eval_args = Vec::new();
                    for arg in args {
                        eval_args.push(eval_expr_sync_with_deps(&arg.node, env, dep_names, dep_dispatch, fn_decls)?);
                    }
                    return dep_dispatch(instance_name, method, eval_args)
                        .map_err(MiniError::Error);
                }
            }
            let recv = eval_expr_sync_with_deps(&receiver.node, env, dep_names, dep_dispatch, fn_decls)?;
            let mut eval_args = Vec::new();
            for arg in args {
                eval_args.push(eval_expr_sync_with_deps(&arg.node, env, dep_names, dep_dispatch, fn_decls)?);
            }
            eval_method_sync(&recv, method, &eval_args)
        }
        Expr::FieldAccess { receiver, field } => {
            let val = eval_expr_sync_with_deps(&receiver.node, env, dep_names, dep_dispatch, fn_decls)?;
            match &val {
                Value::Struct { fields, .. } => fields.get(field).cloned()
                    .ok_or_else(|| MiniError::Error(format!("no field '{field}' on struct"))),
                Value::Map(map) => map.get(field).cloned()
                    .ok_or_else(|| MiniError::Error(format!("no key '{field}' in map"))),
                _ => Err(MiniError::Error(format!("cannot access field '{field}' on {}", val.type_name()))),
            }
        }
        Expr::Index { receiver, index } => {
            let recv = eval_expr_sync_with_deps(&receiver.node, env, dep_names, dep_dispatch, fn_decls)?;
            let idx = eval_expr_sync_with_deps(&index.node, env, dep_names, dep_dispatch, fn_decls)?;
            match (&recv, &idx) {
                (Value::List(items), Value::Int(i)) => {
                    let i = *i;
                    if i < 0 || i as usize >= items.len() {
                        Err(MiniError::Error(format!("index {i} out of bounds")))
                    } else {
                        Ok(items[i as usize].clone())
                    }
                }
                (Value::Map(map), Value::String(k)) => {
                    Ok(map.get(k).cloned().unwrap_or(Value::None))
                }
                (Value::Bytes(bytes), Value::Int(i)) => {
                    let i = *i;
                    if i < 0 || i as usize >= bytes.len() {
                        Err(MiniError::Error(format!("bytes index {i} out of bounds")))
                    } else {
                        Ok(Value::Int(bytes[i as usize] as i64))
                    }
                }
                _ => Err(MiniError::Error(format!(
                    "cannot index {} with {}", recv.type_name(), idx.type_name()
                ))),
            }
        }
        Expr::ListLiteral { elements } => {
            let mut items = Vec::new();
            for elem in elements {
                items.push(eval_expr_sync_with_deps(&elem.node, env, dep_names, dep_dispatch, fn_decls)?);
            }
            Ok(Value::List(items))
        }
        Expr::MapLiteral { entries } => {
            let mut map = HashMap::new();
            for (key, val) in entries {
                let k = eval_expr_sync_with_deps(&key.node, env, dep_names, dep_dispatch, fn_decls)?;
                let v = eval_expr_sync_with_deps(&val.node, env, dep_names, dep_dispatch, fn_decls)?;
                if let Value::String(k) = k {
                    map.insert(k, v);
                }
            }
            Ok(Value::Map(map))
        }
        Expr::StructLiteral { name, fields } => {
            let mut field_map = HashMap::new();
            for (fname, fexpr) in fields {
                let val = eval_expr_sync_with_deps(&fexpr.node, env, dep_names, dep_dispatch, fn_decls)?;
                field_map.insert(fname.clone(), val);
            }
            Ok(Value::Struct { name: name.clone(), fields: field_map })
        }
        Expr::Block(stmts) => {
            let mut events = Vec::new();
            let mut last = Value::Unit;
            for stmt in stmts {
                last = eval_stmt_sync_with_deps(&stmt.node, env, &mut events, dep_names, dep_dispatch, fn_decls)?;
            }
            Ok(last)
        }
        Expr::Try { expr: inner } => {
            let val = eval_expr_sync_with_deps(&inner.node, env, dep_names, dep_dispatch, fn_decls)?;
            match val {
                Value::Ok(v) => Ok(*v),
                Value::Err(_) => Err(MiniError::Return(val)),
                _ => Err(MiniError::Error(format!("? operator requires Ok or Err, got {}", val.type_name()))),
            }
        }
        _ => Err(MiniError::Error(format!("expression not supported in substrate op: {:?}", std::mem::discriminant(expr)))),
    }
}

/// Simplified pattern matching for the mini-evaluator.
fn pattern_matches_sync(pattern: &Pattern, value: &Value) -> Option<Vec<(String, Value)>> {
    match pattern {
        Pattern::Wildcard => Some(vec![]),
        Pattern::Ident(name) => Some(vec![(name.clone(), value.clone())]),
        Pattern::Literal(lit) => {
            let lit_val = literal_to_value_sync(lit);
            if lit_val == *value { Some(vec![]) } else { None }
        }
        Pattern::Struct { name, fields } => {
            if let Value::Struct { name: sname, fields: sfields } = value {
                if sname != name { return None; }
                let mut bindings = Vec::new();
                for (field_name, inner_pat) in fields {
                    let field_val = sfields.get(field_name)?;
                    if let Some(inner) = inner_pat {
                        let sub = pattern_matches_sync(&inner.node, field_val)?;
                        bindings.extend(sub);
                    } else {
                        bindings.push((field_name.clone(), field_val.clone()));
                    }
                }
                Some(bindings)
            } else { None }
        }
        Pattern::Some(inner) => {
            if matches!(value, Value::None) { None }
            else { pattern_matches_sync(&inner.node, value) }
        }
        Pattern::None => {
            if matches!(value, Value::None) { Some(vec![]) } else { None }
        }
        Pattern::Ok(inner) => {
            if let Value::Ok(v) = value {
                pattern_matches_sync(&inner.node, v)
            } else { None }
        }
        Pattern::Err(inner) => {
            if let Value::Err(v) = value {
                pattern_matches_sync(&inner.node, v)
            } else { None }
        }
        Pattern::List { elements, rest } => {
            if let Value::List(items) = value {
                if rest.is_some() {
                    if items.len() < elements.len() { return None; }
                } else if items.len() != elements.len() { return None; }
                let mut bindings = Vec::new();
                for (pat, val) in elements.iter().zip(items.iter()) {
                    let sub = pattern_matches_sync(&pat.node, val)?;
                    bindings.extend(sub);
                }
                if let Some(rest_pat) = rest {
                    let rest_items = items[elements.len()..].to_vec();
                    let sub = pattern_matches_sync(&rest_pat.node, &Value::List(rest_items))?;
                    bindings.extend(sub);
                }
                Some(bindings)
            } else { None }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::span::{Span, Spanned};

    fn spanned<T>(node: T) -> Spanned<T> {
        Spanned::new(node, Span::new(0, 0))
    }

    #[test]
    fn test_balance_substrate_basic() {
        // Build a simple counter substrate
        let op = SubstrateOp {
            name: "increment".to_string(),
            params: vec![Param {
                name: "amount".to_string(),
                ty: spanned(TypeExpr::Named {
                    name: "Int".to_string(),
                    type_args: vec![],
                    nullable: false,
                }),
            }],
            return_type: spanned(TypeExpr::Named {
                name: "Int".to_string(),
                type_args: vec![],
                nullable: false,
            }),
            body: Some(vec![
                // count = count + amount
                spanned(Stmt::Assign {
                    name: "count".to_string(),
                    value: spanned(Expr::Binary {
                        op: BinaryOp::Add,
                        left: Box::new(spanned(Expr::Ident("count".to_string()))),
                        right: Box::new(spanned(Expr::Ident("amount".to_string()))),
                    }),
                }),
                // return count
                spanned(Stmt::Return(Some(spanned(Expr::Ident("count".to_string()))))),
            ]),
        };

        let mut state = HashMap::new();
        state.insert("count".to_string(), Value::Int(0));
        let mut mutable_state = std::collections::HashSet::new();
        mutable_state.insert("count".to_string());

        let mut ops = HashMap::new();
        ops.insert("increment".to_string(), op);

        let mut substrate = BalanceSubstrate::new(
            "counter/test",
            "counter/store",
            state,
            mutable_state,
            ops,
            vec![],
            HashMap::new(),
            HashMap::new(),
            vec![],
            HashMap::new(),
        );

        let mut event_bus = EventBus::new();

        let result = substrate.execute_op("increment", vec![Value::Int(5)], &mut event_bus).unwrap();
        assert_eq!(result, Value::Int(5));

        let result = substrate.execute_op("increment", vec![Value::Int(3)], &mut event_bus).unwrap();
        assert_eq!(result, Value::Int(8));
    }
}
