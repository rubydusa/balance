use std::collections::HashMap;
use std::path::PathBuf;

use crate::ast::*;

/// Unique type variable counter.
static NEXT_TYPEVAR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// Type variable for unification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeVar(u32);

impl TypeVar {
    fn fresh() -> Self {
        Self(NEXT_TYPEVAR.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

impl std::fmt::Display for TypeVar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "t{}", self.0)
    }
}

/// Internal type representation for HM inference.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// Concrete type: String, Int, Bool, Float, Unit, None, Ack
    Con(String),
    /// Unification variable
    Var(TypeVar),
    /// Capability type: cap Port with optional authority qualifier
    Cap(String, Option<AuthorityQualifier>),
    /// Function type: (params) -> return
    Fun(Vec<Type>, Box<Type>),
    /// Interaction<T>
    Interaction(Box<Type>),
    /// Optional<T>  (T?)
    Optional(Box<Type>),
    /// List<T>
    List(Box<Type>),
    /// Map<K, V>
    Map(Box<Type>, Box<Type>),
    /// Struct with named fields
    Struct(String, Vec<(String, Type)>),
    /// Observed<T, F>
    Observed(Box<Type>, Box<Type>),
    /// Result<Ok, Err>
    Result(Box<Type>, Box<Type>),
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Con(name) => write!(f, "{name}"),
            Type::Var(tv) => write!(f, "{tv}"),
            Type::Cap(port, qualifier) => {
                if let Some(q) = qualifier {
                    write!(f, "cap {port} @{q:?}")
                } else {
                    write!(f, "cap {port}")
                }
            }
            Type::Fun(params, ret) => {
                write!(f, "(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{p}")?;
                }
                write!(f, ") -> {ret}")
            }
            Type::Interaction(inner) => write!(f, "Interaction<{inner}>"),
            Type::Optional(inner) => write!(f, "{inner}?"),
            Type::List(inner) => write!(f, "List<{inner}>"),
            Type::Map(k, v) => write!(f, "Map<{k}, {v}>"),
            Type::Struct(name, _) => write!(f, "{name}"),
            Type::Observed(t, frontier) => write!(f, "Observed<{t}, {frontier}>"),
            Type::Result(ok, err) => write!(f, "Result<{ok}, {err}>"),
        }
    }
}

/// Substitution: maps type variables to their resolved types.
#[derive(Debug, Clone, Default)]
pub struct Substitution {
    map: HashMap<TypeVar, Type>,
}

impl Substitution {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn bind(&mut self, tv: TypeVar, ty: Type) {
        self.map.insert(tv, ty);
    }

    pub fn lookup(&self, tv: &TypeVar) -> Option<&Type> {
        self.map.get(tv)
    }
}

/// Apply substitution to a type, resolving all type variables.
pub fn apply(subst: &Substitution, ty: &Type) -> Type {
    match ty {
        Type::Var(tv) => match subst.lookup(tv) {
            Some(resolved) => apply(subst, resolved),
            None => ty.clone(),
        },
        Type::Fun(params, ret) => Type::Fun(
            params.iter().map(|p| apply(subst, p)).collect(),
            Box::new(apply(subst, ret)),
        ),
        Type::Interaction(inner) => Type::Interaction(Box::new(apply(subst, inner))),
        Type::Optional(inner) => Type::Optional(Box::new(apply(subst, inner))),
        Type::List(inner) => Type::List(Box::new(apply(subst, inner))),
        Type::Map(k, v) => Type::Map(Box::new(apply(subst, k)), Box::new(apply(subst, v))),
        Type::Struct(name, fields) => Type::Struct(
            name.clone(),
            fields
                .iter()
                .map(|(n, t)| (n.clone(), apply(subst, t)))
                .collect(),
        ),
        Type::Observed(t, f) => Type::Observed(
            Box::new(apply(subst, t)),
            Box::new(apply(subst, f)),
        ),
        Type::Result(ok, err) => Type::Result(
            Box::new(apply(subst, ok)),
            Box::new(apply(subst, err)),
        ),
        // Con and Cap are ground types
        _ => ty.clone(),
    }
}

/// Check if a type variable occurs in a type (prevents infinite types).
fn occurs_check(tv: &TypeVar, ty: &Type) -> bool {
    match ty {
        Type::Var(tv2) => tv == tv2,
        Type::Fun(params, ret) => {
            params.iter().any(|p| occurs_check(tv, p)) || occurs_check(tv, ret)
        }
        Type::Interaction(inner)
        | Type::Optional(inner)
        | Type::List(inner) => occurs_check(tv, inner),
        Type::Map(k, v) => occurs_check(tv, k) || occurs_check(tv, v),
        Type::Observed(t, f) => occurs_check(tv, t) || occurs_check(tv, f),
        Type::Result(ok, err) => occurs_check(tv, ok) || occurs_check(tv, err),
        Type::Struct(_, fields) => fields.iter().any(|(_, t)| occurs_check(tv, t)),
        Type::Con(_) | Type::Cap(_, _) => false,
    }
}

/// Check if two authority qualifiers are compatible for cap assignment.
/// Authority widening: @consume ⊂ @borrow ⊂ @delegate ⊂ unqualified.
/// A more restrictive cap can be used where a less restrictive one is expected.
fn authority_compatible(
    actual: &Option<AuthorityQualifier>,
    expected: &Option<AuthorityQualifier>,
) -> bool {
    fn level(q: &Option<AuthorityQualifier>) -> u8 {
        match q {
            Some(AuthorityQualifier::Consume) => 0,
            Some(AuthorityQualifier::Borrow) => 1,
            Some(AuthorityQualifier::Delegate) => 2,
            None => 3,
        }
    }
    // A more restrictive (lower level) cap can be passed where a less restrictive is expected
    level(actual) <= level(expected)
}

/// Unification error.
#[derive(Debug)]
pub struct TypeError {
    pub message: String,
    pub span: Option<crate::lexer::span::Span>,
}

/// Unify two types, updating the substitution.
pub fn unify(t1: &Type, t2: &Type, subst: &mut Substitution) -> Result<(), TypeError> {
    let t1 = apply(subst, t1);
    let t2 = apply(subst, t2);

    match (&t1, &t2) {
        // Same concrete type
        (Type::Con(a), Type::Con(b)) if a == b => Ok(()),
        // Cap wildcard: cap Any unifies with any capability
        (Type::Cap(a, _), Type::Cap(_, _)) if a == "Any" => Ok(()),
        (Type::Cap(_, _), Type::Cap(b, _)) if b == "Any" => Ok(()),
        // Cap types: exact match or authority subtyping
        (Type::Cap(a, qa), Type::Cap(b, qb)) if a == b => {
            // Same port name — check authority compatibility
            // Authority widening: @consume ⊂ @borrow ⊂ @delegate ⊂ unqualified
            // Assignability: a more restricted cap can be assigned where a less restricted is expected
            if qa == qb || authority_compatible(qa, qb) {
                Ok(())
            } else {
                Err(TypeError {
                    message: format!(
                        "capability authority mismatch for port '{}': {:?} vs {:?}",
                        a, qa, qb
                    ),
                    span: None,
                })
            }
        }
        // Var = anything (bind)
        (Type::Var(tv), _) => {
            if t1 == t2 {
                return Ok(());
            }
            if occurs_check(tv, &t2) {
                return Err(TypeError {
                    message: format!("infinite type: {tv} ~ {t2}"),
                    span: None,
                });
            }
            subst.bind(*tv, t2);
            Ok(())
        }
        (_, Type::Var(tv)) => {
            if occurs_check(tv, &t1) {
                return Err(TypeError {
                    message: format!("infinite type: {tv} ~ {t1}"),
                    span: None,
                });
            }
            subst.bind(*tv, t1);
            Ok(())
        }
        // Function types
        (Type::Fun(p1, r1), Type::Fun(p2, r2)) => {
            if p1.len() != p2.len() {
                return Err(TypeError {
                    message: format!(
                        "function arity mismatch: {} vs {} parameters",
                        p1.len(),
                        p2.len()
                    ),
                    span: None,
                });
            }
            for (a, b) in p1.iter().zip(p2.iter()) {
                unify(a, b, subst)?;
            }
            unify(r1, r2, subst)
        }
        // Interaction<T>
        (Type::Interaction(a), Type::Interaction(b)) => unify(a, b, subst),
        // Optional<T>
        (Type::Optional(a), Type::Optional(b)) => unify(a, b, subst),
        // List<T>
        (Type::List(a), Type::List(b)) => unify(a, b, subst),
        // Map<K, V>
        (Type::Map(k1, v1), Type::Map(k2, v2)) => {
            unify(k1, k2, subst)?;
            unify(v1, v2, subst)
        }
        // Observed<T, F>
        (Type::Observed(t1, f1), Type::Observed(t2, f2)) => {
            unify(t1, t2, subst)?;
            unify(f1, f2, subst)
        }
        // Result<Ok, Err>
        (Type::Result(ok1, err1), Type::Result(ok2, err2)) => {
            unify(ok1, ok2, subst)?;
            unify(err1, err2, subst)
        }
        // Struct types (same name, structural field checking)
        (Type::Struct(n1, f1), Type::Struct(n2, f2)) if n1 == n2 => {
            // Structural: every field in f1 must exist in f2 with compatible type
            for (name, ty1) in f1 {
                match f2.iter().find(|(n, _)| n == name) {
                    Some((_, ty2)) => unify(ty1, ty2, subst)?,
                    None => return Err(TypeError {
                        message: format!("struct '{}' missing field '{}'", n1, name),
                        span: None,
                    }),
                }
            }
            Ok(())
        }
        // Mismatch
        _ => Err(TypeError {
            message: format!("type mismatch: {t1} vs {t2}"),
            span: None,
        }),
    }
}

/// Type environment for inference: maps variable names to types.
#[derive(Debug, Clone)]
pub struct InferEnv {
    scopes: Vec<HashMap<String, Type>>,
    /// Port declarations: port name -> method name -> (params, return type)
    ports: HashMap<String, HashMap<String, (Vec<Type>, Type)>>,
    /// Function declarations: fn name -> (params, return type)
    functions: HashMap<String, (Vec<Type>, Type)>,
    /// Optional module root for resolving string-literal dynamic imports.
    pub module_root: Option<PathBuf>,
    /// Module-qualified exports: alias -> (symbol_name -> Type).
    /// Populated from aliased imports (e.g., `import kv.storage as storage`).
    module_exports: HashMap<String, HashMap<String, Type>>,
    /// Warnings collected during inference (non-fatal diagnostics).
    pub warnings: Vec<String>,
}

impl InferEnv {
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
            ports: HashMap::new(),
            functions: HashMap::new(),
            module_root: None,
            module_exports: HashMap::new(),
            warnings: Vec::new(),
        }
    }

    /// Register a module alias with its exported types.
    pub fn register_module_exports(&mut self, alias: &str, exports: HashMap<String, Type>) {
        self.module_exports.insert(alias.to_string(), exports);
    }

    pub fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    pub fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    pub fn define(&mut self, name: String, ty: Type) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, ty);
        }
    }

    pub fn lookup(&self, name: &str) -> Option<&Type> {
        for scope in self.scopes.iter().rev() {
            if let Some(ty) = scope.get(name) {
                return Some(ty);
            }
        }
        None
    }

    pub fn register_port(&mut self, port: &PortDecl) {
        let mut methods = HashMap::new();
        for method in &port.methods {
            let params: Vec<Type> = method
                .node
                .params
                .iter()
                .map(|p| type_expr_to_type(&p.ty.node))
                .collect();
            let ret = type_expr_to_type(&method.node.return_type.node);
            methods.insert(method.node.name.clone(), (params, ret));
        }
        self.ports.insert(port.name.clone(), methods);
    }

    pub fn register_fn(&mut self, f: &FnDecl) {
        let params: Vec<Type> = f
            .params
            .iter()
            .map(|p| type_expr_to_type(&p.ty.node))
            .collect();
        let ret = f
            .return_type
            .as_ref()
            .map(|rt| type_expr_to_type(&rt.node))
            .unwrap_or(Type::Con("Unit".to_string()));
        self.functions.insert(f.name.clone(), (params, ret));
    }

    /// Look up a port method's return type.
    pub fn port_method_return(&self, port: &str, method: &str) -> Option<&Type> {
        self.ports
            .get(port)
            .and_then(|methods| methods.get(method))
            .map(|(_, ret)| ret)
    }
}

/// Convert AST TypeExpr to inference Type.
pub fn type_expr_to_type(te: &TypeExpr) -> Type {
    match te {
        TypeExpr::Named {
            name,
            nullable,
            type_args,
        } => {
            let base = match name.as_str() {
                "String" => Type::Con("String".to_string()),
                "Int" => Type::Con("Int".to_string()),
                "Float" => Type::Con("Float".to_string()),
                "Bool" => Type::Con("Bool".to_string()),
                "Unit" | "()" => Type::Con("Unit".to_string()),
                "EventKey" => Type::Con("String".to_string()),
                "Ack" => Type::Struct("Ack".to_string(), vec![
                    ("key".to_string(), Type::Con("String".to_string())),
                ]),
                "List" => {
                    let inner = type_args
                        .first()
                        .map(|a| type_expr_to_type(&a.node))
                        .unwrap_or(Type::Var(TypeVar::fresh()));
                    Type::List(Box::new(inner))
                }
                "Map" => {
                    let k = type_args
                        .first()
                        .map(|a| type_expr_to_type(&a.node))
                        .unwrap_or(Type::Var(TypeVar::fresh()));
                    let v = type_args
                        .get(1)
                        .map(|a| type_expr_to_type(&a.node))
                        .unwrap_or(Type::Var(TypeVar::fresh()));
                    Type::Map(Box::new(k), Box::new(v))
                }
                "Interaction" => {
                    let inner = type_args
                        .first()
                        .map(|a| type_expr_to_type(&a.node))
                        .unwrap_or(Type::Var(TypeVar::fresh()));
                    Type::Interaction(Box::new(inner))
                }
                "Observed" => {
                    let t = type_args
                        .first()
                        .map(|a| type_expr_to_type(&a.node))
                        .unwrap_or(Type::Var(TypeVar::fresh()));
                    let f = type_args
                        .get(1)
                        .map(|a| type_expr_to_type(&a.node))
                        .unwrap_or(Type::Var(TypeVar::fresh()));
                    Type::Observed(Box::new(t), Box::new(f))
                }
                "Result" => {
                    let ok = type_args
                        .first()
                        .map(|a| type_expr_to_type(&a.node))
                        .unwrap_or(Type::Var(TypeVar::fresh()));
                    let err = type_args
                        .get(1)
                        .map(|a| type_expr_to_type(&a.node))
                        .unwrap_or(Type::Var(TypeVar::fresh()));
                    Type::Result(Box::new(ok), Box::new(err))
                }
                other => Type::Con(other.to_string()),
            };
            if *nullable {
                Type::Optional(Box::new(base))
            } else {
                base
            }
        }
        TypeExpr::Cap { port_name, qualifier } => Type::Cap(port_name.clone(), qualifier.clone()),
    }
}

/// Infer the type of an expression, updating the substitution.
pub fn infer_expr(
    expr: &Expr,
    env: &mut InferEnv,
    subst: &mut Substitution,
) -> Result<Type, TypeError> {
    match expr {
        Expr::Literal(lit) => Ok(match lit {
            Literal::String(_) => Type::Con("String".to_string()),
            Literal::Int(_) => Type::Con("Int".to_string()),
            Literal::Float(_) => Type::Con("Float".to_string()),
            Literal::Bool(_) => Type::Con("Bool".to_string()),
            Literal::Bytes(_) => Type::Con("Bytes".to_string()),
        }),
        Expr::None => Ok(Type::Optional(Box::new(Type::Var(TypeVar::fresh())))),
        Expr::Ident(name) => env
            .lookup(name)
            .cloned()
            .ok_or_else(|| TypeError {
                message: format!("undefined variable '{name}'"),
                span: None,
            }),
        Expr::Resolve { port, .. } => Ok(Type::Cap(port.clone(), None)),
        Expr::MethodCall {
            receiver,
            method,
            args,
        } => {
            let recv_ty = infer_expr(&receiver.node, env, subst)?;
            let recv_ty = apply(subst, &recv_ty);

            // Infer arg types
            let mut arg_types = Vec::new();
            for arg in args {
                arg_types.push(infer_expr(&arg.node, env, subst)?);
            }

            match &recv_ty {
                Type::Cap(port_name, _) => {
                    // Static serialization boundary check: capability arguments
                    // with @consume or @borrow cannot cross service boundaries.
                    // Any service method call creates an interaction that may be
                    // dispatched to a remote service, so warn at type-check time.
                    for arg_ty in &arg_types {
                        let resolved = apply(subst, arg_ty);
                        if let Type::Cap(arg_port, Some(qualifier)) = &resolved {
                            match qualifier {
                                crate::ast::AuthorityQualifier::Consume
                                | crate::ast::AuthorityQualifier::Borrow => {
                                    env.warnings.push(format!(
                                        "passing @{} capability (cap {}) as argument to service method \
                                         '{}.{}': only @delegate capabilities can cross service boundaries",
                                        match qualifier {
                                            crate::ast::AuthorityQualifier::Consume => "consume",
                                            crate::ast::AuthorityQualifier::Borrow => "borrow",
                                            _ => unreachable!(),
                                        },
                                        arg_port,
                                        port_name,
                                        method,
                                    ));
                                }
                                _ => {}
                            }
                        }
                    }
                    // Capability method: return Interaction<T>
                    let ret_ty = env
                        .port_method_return(port_name, method)
                        .cloned()
                        .unwrap_or(Type::Var(TypeVar::fresh()));
                    Ok(Type::Interaction(Box::new(ret_ty)))
                }
                Type::Con(name) if name == "String" => match method.as_str() {
                    "len" | "index_of" => Ok(Type::Con("Int".to_string())),
                    "contains" | "starts_with" | "ends_with" => Ok(Type::Con("Bool".to_string())),
                    "trim" | "to_upper" | "to_lower" | "replace" | "substring" => {
                        Ok(Type::Con("String".to_string()))
                    }
                    "split" | "chars" => {
                        Ok(Type::List(Box::new(Type::Con("String".to_string()))))
                    }
                    "to_bytes" => Ok(Type::Con("Bytes".to_string())),
                    _ => Ok(Type::Var(TypeVar::fresh())),
                },
                Type::Con(name) if name == "Bytes" => match method.as_str() {
                    "len" | "at" | "to_int" => Ok(Type::Con("Int".to_string())),
                    "slice" | "concat" => Ok(Type::Con("Bytes".to_string())),
                    "hex" | "to_string" => Ok(Type::Con("String".to_string())),
                    "to_list" => Ok(Type::List(Box::new(Type::Con("Int".to_string())))),
                    _ => Ok(Type::Var(TypeVar::fresh())),
                },
                Type::Con(name) if name == "Int" => match method.as_str() {
                    "to_bytes" => Ok(Type::Con("Bytes".to_string())),
                    "to_float" => Ok(Type::Con("Float".to_string())),
                    "to_string" => Ok(Type::Con("String".to_string())),
                    _ => Ok(Type::Var(TypeVar::fresh())),
                },
                Type::Con(name) if name == "Float" => match method.as_str() {
                    "floor" | "ceil" | "round" | "abs" | "sqrt" => {
                        Ok(Type::Con("Float".to_string()))
                    }
                    "to_int" => Ok(Type::Con("Int".to_string())),
                    "to_string" => Ok(Type::Con("String".to_string())),
                    _ => Ok(Type::Var(TypeVar::fresh())),
                },
                Type::Result(ok, err) => match method.as_str() {
                    "is_ok" | "is_err" => Ok(Type::Con("Bool".to_string())),
                    "unwrap" | "unwrap_or" => Ok(*ok.clone()),
                    "unwrap_err" => Ok(*err.clone()),
                    _ => Ok(Type::Var(TypeVar::fresh())),
                },
                Type::List(inner) => match method.as_str() {
                    "len" => Ok(Type::Con("Int".to_string())),
                    "push" | "filter" | "reverse" | "concat" => Ok(recv_ty.clone()),
                    "map" => Ok(Type::List(Box::new(Type::Var(TypeVar::fresh())))),
                    "fold" => Ok(Type::Var(TypeVar::fresh())),
                    "get" | "first" | "last" => Ok(Type::Optional(inner.clone())),
                    "contains" => Ok(Type::Con("Bool".to_string())),
                    "join" => Ok(Type::Con("String".to_string())),
                    "to_bytes" => Ok(Type::Con("Bytes".to_string())),
                    _ => Ok(Type::Var(TypeVar::fresh())),
                },
                Type::Map(_, v) => match method.as_str() {
                    "keys" => Ok(Type::List(Box::new(Type::Con("String".to_string())))),
                    "values" => Ok(Type::List(v.clone())),
                    "entries" => Ok(Type::List(Box::new(Type::Var(TypeVar::fresh())))),
                    "get" => Ok(Type::Optional(v.clone())),
                    "contains_key" => Ok(Type::Con("Bool".to_string())),
                    "remove" | "insert" => Ok(recv_ty.clone()),
                    "len" => Ok(Type::Con("Int".to_string())),
                    _ => Ok(Type::Var(TypeVar::fresh())),
                },
                _ => Ok(Type::Var(TypeVar::fresh())),
            }
        }
        Expr::Await { expr } => {
            let inner_ty = infer_expr(&expr.node, env, subst)?;
            let inner_ty = apply(subst, &inner_ty);
            match &inner_ty {
                Type::Interaction(result_ty) => Ok(*result_ty.clone()),
                // await on non-interaction is identity
                other => Ok(other.clone()),
            }
        }
        Expr::FieldAccess { receiver, field } => {
            // Check for module-qualified name resolution: `alias.Symbol`
            if let Expr::Ident(ref name) = receiver.node {
                if let Some(exports) = env.module_exports.get(name) {
                    return exports.get(field).cloned().ok_or_else(|| TypeError {
                        message: format!("module '{}' has no exported member '{}'", name, field),
                        span: None,
                    });
                }
            }
            let recv_ty = infer_expr(&receiver.node, env, subst)?;
            let recv_ty = apply(subst, &recv_ty);
            match &recv_ty {
                Type::Struct(_, fields) => {
                    fields
                        .iter()
                        .find(|(n, _)| n == field)
                        .map(|(_, t)| t.clone())
                        .ok_or_else(|| TypeError {
                            message: format!("no field '{field}' on struct"),
                            span: None,
                        })
                }
                Type::Observed(value_ty, frontier_ty) => {
                    match field.as_str() {
                        "value" => Ok(*value_ty.clone()),
                        "frontier" => Ok(*frontier_ty.clone()),
                        _ => Err(TypeError {
                            message: format!("no field '{field}' on Observed"),
                            span: None,
                        }),
                    }
                }
                _ => Ok(Type::Var(TypeVar::fresh())),
            }
        }
        Expr::FnCall { func, args } => {
            if let Expr::Ident(name) = &func.node {
                // Built-in Result constructors
                if name == "ok" {
                    let inner = if let Some(arg) = args.first() {
                        infer_expr(&arg.node, env, subst)?
                    } else {
                        Type::Con("Unit".to_string())
                    };
                    return Ok(Type::Result(Box::new(inner), Box::new(Type::Var(TypeVar::fresh()))));
                }
                if name == "err" {
                    let inner = if let Some(arg) = args.first() {
                        infer_expr(&arg.node, env, subst)?
                    } else {
                        Type::Con("Unit".to_string())
                    };
                    return Ok(Type::Result(Box::new(Type::Var(TypeVar::fresh())), Box::new(inner)));
                }

                if let Some((params, ret)) = env.functions.get(name).cloned() {
                    // Check arity
                    if args.len() != params.len() {
                        return Err(TypeError {
                            message: format!(
                                "function '{}' expects {} args, got {}",
                                name,
                                params.len(),
                                args.len()
                            ),
                            span: None,
                        });
                    }
                    // Unify arg types with param types
                    for (arg, param_ty) in args.iter().zip(params.iter()) {
                        let arg_ty = infer_expr(&arg.node, env, subst)?;
                        unify(&arg_ty, param_ty, subst)?;
                    }
                    return Ok(ret);
                }
                // Port names cannot be constructed directly — use 'resolve'
                if env.ports.contains_key(name) {
                    return Err(TypeError {
                        message: format!(
                            "capability type '{}' cannot be constructed directly; use 'resolve {}[\"...\"]'",
                            name, name
                        ),
                        span: None,
                    });
                }
            }
            // Try inferring the function expression type
            let func_ty = infer_expr(&func.node, env, subst)?;
            let func_ty = apply(subst, &func_ty);
            match func_ty {
                Type::Fun(param_types, ret) => {
                    if args.len() != param_types.len() {
                        return Err(TypeError {
                            message: format!(
                                "closure expects {} args, got {}",
                                param_types.len(),
                                args.len()
                            ),
                            span: None,
                        });
                    }
                    for (arg, param_ty) in args.iter().zip(param_types.iter()) {
                        let arg_ty = infer_expr(&arg.node, env, subst)?;
                        unify(&arg_ty, param_ty, subst)?;
                    }
                    Ok(*ret)
                }
                _ => Ok(Type::Var(TypeVar::fresh())),
            }
        }
        Expr::Block(stmts) => {
            let mut last = Type::Con("Unit".to_string());
            for stmt in stmts {
                last = infer_stmt(&stmt.node, env, subst)?;
            }
            Ok(last)
        }
        Expr::Binary { op, left, right } => {
            let l = infer_expr(&left.node, env, subst)?;
            let r = infer_expr(&right.node, env, subst)?;
            match op {
                BinaryOp::Add => {
                    // String + String = String, Int + Int = Int, etc.
                    let l = apply(subst, &l);
                    match &l {
                        Type::Con(name) if name == "String" => Ok(Type::Con("String".to_string())),
                        _ => {
                            unify(&l, &r, subst)?;
                            Ok(apply(subst, &l))
                        }
                    }
                }
                BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
                    unify(&l, &r, subst)?;
                    Ok(apply(subst, &l))
                }
                BinaryOp::Eq | BinaryOp::Neq | BinaryOp::Lt | BinaryOp::Gt
                | BinaryOp::LtEq | BinaryOp::GtEq => {
                    Ok(Type::Con("Bool".to_string()))
                }
                BinaryOp::And | BinaryOp::Or => Ok(Type::Con("Bool".to_string())),
                BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor
                | BinaryOp::Shl | BinaryOp::Shr => {
                    unify(&l, &Type::Con("Int".to_string()), subst)?;
                    unify(&r, &Type::Con("Int".to_string()), subst)?;
                    Ok(Type::Con("Int".to_string()))
                }
            }
        }
        Expr::Unary { op, operand } => {
            let inner = infer_expr(&operand.node, env, subst)?;
            match op {
                UnaryOp::Neg => Ok(inner),
                UnaryOp::Not => Ok(Type::Con("Bool".to_string())),
                UnaryOp::BitNot => {
                    unify(&inner, &Type::Con("Int".to_string()), subst)?;
                    Ok(Type::Con("Int".to_string()))
                }
            }
        }
        Expr::StructLiteral { name, fields } => {
            let mut field_types = Vec::new();
            for (fname, fexpr) in fields {
                let ty = infer_expr(&fexpr.node, env, subst)?;
                field_types.push((fname.clone(), ty));
            }
            Ok(Type::Struct(name.clone(), field_types))
        }
        Expr::ConcurrentAwait { exprs } => {
            let mut elem_types = Vec::new();
            for expr in exprs {
                let inner_ty = infer_expr(&expr.node, env, subst)?;
                let inner_ty = apply(subst, &inner_ty);
                let resolved = match &inner_ty {
                    Type::Interaction(result_ty) => *result_ty.clone(),
                    other => other.clone(),
                };
                elem_types.push(resolved);
            }
            // Return List type — use first element's type as representative
            let elem_ty = elem_types
                .into_iter()
                .next()
                .unwrap_or(Type::Var(TypeVar::fresh()));
            Ok(Type::List(Box::new(elem_ty)))
        }
        Expr::ListLiteral { elements } => {
            if elements.is_empty() {
                Ok(Type::List(Box::new(Type::Var(TypeVar::fresh()))))
            } else {
                let first_ty = infer_expr(&elements[0].node, env, subst)?;
                for elem in &elements[1..] {
                    let elem_ty = infer_expr(&elem.node, env, subst)?;
                    unify(&first_ty, &elem_ty, subst)?;
                }
                Ok(Type::List(Box::new(apply(subst, &first_ty))))
            }
        }
        Expr::MapLiteral { entries } => {
            if entries.is_empty() {
                Ok(Type::Map(
                    Box::new(Type::Var(TypeVar::fresh())),
                    Box::new(Type::Var(TypeVar::fresh())),
                ))
            } else {
                let (ref first_key, ref first_val) = entries[0];
                let key_ty = infer_expr(&first_key.node, env, subst)?;
                let val_ty = infer_expr(&first_val.node, env, subst)?;
                for (k, v) in &entries[1..] {
                    let kt = infer_expr(&k.node, env, subst)?;
                    let vt = infer_expr(&v.node, env, subst)?;
                    unify(&key_ty, &kt, subst)?;
                    unify(&val_ty, &vt, subst)?;
                }
                Ok(Type::Map(
                    Box::new(apply(subst, &key_ty)),
                    Box::new(apply(subst, &val_ty)),
                ))
            }
        }
        Expr::Closure { params, body } => {
            env.push_scope();
            let param_types: Vec<Type> = params.iter().map(|p| {
                let ty = match &p.ty.node {
                    TypeExpr::Named { name, type_args, .. } if name == "Any" && type_args.is_empty() => {
                        // Unannotated closure param: use fresh type variable for inference
                        Type::Var(TypeVar::fresh())
                    }
                    _ => type_expr_to_type(&p.ty.node),
                };
                env.define(p.name.clone(), ty.clone());
                ty
            }).collect();
            let mut ret_ty = Type::Con("Unit".to_string());
            for stmt in body {
                ret_ty = infer_stmt(&stmt.node, env, subst)?;
            }
            env.pop_scope();
            Ok(Type::Fun(param_types, Box::new(apply(subst, &ret_ty))))
        }
        Expr::MacroCall { .. } => {
            // Macros should be expanded before type checking
            Ok(Type::Var(TypeVar::fresh()))
        }
        Expr::DynamicImport { path } => {
            // If the path is a string literal and module_root is set,
            // attempt to parse the imported file and register its exported fns.
            if let Expr::Literal(Literal::String(ref import_path)) = path.node {
                if let Some(ref root) = env.module_root {
                    let file_path = root.join(import_path);
                    if let Ok(source) = std::fs::read_to_string(&file_path) {
                        if let Ok(tokens) = crate::lexer::tokenize(&source) {
                            let (program, _) = crate::parser::parse(&source, &tokens);
                            for item in &program.items {
                                if let Item::FnDecl(f) = &item.node {
                                    // Functions are exported by default (like Rust pub fn in lib)
                                    env.register_fn(f);
                                }
                            }
                        }
                    }
                }
            }
            // Runtime returns Unit (import merges fns into scope)
            Ok(Type::Con("Unit".to_string()))
        }
        Expr::Index { receiver, index } => {
            let recv_ty = infer_expr(&receiver.node, env, subst)?;
            let recv_ty = apply(subst, &recv_ty);
            let _idx_ty = infer_expr(&index.node, env, subst)?;
            match &recv_ty {
                Type::List(inner) => Ok(*inner.clone()),
                Type::Map(_, v) => Ok(*v.clone()),
                Type::Con(name) if name == "String" => Ok(Type::Con("String".to_string())),
                _ => Ok(Type::Var(TypeVar::fresh())),
            }
        }
        Expr::Match { expr, arms } => {
            let val_ty = infer_expr(&expr.node, env, subst)?;
            let result_ty = Type::Var(TypeVar::fresh());
            for arm in arms {
                env.push_scope();
                infer_pattern(&arm.pattern.node, &val_ty, env, subst);
                if let Some(ref guard) = arm.guard {
                    let _ = infer_expr(&guard.node, env, subst)?;
                }
                let mut arm_ty = Type::Con("Unit".to_string());
                for stmt in &arm.body {
                    arm_ty = infer_stmt(&stmt.node, env, subst)?;
                }
                env.pop_scope();
                unify(&result_ty, &arm_ty, subst)?;
            }
            Ok(apply(subst, &result_ty))
        }
        Expr::Try { expr } => {
            let inner_ty = infer_expr(&expr.node, env, subst)?;
            let inner_ty = apply(subst, &inner_ty);
            match inner_ty {
                Type::Result(ok, _) => Ok(*ok),
                _ => Ok(Type::Var(TypeVar::fresh())),
            }
        }
        Expr::Select { timeout_ms, arms, else_body } => {
            if let Some(ref te) = timeout_ms {
                let t = infer_expr(&te.node, env, subst)?;
                unify(&t, &Type::Con("Int".to_string()), subst)?;
            }
            let result_ty = Type::Var(TypeVar::fresh());
            for arm in arms {
                let _ = infer_expr(&arm.expr.node, env, subst)?;
                env.push_scope();
                // Binding gets Any type — arm expr returns different types
                env.define(arm.binding.clone(), Type::Var(TypeVar::fresh()));
                let mut arm_ty = Type::Con("Unit".to_string());
                for stmt in &arm.body {
                    arm_ty = infer_stmt(&stmt.node, env, subst)?;
                }
                env.pop_scope();
                unify(&result_ty, &arm_ty, subst)?;
            }
            if let Some(ref eb) = else_body {
                env.push_scope();
                let mut else_ty = Type::Con("Unit".to_string());
                for stmt in eb {
                    else_ty = infer_stmt(&stmt.node, env, subst)?;
                }
                env.pop_scope();
                unify(&result_ty, &else_ty, subst)?;
            }
            Ok(apply(subst, &result_ty))
        }
    }
}

/// Define pattern bindings in the type environment.
fn infer_pattern(
    pattern: &Pattern,
    val_ty: &Type,
    env: &mut InferEnv,
    subst: &mut Substitution,
) {
    match pattern {
        Pattern::Ident(name) => {
            env.define(name.clone(), apply(subst, val_ty));
        }
        Pattern::Wildcard | Pattern::Literal(_) => {}
        Pattern::Struct { fields, .. } => {
            // Define each field binding with a fresh type var
            for (field_name, inner) in fields {
                let field_ty = if let Type::Struct(_, sfields) = val_ty {
                    sfields
                        .iter()
                        .find(|(n, _)| n == field_name)
                        .map(|(_, t)| t.clone())
                        .unwrap_or(Type::Var(TypeVar::fresh()))
                } else {
                    Type::Var(TypeVar::fresh())
                };
                if let Some(inner_pat) = inner {
                    infer_pattern(&inner_pat.node, &field_ty, env, subst);
                } else {
                    env.define(field_name.clone(), apply(subst, &field_ty));
                }
            }
        }
        Pattern::Some(inner) => {
            // Some(x) unwraps Optional<T> to T
            let inner_ty = match val_ty {
                Type::Optional(t) => *t.clone(),
                _ => val_ty.clone(),
            };
            infer_pattern(&inner.node, &inner_ty, env, subst);
        }
        Pattern::None => {
            // None pattern matches Optional<T>, no bindings
        }
        Pattern::Ok(inner) => {
            let ok_ty = match val_ty {
                Type::Result(ok, _) => *ok.clone(),
                _ => Type::Var(TypeVar::fresh()),
            };
            infer_pattern(&inner.node, &ok_ty, env, subst);
        }
        Pattern::Err(inner) => {
            let err_ty = match val_ty {
                Type::Result(_, err) => *err.clone(),
                _ => Type::Var(TypeVar::fresh()),
            };
            infer_pattern(&inner.node, &err_ty, env, subst);
        }
        Pattern::List { elements, rest } => {
            let elem_ty = match val_ty {
                Type::List(inner) => *inner.clone(),
                _ => Type::Var(TypeVar::fresh()),
            };
            for elem_pat in elements {
                infer_pattern(&elem_pat.node, &elem_ty, env, subst);
            }
            if let Some(rest_pat) = rest {
                infer_pattern(&rest_pat.node, val_ty, env, subst);
            }
        }
    }
}

/// Infer the type produced by a statement.
pub fn infer_stmt(
    stmt: &Stmt,
    env: &mut InferEnv,
    subst: &mut Substitution,
) -> Result<Type, TypeError> {
    match stmt {
        Stmt::Let { name, ty, value, .. } => {
            let inferred = infer_expr(&value.node, env, subst)?;
            let var_ty = if let Some(type_expr) = ty {
                let declared = type_expr_to_type(&type_expr.node);
                unify(&inferred, &declared, subst)?;
                declared
            } else {
                inferred
            };
            env.define(name.clone(), apply(subst, &var_ty));
            Ok(Type::Con("Unit".to_string()))
        }
        Stmt::Return(Some(expr)) => infer_expr(&expr.node, env, subst),
        Stmt::Return(None) => Ok(Type::Con("Unit".to_string())),
        Stmt::Expr(expr) => infer_expr(&expr.node, env, subst),
        Stmt::If {
            condition,
            then_block,
            else_block,
        } => {
            let _cond = infer_expr(&condition.node, env, subst)?;
            let mut then_ty = Type::Con("Unit".to_string());
            for stmt in then_block {
                then_ty = infer_stmt(&stmt.node, env, subst)?;
            }
            if let Some(else_stmts) = else_block {
                let mut else_ty = Type::Con("Unit".to_string());
                for stmt in else_stmts {
                    else_ty = infer_stmt(&stmt.node, env, subst)?;
                }
                // Unify branches
                unify(&then_ty, &else_ty, subst)?;
            }
            Ok(apply(subst, &then_ty))
        }
        Stmt::Match { expr, arms } => {
            let val_ty = infer_expr(&expr.node, env, subst)?;
            let result_ty = Type::Var(TypeVar::fresh());
            for arm in arms {
                env.push_scope();
                infer_pattern(&arm.pattern.node, &val_ty, env, subst);
                if let Some(ref guard) = arm.guard {
                    let _ = infer_expr(&guard.node, env, subst)?;
                }
                let mut arm_ty = Type::Con("Unit".to_string());
                for stmt in &arm.body {
                    arm_ty = infer_stmt(&stmt.node, env, subst)?;
                }
                env.pop_scope();
                unify(&result_ty, &arm_ty, subst)?;
            }
            Ok(apply(subst, &result_ty))
        }
        Stmt::For { variable, iterable, body } => {
            let iter_ty = infer_expr(&iterable.node, env, subst)?;
            let iter_ty = apply(subst, &iter_ty);
            let elem_ty = match &iter_ty {
                Type::List(inner) => *inner.clone(),
                _ => Type::Var(TypeVar::fresh()),
            };
            env.push_scope();
            env.define(variable.clone(), elem_ty);
            for stmt in body {
                infer_stmt(&stmt.node, env, subst)?;
            }
            env.pop_scope();
            Ok(Type::Con("Unit".to_string()))
        }
        Stmt::While { condition, body } => {
            let _cond = infer_expr(&condition.node, env, subst)?;
            env.push_scope();
            for stmt in body {
                infer_stmt(&stmt.node, env, subst)?;
            }
            env.pop_scope();
            Ok(Type::Con("Unit".to_string()))
        }
        Stmt::Break | Stmt::Continue => {
            Ok(Type::Con("Unit".to_string()))
        }
        Stmt::Assign { name, value } => {
            let val_ty = infer_expr(&value.node, env, subst)?;
            if let Some(existing) = env.lookup(name).cloned() {
                unify(&val_ty, &existing, subst)?;
            }
            Ok(Type::Con("Unit".to_string()))
        }
        Stmt::Emit { fields, .. } => {
            for (_name, expr) in fields {
                infer_expr(&expr.node, env, subst)?;
            }
            Ok(Type::Con("Unit".to_string()))
        }
    }
}

/// Run type inference on a program with a module root for dynamic import resolution.
/// Result of type inference: errors (fatal) and warnings (non-fatal diagnostics).
#[derive(Debug)]
pub struct InferResult {
    pub errors: Vec<TypeError>,
    pub warnings: Vec<String>,
}

pub fn infer_program_with_root(program: &Program, module_root: PathBuf) -> InferResult {
    let mut errors = Vec::new();
    let mut env = InferEnv::new();
    env.module_root = Some(module_root);
    let mut subst = Substitution::new();
    infer_program_inner(program, &mut env, &mut subst, &mut errors);
    InferResult {
        errors,
        warnings: env.warnings,
    }
}

/// Run type inference on a program, returning any errors found.
/// This is designed to augment (not replace) the existing structural checks.
pub fn infer_program(program: &Program) -> InferResult {
    let mut errors = Vec::new();
    let mut env = InferEnv::new();
    let mut subst = Substitution::new();
    infer_program_inner(program, &mut env, &mut subst, &mut errors);
    InferResult {
        errors,
        warnings: env.warnings,
    }
}

/// Shared inner body for program inference.
fn infer_program_inner(
    program: &Program,
    env: &mut InferEnv,
    subst: &mut Substitution,
    errors: &mut Vec<TypeError>,
) {
    // Pass 1: Register declarations
    for item in &program.items {
        match &item.node {
            Item::Port(port) => env.register_port(port),
            Item::FnDecl(f) => env.register_fn(f),
            _ => {}
        }
    }

    // Pass 1b: Process aliased imports for qualified name resolution
    if let Some(ref root) = env.module_root.clone() {
        for import in &program.imports {
            let imp = &import.node;
            if let Some(ref alias) = imp.alias {
                let mod_path_str = imp.path.join(".");
                let mut loader = crate::module::ModuleLoader::new(root.clone());
                // Resolve "kv.storage" -> "kv/storage.bl" relative to root
                let relative: std::path::PathBuf = imp.path.iter().collect::<std::path::PathBuf>().with_extension("bl");
                let file_path = root.join(&relative);
                if file_path.exists() {
                    if let Ok(_mod_name) = loader.load_file(&file_path) {
                        let exports = extract_module_exports(&loader, &mod_path_str);
                        if !exports.is_empty() {
                            env.register_module_exports(alias, exports);
                        }
                    }
                }
            }
        }
    }

    // Pass 2: Infer entry bodies
    for item in &program.items {
        match &item.node {
            Item::Entry(entry) => {
                env.push_scope();
                for param in &entry.params {
                    let ty = type_expr_to_type(&param.ty.node);
                    env.define(param.name.clone(), ty);
                }
                for stmt in &entry.body {
                    if let Err(e) = infer_stmt(&stmt.node, env, subst) {
                        errors.push(e);
                    }
                }
                env.pop_scope();
            }
            Item::FnDecl(f) => {
                env.push_scope();
                for param in &f.params {
                    let ty = type_expr_to_type(&param.ty.node);
                    env.define(param.name.clone(), ty);
                }
                let mut body_ty = Type::Con("Unit".to_string());
                for stmt in &f.body {
                    match infer_stmt(&stmt.node, env, subst) {
                        Ok(ty) => body_ty = ty,
                        Err(e) => errors.push(e),
                    }
                }
                // Unify body return type with declared return type
                if let Some(ref rt) = f.return_type {
                    let declared = type_expr_to_type(&rt.node);
                    let declared_name = match &declared {
                        Type::Con(n) => n.as_str(),
                        _ => "",
                    };
                    // Skip unification for "Any" or "Unit" declared return
                    if declared_name != "Any" && declared_name != "Unit" {
                        if let Err(e) = unify(&body_ty, &declared, subst) {
                            errors.push(TypeError {
                                message: format!(
                                    "function '{}' declared return type {} but body returns {}: {}",
                                    f.name,
                                    apply(subst, &declared),
                                    apply(subst, &body_ty),
                                    e.message
                                ),
                                span: None,
                            });
                        }
                    }
                }
                env.pop_scope();
            }
            Item::Service(service) => {
                // Collect component names for scope binding
                let component_names: Vec<String> = service.items.iter().filter_map(|si| {
                    if let ServiceItem::Component(comp) = &si.node {
                        Some(comp.name.clone())
                    } else {
                        None
                    }
                }).collect();

                // Infer command and query bodies within services
                for svc_item in &service.items {
                    match &svc_item.node {
                        ServiceItem::Command(cmd) => {
                            if let CommandBody::Block(stmts) = &cmd.body {
                                env.push_scope();
                                // Bind component names as opaque types
                                for comp in &component_names {
                                    env.define(comp.clone(), Type::Var(TypeVar::fresh()));
                                }
                                for param in &cmd.params {
                                    let ty = type_expr_to_type(&param.ty.node);
                                    env.define(param.name.clone(), ty);
                                }
                                for stmt in stmts {
                                    if let Err(e) = infer_stmt(&stmt.node, env, subst) {
                                        errors.push(e);
                                    }
                                }
                                env.pop_scope();
                            }
                            // ViaSettle: infer the via_expr
                            if let CommandBody::ViaSettle { via_expr, .. } = &cmd.body {
                                env.push_scope();
                                for comp in &component_names {
                                    env.define(comp.clone(), Type::Var(TypeVar::fresh()));
                                }
                                for param in &cmd.params {
                                    let ty = type_expr_to_type(&param.ty.node);
                                    env.define(param.name.clone(), ty);
                                }
                                if let Err(e) = infer_expr(&via_expr.node, env, subst) {
                                    errors.push(e);
                                }
                                env.pop_scope();
                            }
                        }
                        ServiceItem::Query(q) => {
                            if let QueryBody::Block(stmts) = &q.body {
                                env.push_scope();
                                for comp in &component_names {
                                    env.define(comp.clone(), Type::Var(TypeVar::fresh()));
                                }
                                for param in &q.params {
                                    let ty = type_expr_to_type(&param.ty.node);
                                    env.define(param.name.clone(), ty);
                                }
                                for stmt in stmts {
                                    if let Err(e) = infer_stmt(&stmt.node, env, subst) {
                                        errors.push(e);
                                    }
                                }
                                env.pop_scope();
                            }
                            // ViaObserve: infer the via_expr and return_expr
                            if let QueryBody::ViaObserve { via_expr, return_expr, .. } = &q.body {
                                env.push_scope();
                                for comp in &component_names {
                                    env.define(comp.clone(), Type::Var(TypeVar::fresh()));
                                }
                                for param in &q.params {
                                    let ty = type_expr_to_type(&param.ty.node);
                                    env.define(param.name.clone(), ty);
                                }
                                if let Err(e) = infer_expr(&via_expr.node, env, subst) {
                                    errors.push(e);
                                }
                                // 'res' binding is available for return_expr in ViaObserve
                                env.define("res".to_string(), Type::Var(TypeVar::fresh()));
                                if let Err(e) = infer_expr(&return_expr.node, env, subst) {
                                    errors.push(e);
                                }
                                env.pop_scope();
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// Extract exported types from a loaded module for qualified name resolution.
/// Returns a map of symbol_name → Type for all exported declarations.
fn extract_module_exports(
    loader: &crate::module::ModuleLoader,
    module_name: &str,
) -> HashMap<String, Type> {
    let mut exports = HashMap::new();
    let Some(module) = loader.get_module(module_name) else {
        return exports;
    };
    for item in &module.program.items {
        match &item.node {
            Item::Port(p) if module.exported_ports.contains(&p.name) => {
                // Ports resolve to their capability type
                exports.insert(p.name.clone(), Type::Con(p.name.clone()));
            }
            Item::FnDecl(f) if module.exported_fns.contains(&f.name) => {
                let ret = f
                    .return_type
                    .as_ref()
                    .map(|rt| type_expr_to_type(&rt.node))
                    .unwrap_or(Type::Con("Unit".to_string()));
                exports.insert(f.name.clone(), ret);
            }
            Item::TypeDecl(t) if module.exported_types.contains(&t.name) => {
                let fields: Vec<(String, Type)> = t
                    .fields
                    .iter()
                    .map(|f| (f.name.clone(), type_expr_to_type(&f.ty.node)))
                    .collect();
                exports.insert(t.name.clone(), Type::Struct(t.name.clone(), fields));
            }
            _ => {}
        }
    }
    exports
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Spanned;

    #[test]
    fn test_unify_same_concrete() {
        let mut subst = Substitution::new();
        assert!(unify(
            &Type::Con("Int".into()),
            &Type::Con("Int".into()),
            &mut subst
        )
        .is_ok());
    }

    #[test]
    fn test_unify_different_concrete_fails() {
        let mut subst = Substitution::new();
        assert!(unify(
            &Type::Con("Int".into()),
            &Type::Con("String".into()),
            &mut subst
        )
        .is_err());
    }

    #[test]
    fn test_unify_var_binds() {
        let mut subst = Substitution::new();
        let tv = TypeVar::fresh();
        unify(
            &Type::Var(tv),
            &Type::Con("Int".into()),
            &mut subst,
        )
        .unwrap();
        assert_eq!(apply(&subst, &Type::Var(tv)), Type::Con("Int".into()));
    }

    #[test]
    fn test_unify_interaction_types() {
        let mut subst = Substitution::new();
        let tv = TypeVar::fresh();
        unify(
            &Type::Interaction(Box::new(Type::Var(tv))),
            &Type::Interaction(Box::new(Type::Con("String".into()))),
            &mut subst,
        )
        .unwrap();
        assert_eq!(apply(&subst, &Type::Var(tv)), Type::Con("String".into()));
    }

    #[test]
    fn test_occurs_check_prevents_infinite() {
        let mut subst = Substitution::new();
        let tv = TypeVar::fresh();
        let result = unify(
            &Type::Var(tv),
            &Type::Interaction(Box::new(Type::Var(tv))),
            &mut subst,
        );
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("infinite"));
    }

    #[test]
    fn test_infer_literal_types() {
        let mut env = InferEnv::new();
        let mut subst = Substitution::new();

        assert_eq!(
            infer_expr(&Expr::Literal(Literal::Int(42)), &mut env, &mut subst).unwrap(),
            Type::Con("Int".into())
        );
        assert_eq!(
            infer_expr(
                &Expr::Literal(Literal::String("hello".into())),
                &mut env,
                &mut subst
            )
            .unwrap(),
            Type::Con("String".into())
        );
        assert_eq!(
            infer_expr(&Expr::Literal(Literal::Bool(true)), &mut env, &mut subst).unwrap(),
            Type::Con("Bool".into())
        );
    }

    #[test]
    fn test_infer_await_unwraps_interaction() {
        use crate::lexer::span::{Span, Spanned};

        let mut env = InferEnv::new();
        let mut subst = Substitution::new();

        // await on Interaction<String> should give String
        // We can't easily construct this without a capability, so test
        // that await on a non-interaction is identity
        let expr = Expr::Await {
            expr: Box::new(Spanned {
                node: Expr::Literal(Literal::Int(42)),
                span: Span::new(0, 0),
            }),
        };
        let ty = infer_expr(&expr, &mut env, &mut subst).unwrap();
        assert_eq!(ty, Type::Con("Int".into()));
    }

    #[test]
    fn test_infer_program_catches_errors() {
        use crate::lexer::tokenize;
        use crate::parser::parse;

        let source = r#"
            fn add(a: Int, b: Int) -> Int {
                return a + b
            }
            entry() {
                let x = add(1, 2)
                return x
            }
        "#;
        let tokens = tokenize(source).unwrap();
        let (program, parse_errors) = parse(source, &tokens);
        assert!(parse_errors.is_empty());

        let result = infer_program(&program);
        assert!(result.errors.is_empty(), "unexpected errors: {:?}", result.errors);
    }

    #[test]
    fn test_type_expr_conversion() {
        let te = TypeExpr::Named {
            name: "String".to_string(),
            type_args: Vec::new(),
            nullable: false,
        };
        assert_eq!(type_expr_to_type(&te), Type::Con("String".to_string()));

        let te_nullable = TypeExpr::Named {
            name: "String".to_string(),
            type_args: Vec::new(),
            nullable: true,
        };
        assert_eq!(
            type_expr_to_type(&te_nullable),
            Type::Optional(Box::new(Type::Con("String".to_string())))
        );

        let te_cap = TypeExpr::Cap {
            port_name: "KV".to_string(),
            qualifier: None,
        };
        assert_eq!(type_expr_to_type(&te_cap), Type::Cap("KV".to_string(), None));
    }

    #[test]
    fn test_observed_type_expr_to_type() {
        use crate::lexer::span::{Span, Spanned};

        let te = TypeExpr::Named {
            name: "Observed".to_string(),
            type_args: vec![
                Spanned {
                    node: TypeExpr::Named {
                        name: "String".to_string(),
                        type_args: vec![],
                        nullable: false,
                    },
                    span: Span::new(0, 0),
                },
                Spanned {
                    node: TypeExpr::Named {
                        name: "Int".to_string(),
                        type_args: vec![],
                        nullable: false,
                    },
                    span: Span::new(0, 0),
                },
            ],
            nullable: false,
        };
        assert_eq!(
            type_expr_to_type(&te),
            Type::Observed(
                Box::new(Type::Con("String".to_string())),
                Box::new(Type::Con("Int".to_string()))
            )
        );
    }

    #[test]
    fn test_observed_unify_matching() {
        let mut subst = Substitution::new();
        let result = unify(
            &Type::Observed(
                Box::new(Type::Con("String".into())),
                Box::new(Type::Con("Int".into())),
            ),
            &Type::Observed(
                Box::new(Type::Con("String".into())),
                Box::new(Type::Con("Int".into())),
            ),
            &mut subst,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_observed_unify_with_vars() {
        let mut subst = Substitution::new();
        let tv1 = TypeVar::fresh();
        let tv2 = TypeVar::fresh();
        unify(
            &Type::Observed(Box::new(Type::Var(tv1)), Box::new(Type::Var(tv2))),
            &Type::Observed(
                Box::new(Type::Con("String".into())),
                Box::new(Type::Con("Int".into())),
            ),
            &mut subst,
        )
        .unwrap();
        assert_eq!(apply(&subst, &Type::Var(tv1)), Type::Con("String".into()));
        assert_eq!(apply(&subst, &Type::Var(tv2)), Type::Con("Int".into()));
    }

    #[test]
    fn test_observed_unify_mismatch() {
        let mut subst = Substitution::new();
        let result = unify(
            &Type::Observed(
                Box::new(Type::Con("String".into())),
                Box::new(Type::Con("Int".into())),
            ),
            &Type::Observed(
                Box::new(Type::Con("Int".into())),
                Box::new(Type::Con("Int".into())),
            ),
            &mut subst,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_observed_no_type_args() {
        // Observed without type args should produce fresh type vars
        let te = TypeExpr::Named {
            name: "Observed".to_string(),
            type_args: vec![],
            nullable: false,
        };
        let ty = type_expr_to_type(&te);
        match ty {
            Type::Observed(t, f) => {
                assert!(matches!(*t, Type::Var(_)));
                assert!(matches!(*f, Type::Var(_)));
            }
            other => panic!("expected Observed, got {other}"),
        }
    }

    #[test]
    fn test_struct_unify_matching_fields() {
        let mut subst = Substitution::new();
        let s1 = Type::Struct(
            "Point".to_string(),
            vec![
                ("x".to_string(), Type::Con("Int".to_string())),
                ("y".to_string(), Type::Con("Int".to_string())),
            ],
        );
        let s2 = Type::Struct(
            "Point".to_string(),
            vec![
                ("x".to_string(), Type::Con("Int".to_string())),
                ("y".to_string(), Type::Con("Int".to_string())),
            ],
        );
        assert!(unify(&s1, &s2, &mut subst).is_ok());
    }

    #[test]
    fn test_struct_unify_width_subtyping() {
        // s1 expects {x: Int}, s2 has {x: Int, y: Int} — should unify
        let mut subst = Substitution::new();
        let expected = Type::Struct(
            "Point".to_string(),
            vec![("x".to_string(), Type::Con("Int".to_string()))],
        );
        let actual = Type::Struct(
            "Point".to_string(),
            vec![
                ("x".to_string(), Type::Con("Int".to_string())),
                ("y".to_string(), Type::Con("Int".to_string())),
            ],
        );
        assert!(unify(&expected, &actual, &mut subst).is_ok());
    }

    #[test]
    fn test_struct_unify_field_type_mismatch() {
        let mut subst = Substitution::new();
        let s1 = Type::Struct(
            "Point".to_string(),
            vec![("x".to_string(), Type::Con("Int".to_string()))],
        );
        let s2 = Type::Struct(
            "Point".to_string(),
            vec![("x".to_string(), Type::Con("String".to_string()))],
        );
        let result = unify(&s1, &s2, &mut subst);
        assert!(result.is_err());
    }

    #[test]
    fn test_struct_unify_missing_field() {
        let mut subst = Substitution::new();
        let s1 = Type::Struct(
            "Point".to_string(),
            vec![
                ("x".to_string(), Type::Con("Int".to_string())),
                ("z".to_string(), Type::Con("Int".to_string())),
            ],
        );
        let s2 = Type::Struct(
            "Point".to_string(),
            vec![("x".to_string(), Type::Con("Int".to_string()))],
        );
        let result = unify(&s1, &s2, &mut subst);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("missing field 'z'"));
    }

    #[test]
    fn test_struct_unify_different_names() {
        let mut subst = Substitution::new();
        let s1 = Type::Struct(
            "Point".to_string(),
            vec![("x".to_string(), Type::Con("Int".to_string()))],
        );
        let s2 = Type::Struct(
            "Vec2".to_string(),
            vec![("x".to_string(), Type::Con("Int".to_string()))],
        );
        // Different struct names should not unify
        assert!(unify(&s1, &s2, &mut subst).is_err());
    }

    #[test]
    fn test_observed_field_access_value() {
        use crate::lexer::span::{Span, Spanned};

        let mut env = InferEnv::new();
        let mut subst = Substitution::new();

        // Define a variable with Observed(Int, Int) type
        env.define(
            "result".to_string(),
            Type::Observed(
                Box::new(Type::Con("Int".to_string())),
                Box::new(Type::Con("Int".to_string())),
            ),
        );

        let expr = Expr::FieldAccess {
            receiver: Box::new(Spanned {
                node: Expr::Ident("result".to_string()),
                span: Span::new(0, 0),
            }),
            field: "value".to_string(),
        };
        let ty = infer_expr(&expr, &mut env, &mut subst).unwrap();
        assert_eq!(ty, Type::Con("Int".into()));
    }

    #[test]
    fn test_observed_field_access_frontier() {
        use crate::lexer::span::{Span, Spanned};

        let mut env = InferEnv::new();
        let mut subst = Substitution::new();

        env.define(
            "result".to_string(),
            Type::Observed(
                Box::new(Type::Con("String".to_string())),
                Box::new(Type::Con("Int".to_string())),
            ),
        );

        let expr = Expr::FieldAccess {
            receiver: Box::new(Spanned {
                node: Expr::Ident("result".to_string()),
                span: Span::new(0, 0),
            }),
            field: "frontier".to_string(),
        };
        let ty = infer_expr(&expr, &mut env, &mut subst).unwrap();
        assert_eq!(ty, Type::Con("Int".into()));
    }

    #[test]
    fn test_observed_field_access_unknown_field_error() {
        use crate::lexer::span::{Span, Spanned};

        let mut env = InferEnv::new();
        let mut subst = Substitution::new();

        env.define(
            "result".to_string(),
            Type::Observed(
                Box::new(Type::Con("Int".to_string())),
                Box::new(Type::Con("Int".to_string())),
            ),
        );

        let expr = Expr::FieldAccess {
            receiver: Box::new(Spanned {
                node: Expr::Ident("result".to_string()),
                span: Span::new(0, 0),
            }),
            field: "unknown".to_string(),
        };
        let result = infer_expr(&expr, &mut env, &mut subst);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("no field 'unknown' on Observed"));
    }

    #[test]
    fn test_return_type_mismatch_error() {
        use crate::lexer::tokenize;
        use crate::parser::parse;

        let source = r#"
            fn foo() -> Int {
                return "hello"
            }
        "#;
        let tokens = tokenize(source).unwrap();
        let (program, errors) = parse(source, &tokens);
        assert!(errors.is_empty());
        let infer_result = infer_program(&program);
        assert!(
            infer_result.errors.iter().any(|e| e.message.contains("foo") && e.message.contains("return type")),
            "expected return type mismatch, got: {:?}",
            infer_result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_return_type_match_ok() {
        use crate::lexer::tokenize;
        use crate::parser::parse;

        let source = r#"
            fn bar() -> String {
                return "ok"
            }
        "#;
        let tokens = tokenize(source).unwrap();
        let (program, errors) = parse(source, &tokens);
        assert!(errors.is_empty());
        let infer_result = infer_program(&program);
        assert!(
            infer_result.errors.is_empty(),
            "expected no errors, got: {:?}",
            infer_result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_no_return_type_declared_ok() {
        use crate::lexer::tokenize;
        use crate::parser::parse;

        let source = r#"
            fn baz() {
                42
            }
        "#;
        let tokens = tokenize(source).unwrap();
        let (program, errors) = parse(source, &tokens);
        assert!(errors.is_empty());
        let infer_result = infer_program(&program);
        assert!(
            infer_result.errors.is_empty(),
            "expected no errors when no return type declared, got: {:?}",
            infer_result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // === Gap 3: Service body type inference ===

    #[test]
    fn test_service_command_body_type_error_caught() {
        use crate::lexer::tokenize;
        use crate::parser::parse;

        let source = r#"
            fn add(a: Int, b: Int) -> Int {
                return a + b
            }
            port KV {
                put(key: String, value: String) -> String [command]
            }
            substrate ReplicatedLog<T> {
                op append(entry: T) -> String
                emits { quorum_committed }
            }
            service MyKV provides KV {
                publish as "kv/main"
                component log = spawn ReplicatedLog("log/main")
                command put(key: String, value: String) -> String {
                    let x = add("not_int", "also_not_int")
                    return x
                }
            }
        "#;
        let tokens = tokenize(source).unwrap();
        let (mut program, errors) = parse(source, &tokens);
        assert!(errors.is_empty());
        crate::macro_expand::expand_program(&mut program);
        let infer_result = infer_program(&program);
        assert!(
            infer_result.errors.iter().any(|e| e.message.contains("type mismatch")),
            "expected type mismatch in service command body, got: {:?}",
            infer_result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_service_query_body_type_error_caught() {
        use crate::lexer::tokenize;
        use crate::parser::parse;

        let source = r#"
            fn add(a: Int, b: Int) -> Int {
                return a + b
            }
            port Reader {
                get(key: String) -> String? [query]
            }
            service MyReader provides Reader {
                publish as "reader/main"
                query get(key: String) -> String? {
                    let x = add("wrong", "types")
                    return x
                }
            }
        "#;
        let tokens = tokenize(source).unwrap();
        let (mut program, errors) = parse(source, &tokens);
        assert!(errors.is_empty());
        crate::macro_expand::expand_program(&mut program);
        let infer_result = infer_program(&program);
        assert!(
            infer_result.errors.iter().any(|e| e.message.contains("type mismatch")),
            "expected type mismatch in service query body, got: {:?}",
            infer_result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // === Gap 15: Cap authority subtyping ===

    #[test]
    fn test_cap_authority_same_port_compatible() {
        let mut subst = Substitution::new();
        // @borrow P assigned to @delegate P → ok (widening)
        let result = unify(
            &Type::Cap("MyPort".into(), Some(AuthorityQualifier::Borrow)),
            &Type::Cap("MyPort".into(), Some(AuthorityQualifier::Delegate)),
            &mut subst,
        );
        assert!(result.is_ok(), "expected @borrow → @delegate to be compatible");
    }

    #[test]
    fn test_cap_authority_delegate_to_consume_error() {
        let mut subst = Substitution::new();
        // @delegate P assigned to @consume P → error (narrowing)
        let result = unify(
            &Type::Cap("MyPort".into(), Some(AuthorityQualifier::Delegate)),
            &Type::Cap("MyPort".into(), Some(AuthorityQualifier::Consume)),
            &mut subst,
        );
        assert!(result.is_err(), "expected @delegate → @consume to fail");
    }

    #[test]
    fn test_cap_authority_same_qualifier_ok() {
        let mut subst = Substitution::new();
        let result = unify(
            &Type::Cap("P".into(), Some(AuthorityQualifier::Consume)),
            &Type::Cap("P".into(), Some(AuthorityQualifier::Consume)),
            &mut subst,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_cap_authority_none_to_none_ok() {
        let mut subst = Substitution::new();
        let result = unify(
            &Type::Cap("P".into(), None),
            &Type::Cap("P".into(), None),
            &mut subst,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_cap_different_ports_error() {
        let mut subst = Substitution::new();
        let result = unify(
            &Type::Cap("A".into(), None),
            &Type::Cap("B".into(), None),
            &mut subst,
        );
        assert!(result.is_err());
    }

    // === Gap 3: EventKey type alias ===

    #[test]
    fn test_eventkey_maps_to_string() {
        let te = TypeExpr::Named {
            name: "EventKey".to_string(),
            type_args: vec![],
            nullable: false,
        };
        assert_eq!(type_expr_to_type(&te), Type::Con("String".to_string()));
    }

    #[test]
    fn test_eventkey_unifies_with_string() {
        let mut subst = Substitution::new();
        let eventkey_ty = type_expr_to_type(&TypeExpr::Named {
            name: "EventKey".to_string(),
            type_args: vec![],
            nullable: false,
        });
        let string_ty = Type::Con("String".to_string());
        assert!(unify(&eventkey_ty, &string_ty, &mut subst).is_ok());
    }

    // === Gap 7: cap Any wildcard subtyping ===

    #[test]
    fn test_cap_any_unifies_with_named_cap() {
        let mut subst = Substitution::new();
        let result = unify(
            &Type::Cap("Any".into(), None),
            &Type::Cap("KV".into(), None),
            &mut subst,
        );
        assert!(result.is_ok(), "cap Any should unify with cap KV");
    }

    #[test]
    fn test_named_cap_unifies_with_cap_any() {
        let mut subst = Substitution::new();
        let result = unify(
            &Type::Cap("KV".into(), None),
            &Type::Cap("Any".into(), None),
            &mut subst,
        );
        assert!(result.is_ok(), "cap KV should unify with cap Any");
    }

    #[test]
    fn test_cap_any_unifies_with_cap_any() {
        let mut subst = Substitution::new();
        let result = unify(
            &Type::Cap("Any".into(), None),
            &Type::Cap("Any".into(), None),
            &mut subst,
        );
        assert!(result.is_ok(), "cap Any should unify with cap Any");
    }

    // === Gap 6: Dynamic import type resolution ===

    #[test]
    fn test_dynamic_import_string_literal_resolves_exported_fns() {
        let dir = std::env::temp_dir().join("balance_infer_import_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Create a helper file with an exported function
        std::fs::write(
            dir.join("helpers.bl"),
            "export fn helper(x: Int) -> Int { return x + 1 }",
        )
        .unwrap();

        let source = r#"
            entry() {
                let m = import("helpers.bl")
                let result = helper(42)
                return result
            }
        "#;
        let tokens = crate::lexer::tokenize(source).unwrap();
        let (program, errors) = crate::parser::parse(source, &tokens);
        assert!(errors.is_empty());

        let infer_result = infer_program_with_root(&program, dir.clone());
        // Should NOT have "undefined variable 'helper'" error
        assert!(
            !infer_result.errors.iter().any(|e| e.message.contains("undefined variable 'helper'")),
            "expected no undefined error for dynamically imported fn, got: {:?}",
            infer_result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_dynamic_import_missing_file_no_error() {
        let dir = std::env::temp_dir().join("balance_infer_import_missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let source = r#"
            entry() {
                let m = import("nonexistent.bl")
                return m
            }
        "#;
        let tokens = crate::lexer::tokenize(source).unwrap();
        let (program, errors) = crate::parser::parse(source, &tokens);
        assert!(errors.is_empty());

        // Should not crash or produce an error for missing file — just doesn't resolve
        let infer_result = infer_program_with_root(&program, dir.clone());
        assert!(
            !infer_result.errors.iter().any(|e| e.message.contains("nonexistent")),
            "expected no error for missing import file, got: {:?}",
            infer_result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // === Gap 4: Reject cap construction (port-as-function) ===

    #[test]
    fn test_port_as_function_rejected() {
        let source = r#"
            port KV {
                get(key: String) -> String? [query]
            }
            entry() {
                let kv = KV()
                return kv
            }
        "#;
        let tokens = crate::lexer::tokenize(source).unwrap();
        let (program, parse_errors) = crate::parser::parse(source, &tokens);
        assert!(parse_errors.is_empty());
        let result = infer_program(&program);
        assert!(
            result.errors.iter().any(|e| e.message.contains("cannot be constructed directly")),
            "expected cap construction error, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_non_port_function_still_works() {
        let source = r#"
            fn double(x: Int) -> Int { return x * 2 }
            entry() {
                let y = double(21)
                return y
            }
        "#;
        let tokens = crate::lexer::tokenize(source).unwrap();
        let (program, parse_errors) = crate::parser::parse(source, &tokens);
        assert!(parse_errors.is_empty());
        let result = infer_program(&program);
        assert!(
            !result.errors.iter().any(|e| e.message.contains("cannot be constructed directly")),
            "should not get cap construction error for normal function"
        );
    }

    #[test]
    fn test_qualified_port_resolution() {
        // Register module exports with a port name, then access it via `alias.Port`
        let mut env = InferEnv::new();
        let mut exports = HashMap::new();
        exports.insert("KV".to_string(), Type::Con("KV".to_string()));
        env.register_module_exports("storage", exports);

        let expr = Expr::FieldAccess {
            receiver: Box::new(Spanned {
                node: Expr::Ident("storage".to_string()),
                span: crate::lexer::Span::new(0, 0),
            }),
            field: "KV".to_string(),
        };
        let mut subst = Substitution::new();
        let result = infer_expr(&expr, &mut env, &mut subst);
        assert!(result.is_ok(), "qualified port should resolve: {:?}", result.err());
        assert_eq!(result.unwrap(), Type::Con("KV".to_string()));
    }

    #[test]
    fn test_qualified_fn_resolution() {
        // Register module exports with a function return type
        let mut env = InferEnv::new();
        let mut exports = HashMap::new();
        exports.insert("helper".to_string(), Type::Con("Int".to_string()));
        env.register_module_exports("utils", exports);

        let expr = Expr::FieldAccess {
            receiver: Box::new(Spanned {
                node: Expr::Ident("utils".to_string()),
                span: crate::lexer::Span::new(0, 0),
            }),
            field: "helper".to_string(),
        };
        let mut subst = Substitution::new();
        let result = infer_expr(&expr, &mut env, &mut subst);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Type::Con("Int".to_string()));
    }

    #[test]
    fn test_qualified_unknown_member_error() {
        // Access a non-existent member on a known module alias
        let mut env = InferEnv::new();
        let mut exports = HashMap::new();
        exports.insert("KV".to_string(), Type::Con("KV".to_string()));
        env.register_module_exports("storage", exports);

        let expr = Expr::FieldAccess {
            receiver: Box::new(Spanned {
                node: Expr::Ident("storage".to_string()),
                span: crate::lexer::Span::new(0, 0),
            }),
            field: "NonExistent".to_string(),
        };
        let mut subst = Substitution::new();
        let result = infer_expr(&expr, &mut env, &mut subst);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("no exported member 'NonExistent'"),
            "error: {}",
            err.message
        );
    }

    #[test]
    fn test_serialization_boundary_warning_for_consume_cap() {
        // Passing a @consume capability to a service method should warn
        let mut env = InferEnv::new();
        env.define(
            "kv".to_string(),
            Type::Cap("KV".to_string(), Some(crate::ast::AuthorityQualifier::Delegate)),
        );
        env.define(
            "logger".to_string(),
            Type::Cap("Logger".to_string(), Some(crate::ast::AuthorityQualifier::Consume)),
        );
        env.ports.insert(
            "KV".to_string(),
            [("put".to_string(), (vec![Type::Con("String".to_string()), Type::Var(TypeVar::fresh())], Type::Con("String".to_string())))]
                .into_iter()
                .collect(),
        );
        let expr = Expr::MethodCall {
            receiver: Box::new(Spanned {
                node: Expr::Ident("kv".to_string()),
                span: crate::lexer::Span::new(0, 0),
            }),
            method: "put".to_string(),
            args: vec![
                Spanned {
                    node: Expr::Literal(crate::ast::Literal::String("key".to_string())),
                    span: crate::lexer::Span::new(0, 0),
                },
                Spanned {
                    node: Expr::Ident("logger".to_string()),
                    span: crate::lexer::Span::new(0, 0),
                },
            ],
        };
        let mut subst = Substitution::new();
        let result = infer_expr(&expr, &mut env, &mut subst);
        assert!(result.is_ok());
        assert_eq!(env.warnings.len(), 1);
        assert!(
            env.warnings[0].contains("@consume"),
            "warning: {}",
            env.warnings[0]
        );
        assert!(
            env.warnings[0].contains("service boundaries"),
            "warning: {}",
            env.warnings[0]
        );
    }

    #[test]
    fn test_serialization_boundary_no_warning_for_delegate_cap() {
        // Passing a @delegate capability to a service method should NOT warn
        let mut env = InferEnv::new();
        env.define(
            "kv".to_string(),
            Type::Cap("KV".to_string(), Some(crate::ast::AuthorityQualifier::Delegate)),
        );
        env.define(
            "logger".to_string(),
            Type::Cap("Logger".to_string(), Some(crate::ast::AuthorityQualifier::Delegate)),
        );
        env.ports.insert(
            "KV".to_string(),
            [("put".to_string(), (vec![Type::Con("String".to_string()), Type::Var(TypeVar::fresh())], Type::Con("String".to_string())))]
                .into_iter()
                .collect(),
        );
        let expr = Expr::MethodCall {
            receiver: Box::new(Spanned {
                node: Expr::Ident("kv".to_string()),
                span: crate::lexer::Span::new(0, 0),
            }),
            method: "put".to_string(),
            args: vec![
                Spanned {
                    node: Expr::Literal(crate::ast::Literal::String("key".to_string())),
                    span: crate::lexer::Span::new(0, 0),
                },
                Spanned {
                    node: Expr::Ident("logger".to_string()),
                    span: crate::lexer::Span::new(0, 0),
                },
            ],
        };
        let mut subst = Substitution::new();
        let result = infer_expr(&expr, &mut env, &mut subst);
        assert!(result.is_ok());
        assert!(env.warnings.is_empty(), "unexpected warnings: {:?}", env.warnings);
    }
}
