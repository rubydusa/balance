use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::lexer::span::Span;
use super::infer;

#[derive(Debug)]
pub struct TypeCheckError {
    pub message: String,
    pub span: Option<Span>,
}

#[derive(Debug)]
pub struct TypeWarning {
    pub message: String,
    pub span: Option<Span>,
}

pub struct TypeCheckResult {
    pub errors: Vec<TypeCheckError>,
    pub warnings: Vec<TypeWarning>,
}

/// Two-pass type checker.
/// Pass 1: Collect declarations into a type environment.
/// Pass 2: Validate bodies against declared types.
pub fn check_program(program: &Program) -> TypeCheckResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    // Pass 1: Collect type environment
    let mut env = TypeEnv::new();
    for item in &program.items {
        match &item.node {
            Item::Port(port) => env.register_port(port),
            Item::Service(service) => env.register_service(service),
            Item::TypeDecl(td) => env.register_type(td),
            Item::FnDecl(f) => env.register_fn(f),
            Item::Substrate(s) => env.register_substrate(s),
            Item::Guarantee(g) => env.register_guarantee(g),
            _ => {}
        }
    }

    // Pass 2: Check
    for item in &program.items {
        match &item.node {
            Item::Port(port) => check_port(port, &mut warnings),
            Item::Service(service) => check_service(service, &env, &mut errors, &mut warnings),
            Item::Entry(entry) => check_entry(entry, &env, &mut errors, &mut warnings),
            Item::FnDecl(f) => check_fn(f, &env, &mut errors, &mut warnings),
            Item::Guarantee(g) => check_guarantee(g, &env, &mut errors),
            _ => {}
        }
    }

    // Pass 2b: Check generic type parameter arity
    for item in &program.items {
        match &item.node {
            Item::FnDecl(f) => {
                for param in &f.params {
                    check_type_expr_arity(&param.ty.node, &env, &mut errors);
                }
                if let Some(rt) = &f.return_type {
                    check_type_expr_arity(&rt.node, &env, &mut errors);
                }
                for stmt in &f.body {
                    check_stmt_type_arity(&stmt.node, &env, &mut errors);
                }
            }
            Item::Entry(entry) => {
                for param in &entry.params {
                    check_type_expr_arity(&param.ty.node, &env, &mut errors);
                }
                for stmt in &entry.body {
                    check_stmt_type_arity(&stmt.node, &env, &mut errors);
                }
            }
            _ => {}
        }
    }

    // Pass 2c: Check for circular service resolve chains
    check_service_cycles(program, &mut errors);

    // Pass 2d: Validate profile references
    check_profile_references(program, &mut errors);

    // Pass 2e: Validate resolve service IDs against declared publish IDs
    check_resolve_publish_ids(program, &mut warnings);

    // Pass 2f: Check for duplicate profile declarations
    check_duplicate_profiles(program, &mut errors);

    // Pass 3: Run HM type inference on entry, function, and service bodies
    let infer_result = infer::infer_program(program);
    for ie in infer_result.errors {
        errors.push(TypeCheckError {
            message: ie.message,
            span: ie.span,
        });
    }
    for w in infer_result.warnings {
        warnings.push(TypeWarning { message: w, span: None });
    }

    TypeCheckResult { errors, warnings }
}

/// Type-check a program with a module root for resolving dynamic import types.
pub fn check_program_with_root(program: &Program, module_root: std::path::PathBuf) -> TypeCheckResult {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    // Pass 1: Collect type environment (same as check_program)
    let mut env = TypeEnv::new();
    for item in &program.items {
        match &item.node {
            Item::Port(port) => env.register_port(port),
            Item::Service(service) => env.register_service(service),
            Item::TypeDecl(td) => env.register_type(td),
            Item::FnDecl(f) => env.register_fn(f),
            Item::Substrate(s) => env.register_substrate(s),
            Item::Guarantee(g) => env.register_guarantee(g),
            _ => {}
        }
    }

    // Pass 2: Check (same as check_program)
    for item in &program.items {
        match &item.node {
            Item::Port(port) => check_port(port, &mut warnings),
            Item::Service(service) => check_service(service, &env, &mut errors, &mut warnings),
            Item::Entry(entry) => check_entry(entry, &env, &mut errors, &mut warnings),
            Item::FnDecl(f) => check_fn(f, &env, &mut errors, &mut warnings),
            Item::Guarantee(g) => check_guarantee(g, &env, &mut errors),
            _ => {}
        }
    }

    // Pass 2b-d (same as check_program)
    for item in &program.items {
        match &item.node {
            Item::FnDecl(f) => {
                for param in &f.params {
                    check_type_expr_arity(&param.ty.node, &env, &mut errors);
                }
                if let Some(rt) = &f.return_type {
                    check_type_expr_arity(&rt.node, &env, &mut errors);
                }
                for stmt in &f.body {
                    check_stmt_type_arity(&stmt.node, &env, &mut errors);
                }
            }
            Item::Entry(entry) => {
                for param in &entry.params {
                    check_type_expr_arity(&param.ty.node, &env, &mut errors);
                }
                for stmt in &entry.body {
                    check_stmt_type_arity(&stmt.node, &env, &mut errors);
                }
            }
            _ => {}
        }
    }
    check_service_cycles(program, &mut errors);
    check_profile_references(program, &mut errors);
    check_resolve_publish_ids(program, &mut warnings);
    check_duplicate_profiles(program, &mut errors);

    // Pass 3: Run HM type inference with module_root
    let infer_result = infer::infer_program_with_root(program, module_root);
    for ie in infer_result.errors {
        errors.push(TypeCheckError {
            message: ie.message,
            span: ie.span,
        });
    }
    for w in infer_result.warnings {
        warnings.push(TypeWarning { message: w, span: None });
    }

    TypeCheckResult { errors, warnings }
}

/// Type environment built in pass 1.
struct TypeEnv {
    ports: HashMap<String, PortInfo>,
    services: HashMap<String, ServiceInfo>,
    types: HashMap<String, TypeDeclInfo>,
    functions: HashMap<String, FnInfo>,
    substrates: HashMap<String, SubstrateInfo>,
    guarantees: HashMap<String, GuaranteeInfo>,
}

struct PortInfo {
    methods: HashMap<String, PortMethodInfo>,
}

struct PortMethodInfo {
    kind: Option<MethodKind>,
    param_count: usize,
    has_visible: bool,
    /// The event type argument from `visible(event_type)`, if any.
    visible_arg: Option<String>,
    #[allow(dead_code)]
    return_nullable: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum MethodKind {
    Command,
    Query,
}

struct ServiceInfo {
    provides: String,
    has_publish: bool,
    has_components: bool,
    /// Maps component name → substrate type name for EventRef validation.
    components: HashMap<String, String>,
    /// Whether the service has `on replicated(N)` directive.
    is_replicated: bool,
}

#[allow(dead_code)]
struct TypeDeclInfo {
    fields: Vec<String>,
    type_param_count: usize,
}

#[allow(dead_code)]
struct FnInfo {
    param_count: usize,
}

#[allow(dead_code)]
struct SubstrateInfo {
    ops: Vec<String>,
    emits: Vec<String>,
}

#[allow(dead_code)]
struct GuaranteeInfo {
    laws: Vec<String>,
}

impl TypeEnv {
    fn new() -> Self {
        Self {
            ports: HashMap::new(),
            services: HashMap::new(),
            types: HashMap::new(),
            functions: HashMap::new(),
            substrates: HashMap::new(),
            guarantees: HashMap::new(),
        }
    }

    fn register_port(&mut self, port: &PortDecl) {
        let mut methods = HashMap::new();
        for method in &port.methods {
            let kind = method
                .node
                .annotations
                .iter()
                .find(|a| a.name == "command" || a.name == "query")
                .map(|a| {
                    if a.name == "command" {
                        MethodKind::Command
                    } else {
                        MethodKind::Query
                    }
                });
            let visible_annotation = method
                .node
                .annotations
                .iter()
                .find(|a| a.name == "visible");
            let has_visible = visible_annotation.is_some();
            let visible_arg = visible_annotation.and_then(|a| a.arg.clone());
            let return_nullable = matches!(
                &method.node.return_type.node,
                TypeExpr::Named { nullable: true, .. }
            );
            methods.insert(
                method.node.name.clone(),
                PortMethodInfo {
                    kind,
                    param_count: method.node.params.len(),
                    has_visible,
                    visible_arg,
                    return_nullable,
                },
            );
        }
        self.ports.insert(port.name.clone(), PortInfo { methods });
    }

    fn register_service(&mut self, service: &ServiceDecl) {
        let has_publish = service
            .items
            .iter()
            .any(|i| matches!(&i.node, ServiceItem::Publish(_)));
        let is_replicated = service
            .items
            .iter()
            .any(|i| matches!(&i.node, ServiceItem::Replicated(_)));
        let mut components = HashMap::new();
        for item in &service.items {
            if let ServiceItem::Component(comp) = &item.node {
                components.insert(comp.name.clone(), comp.service.clone());
            }
        }
        let has_components = !components.is_empty();
        self.services.insert(
            service.name.clone(),
            ServiceInfo {
                provides: service.provides.clone(),
                has_publish,
                has_components,
                components,
                is_replicated,
            },
        );
    }

    fn register_type(&mut self, td: &TypeDecl) {
        let fields = td.fields.iter().map(|f| f.name.clone()).collect();
        self.types.insert(
            td.name.clone(),
            TypeDeclInfo {
                fields,
                type_param_count: td.type_params.len(),
            },
        );
    }

    fn register_fn(&mut self, f: &FnDecl) {
        self.functions.insert(
            f.name.clone(),
            FnInfo {
                param_count: f.params.len(),
            },
        );
    }

    fn register_substrate(&mut self, s: &SubstrateDecl) {
        self.substrates.insert(
            s.name.clone(),
            SubstrateInfo {
                ops: s.ops.iter().map(|o| o.name.clone()).collect(),
                emits: s.emits.iter().map(|e| e.name.clone()).collect(),
            },
        );
    }

    fn register_guarantee(&mut self, g: &GuaranteeDecl) {
        self.guarantees.insert(
            g.name.clone(),
            GuaranteeInfo {
                laws: g.laws.iter().map(|l| l.body.clone()).collect(),
            },
        );
    }
}

fn check_port(port: &PortDecl, warnings: &mut Vec<TypeWarning>) {
    for method in &port.methods {
        let has_command = method.node.annotations.iter().any(|a| a.name == "command");
        let has_query = method.node.annotations.iter().any(|a| a.name == "query");
        if !has_command && !has_query {
            warnings.push(TypeWarning {
                message: format!(
                    "port method '{}' in '{}' has no [command] or [query] annotation",
                    method.node.name, port.name
                ),
                span: Some(method.span),
            });
        }
    }
}

fn check_service(
    service: &ServiceDecl,
    env: &TypeEnv,
    errors: &mut Vec<TypeCheckError>,
    warnings: &mut Vec<TypeWarning>,
) {
    let has_publish = service
        .items
        .iter()
        .any(|i| matches!(&i.node, ServiceItem::Publish(_)));
    if !has_publish {
        warnings.push(TypeWarning {
            message: format!("service '{}' has no 'publish as' declaration", service.name),
            span: None,
        });
    }

    // Check resolve expressions reference declared ports
    let port_info = env.ports.get(&service.provides);

    for item in &service.items {
        match &item.node {
            ServiceItem::Command(cmd) => {
                // Block body commands are allowed — they manage substrate
                // interactions directly and rely on guarantee laws for
                // invariant enforcement at the substrate dispatch level.

                // Check arity against port declaration
                if let Some(port) = port_info {
                    if let Some(method_info) = port.methods.get(&cmd.name) {
                        if method_info.param_count != cmd.params.len() {
                            errors.push(TypeCheckError {
                                message: format!(
                                    "command '{}' in service '{}' has {} params, \
                                     but port declares {}",
                                    cmd.name,
                                    service.name,
                                    cmd.params.len(),
                                    method_info.param_count
                                ),
                                span: None,
                            });
                        }
                    }
                }
            }
            ServiceItem::Query(q) => {
                // Queries MUST include observe clause OR have visible() annotation
                // Exception: services without components are pure computations —
                // they don't need observation semantics since they have no substrate state.
                if matches!(&q.body, QueryBody::Block(_)) {
                    let svc_has_components = env
                        .services
                        .get(&service.name)
                        .map(|s| s.has_components)
                        .unwrap_or(false);

                    if svc_has_components {
                        let has_visible = port_info
                            .and_then(|p| p.methods.get(&q.name))
                            .map(|m| m.has_visible)
                            .unwrap_or(false);

                        if !has_visible {
                            errors.push(TypeCheckError {
                                message: format!(
                                    "query '{}' in service '{}' must declare observation semantics \
                                     via 'via'/'observe' or have [visible] annotation on port method",
                                    q.name, service.name
                                ),
                                span: None,
                            });
                        }
                    }
                }

                // Check arity against port declaration
                if let Some(port) = port_info {
                    if let Some(method_info) = port.methods.get(&q.name) {
                        if method_info.param_count != q.params.len() {
                            errors.push(TypeCheckError {
                                message: format!(
                                    "query '{}' in service '{}' has {} params, \
                                     but port declares {}",
                                    q.name,
                                    service.name,
                                    q.params.len(),
                                    method_info.param_count
                                ),
                                span: None,
                            });
                        }
                    }
                }
            }
            ServiceItem::Replicated(n) => {
                if *n == 0 {
                    errors.push(TypeCheckError {
                        message: format!(
                            "replication factor in service '{}' must be greater than 0",
                            service.name
                        ),
                        span: None,
                    });
                }
            }
            _ => {}
        }
    }

    // Check capability authority qualifiers on command/query params
    let svc_is_replicated = env
        .services
        .get(&service.name)
        .map(|s| s.is_replicated)
        .unwrap_or(false);

    for item in &service.items {
        let params = match &item.node {
            ServiceItem::Command(cmd) => Some((&cmd.name, &cmd.params)),
            ServiceItem::Query(q) => Some((&q.name, &q.params)),
            _ => None,
        };
        if let Some((method_name, params)) = params {
            for param in params {
                if let TypeExpr::Cap { qualifier, port_name } = &param.ty.node {
                    if qualifier.is_none() {
                        if svc_is_replicated {
                            errors.push(TypeCheckError {
                                message: format!(
                                    "capability parameter '{}' (cap {}) in service '{}' method '{}' \
                                     requires @delegate qualifier (service is replicated)",
                                    param.name, port_name, service.name, method_name
                                ),
                                span: Some(param.ty.span),
                            });
                        } else {
                            errors.push(TypeCheckError {
                                message: format!(
                                    "capability parameter '{}' (cap {}) in service '{}' method '{}' \
                                     requires an authority qualifier (@consume, @borrow, or @delegate)",
                                    param.name, port_name, service.name, method_name
                                ),
                                span: Some(param.ty.span),
                            });
                        }
                    }
                }
            }
        }
    }

    // Validate visible() annotation event type arguments against substrate emits
    if let Some(port) = port_info {
        if let Some(svc) = env.services.get(&service.name) {
            // Collect all event types emitted by any substrate component of this service
            let mut service_emits: std::collections::HashSet<String> = std::collections::HashSet::new();
            for (_comp_name, substrate_type) in &svc.components {
                if let Some(sub_info) = env.substrates.get(substrate_type) {
                    for emit in &sub_info.emits {
                        service_emits.insert(emit.clone());
                    }
                }
            }
            // Only validate if the service has components with known substrates
            if !service_emits.is_empty() {
                for method_info in port.methods.values() {
                    if let Some(ref event_type) = method_info.visible_arg {
                        if !service_emits.contains(event_type) {
                            errors.push(TypeCheckError {
                                message: format!(
                                    "visible('{}') in service '{}': event type '{}' is not emitted by any \
                                     substrate component (available: {})",
                                    event_type,
                                    service.name,
                                    event_type,
                                    service_emits.iter().cloned().collect::<Vec<_>>().join(", ")
                                ),
                                span: None,
                            });
                        }
                    }
                }
            }
        }
    }

    // Check resolve expressions in command/query bodies
    check_service_resolves(service, env, errors, warnings);
}

/// Check that resolve expressions in service bodies reference declared ports,
/// and that settle/observe event references point to valid components and events.
fn check_service_resolves(
    service: &ServiceDecl,
    env: &TypeEnv,
    errors: &mut Vec<TypeCheckError>,
    warnings: &mut Vec<TypeWarning>,
) {
    let service_info = env.services.get(&service.name);
    for item in &service.items {
        match &item.node {
            ServiceItem::Command(cmd) => match &cmd.body {
                CommandBody::ViaSettle {
                    via_expr,
                    settle_event,
                    ..
                } => {
                    check_expr_resolves(&via_expr.node, env, errors, warnings);
                    check_event_ref(
                        &service.name,
                        settle_event,
                        "settle",
                        service_info,
                        env,
                        errors,
                    );
                }
                CommandBody::Block(stmts) => {
                    for stmt in stmts {
                        check_stmt_resolves(&stmt.node, env, errors, warnings);
                    }
                }
            },
            ServiceItem::Query(q) => match &q.body {
                QueryBody::ViaObserve {
                    via_expr,
                    observe_event,
                    return_expr,
                    ..
                } => {
                    check_expr_resolves(&via_expr.node, env, errors, warnings);
                    check_expr_resolves(&return_expr.node, env, errors, warnings);
                    check_event_ref(
                        &service.name,
                        observe_event,
                        "observe",
                        service_info,
                        env,
                        errors,
                    );
                }
                QueryBody::Block(stmts) => {
                    for stmt in stmts {
                        check_stmt_resolves(&stmt.node, env, errors, warnings);
                    }
                }
            },
            _ => {}
        }
    }
}

/// Validate that an EventRef (e.g., `log.quorum_committed`) references a
/// declared component whose substrate type emits the named event.
fn check_event_ref(
    service_name: &str,
    event_ref: &EventRef,
    context: &str,
    service_info: Option<&ServiceInfo>,
    env: &TypeEnv,
    errors: &mut Vec<TypeCheckError>,
) {
    let Some(svc) = service_info else { return };

    if let Some(substrate_type) = svc.components.get(&event_ref.source) {
        // Component found — validate the event type against the substrate's emits
        if let Some(substrate_info) = env.substrates.get(substrate_type) {
            if !substrate_info.emits.contains(&event_ref.event) {
                errors.push(TypeCheckError {
                    message: format!(
                        "{context} in service '{}': substrate '{}' does not emit event '{}' \
                         (available: {})",
                        service_name,
                        substrate_type,
                        event_ref.event,
                        substrate_info.emits.join(", ")
                    ),
                    span: None,
                });
            }
        }
        // If substrate type not found in env, it may be user-defined — skip validation
    } else {
        // No component with this name in the service
        let available: Vec<&String> = svc.components.keys().collect();
        errors.push(TypeCheckError {
            message: format!(
                "{context} source '{}' in service '{}' is not a declared component \
                 (available: {})",
                event_ref.source,
                service_name,
                if available.is_empty() {
                    "none".to_string()
                } else {
                    available.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                }
            ),
            span: None,
        });
    }
}

fn check_stmt_resolves(stmt: &Stmt, env: &TypeEnv, errors: &mut Vec<TypeCheckError>, warnings: &mut Vec<TypeWarning>) {
    match stmt {
        Stmt::Let { value, .. } => check_expr_resolves(&value.node, env, errors, warnings),
        Stmt::Return(Some(expr)) => check_expr_resolves(&expr.node, env, errors, warnings),
        Stmt::Expr(expr) => check_expr_resolves(&expr.node, env, errors, warnings),
        Stmt::If {
            condition,
            then_block,
            else_block,
        } => {
            check_expr_resolves(&condition.node, env, errors, warnings);
            for stmt in then_block {
                check_stmt_resolves(&stmt.node, env, errors, warnings);
            }
            if let Some(else_stmts) = else_block {
                for stmt in else_stmts {
                    check_stmt_resolves(&stmt.node, env, errors, warnings);
                }
            }
        }
        Stmt::Match { expr, arms } => {
            check_expr_resolves(&expr.node, env, errors, warnings);
            for arm in arms {
                if let Some(ref guard) = arm.guard {
                    check_expr_resolves(&guard.node, env, errors, warnings);
                }
                for stmt in &arm.body {
                    check_stmt_resolves(&stmt.node, env, errors, warnings);
                }
            }
        }
        Stmt::For { iterable, body, .. } => {
            check_expr_resolves(&iterable.node, env, errors, warnings);
            for stmt in body {
                check_stmt_resolves(&stmt.node, env, errors, warnings);
            }
        }
        Stmt::While { condition, body } => {
            check_expr_resolves(&condition.node, env, errors, warnings);
            for stmt in body {
                check_stmt_resolves(&stmt.node, env, errors, warnings);
            }
        }
        _ => {}
    }
}

fn check_expr_resolves(expr: &Expr, env: &TypeEnv, errors: &mut Vec<TypeCheckError>, warnings: &mut Vec<TypeWarning>) {
    match expr {
        Expr::Resolve { port, .. } => {
            // Built-in ports (Stdout, etc.) are allowed
            if !env.ports.contains_key(port)
                && port != "Stdout"
                && port != "Log"
                && port != "Index"
            {
                errors.push(TypeCheckError {
                    message: format!("resolve references undeclared port '{port}'"),
                    span: None,
                });
            }
        }
        Expr::MethodCall {
            receiver, args, ..
        } => {
            check_expr_resolves(&receiver.node, env, errors, warnings);
            for arg in args {
                check_expr_resolves(&arg.node, env, errors, warnings);
            }
        }
        Expr::FnCall { func, args } => {
            check_expr_resolves(&func.node, env, errors, warnings);
            for arg in args {
                check_expr_resolves(&arg.node, env, errors, warnings);
            }
        }
        Expr::FieldAccess { receiver, .. } => {
            check_expr_resolves(&receiver.node, env, errors, warnings);
        }
        Expr::Binary { left, right, .. } => {
            check_expr_resolves(&left.node, env, errors, warnings);
            check_expr_resolves(&right.node, env, errors, warnings);
        }
        Expr::Unary { operand, .. } => {
            check_expr_resolves(&operand.node, env, errors, warnings);
        }
        Expr::Block(stmts) => {
            for stmt in stmts {
                check_stmt_resolves(&stmt.node, env, errors, warnings);
            }
        }
        Expr::StructLiteral { fields, .. } => {
            for (_, fexpr) in fields {
                check_expr_resolves(&fexpr.node, env, errors, warnings);
            }
        }
        Expr::Await { expr } => {
            check_expr_resolves(&expr.node, env, errors, warnings);
        }
        Expr::ConcurrentAwait { exprs } => {
            for expr in exprs {
                check_expr_resolves(&expr.node, env, errors, warnings);
            }
        }
        Expr::ListLiteral { elements } => {
            for elem in elements {
                check_expr_resolves(&elem.node, env, errors, warnings);
            }
        }
        Expr::MapLiteral { entries } => {
            for (k, v) in entries {
                check_expr_resolves(&k.node, env, errors, warnings);
                check_expr_resolves(&v.node, env, errors, warnings);
            }
        }
        Expr::Closure { body, .. } => {
            for stmt in body {
                check_stmt_resolves(&stmt.node, env, errors, warnings);
            }
        }
        Expr::MacroCall { args, .. } => {
            for arg in args {
                check_expr_resolves(&arg.node, env, errors, warnings);
            }
        }
        Expr::DynamicImport { path } => {
            check_expr_resolves(&path.node, env, errors, warnings);
            // Check if import path is a string literal (preferred for static validation)
            if !matches!(&path.node, Expr::Literal(Literal::String(_))) {
                warnings.push(TypeWarning {
                    message: "dynamic import path should be a string literal for static validation; \
                              use static imports for full type safety"
                        .to_string(),
                    span: None,
                });
            } else {
                warnings.push(TypeWarning {
                    message: "dynamic import types are not statically checked; \
                              use static imports for full type safety"
                        .to_string(),
                    span: None,
                });
            }
        }
        Expr::Index { receiver, index } => {
            check_expr_resolves(&receiver.node, env, errors, warnings);
            check_expr_resolves(&index.node, env, errors, warnings);
        }
        Expr::Match { expr, arms } => {
            check_expr_resolves(&expr.node, env, errors, warnings);
            for arm in arms {
                if let Some(ref guard) = arm.guard {
                    check_expr_resolves(&guard.node, env, errors, warnings);
                }
                for stmt in &arm.body {
                    check_stmt_resolves(&stmt.node, env, errors, warnings);
                }
            }
        }
        Expr::Try { expr } => {
            check_expr_resolves(&expr.node, env, errors, warnings);
        }
        Expr::Select { timeout_ms, arms, else_body } => {
            if let Some(ref te) = timeout_ms {
                check_expr_resolves(&te.node, env, errors, warnings);
            }
            for arm in arms {
                check_expr_resolves(&arm.expr.node, env, errors, warnings);
                for stmt in &arm.body {
                    check_stmt_resolves(&stmt.node, env, errors, warnings);
                }
            }
            if let Some(ref eb) = else_body {
                for stmt in eb {
                    check_stmt_resolves(&stmt.node, env, errors, warnings);
                }
            }
        }
        _ => {}
    }
}

fn check_entry(entry: &EntryDecl, env: &TypeEnv, errors: &mut Vec<TypeCheckError>, warnings: &mut Vec<TypeWarning>) {
    for param in &entry.params {
        match &param.ty.node {
            TypeExpr::Cap { qualifier, port_name } => {
                // Bare cap without authority qualifier is an error — authority must be explicit
                if qualifier.is_none() {
                    errors.push(TypeCheckError {
                        message: format!(
                            "capability parameter '{}' (cap {}) requires an authority qualifier \
                             (@consume, @borrow, or @delegate)",
                            param.name, port_name
                        ),
                        span: Some(param.ty.span),
                    });
                }
            }
            TypeExpr::Named { .. } => {
                warnings.push(TypeWarning {
                    message: format!(
                        "entry parameter '{}' is a value type; consider using 'cap' for capabilities",
                        param.name
                    ),
                    span: Some(param.ty.span),
                });
            }
        }
    }
    // Walk entry body for resolve/import checks
    for stmt in &entry.body {
        check_stmt_resolves(&stmt.node, env, errors, warnings);
    }
}

fn check_fn(f: &FnDecl, env: &TypeEnv, errors: &mut Vec<TypeCheckError>, warnings: &mut Vec<TypeWarning>) {
    // Pure function checking: pure fns cannot perform capability operations
    if f.pure {
        let mut purity_errors = Vec::new();
        for stmt in &f.body {
            check_purity_stmt(&stmt.node, &f.name, &mut purity_errors);
        }
        for msg in purity_errors {
            errors.push(TypeCheckError { message: msg, span: None });
        }
    }
    // Walk function body for resolve/import checks
    for stmt in &f.body {
        check_stmt_resolves(&stmt.node, env, errors, warnings);
    }
}

/// Check that guarantee law event types reference events actually emitted by
/// substrates declared in the program.
fn check_guarantee(
    guarantee: &GuaranteeDecl,
    env: &TypeEnv,
    errors: &mut Vec<TypeCheckError>,
) {
    // Collect all event types emitted by all substrates
    let mut all_emits: std::collections::HashSet<String> = std::collections::HashSet::new();
    for substrate_info in env.substrates.values() {
        for emit in &substrate_info.emits {
            all_emits.insert(emit.clone());
        }
    }

    // If no substrates are declared, skip validation (may be user-defined runtime)
    if all_emits.is_empty() {
        return;
    }

    for law in &guarantee.laws {
        // Absence laws (NOT event_type) may reference events that should never occur,
        // so they don't need to be in any substrate's emits list.
        if law.body.trim().starts_with("NOT ") {
            continue;
        }
        // Extract event types from the law body text
        let event_types = extract_event_types_from_law(&law.body);
        for event_type in &event_types {
            if !all_emits.contains(event_type) {
                errors.push(TypeCheckError {
                    message: format!(
                        "guarantee '{}' references event type '{}' which is not emitted by any declared substrate (available: {})",
                        guarantee.name,
                        event_type,
                        all_emits.iter().cloned().collect::<Vec<_>>().join(", ")
                    ),
                    span: None,
                });
            }
        }
    }
}

/// Extract event type names from a guarantee law body string.
/// Handles formats like "event_type(key)" and "event_type".
fn extract_event_types_from_law(body: &str) -> Vec<String> {
    let mut types = Vec::new();
    let body = body.trim();

    // Remove NOT prefix
    let body = body.strip_prefix("NOT ").unwrap_or(body);

    // Split by =>, AND, must_precede
    let parts: Vec<&str> = body
        .split("=>")
        .flat_map(|s| s.split(" AND "))
        .flat_map(|s| s.split("must_precede"))
        .collect();

    for part in parts {
        let part = part.trim();
        // Remove "within Nms" suffix
        let part = if let Some(idx) = part.find("within ") {
            part[..idx].trim()
        } else {
            part
        };
        // Remove ∃ / exists prefix
        let part = part
            .trim_start_matches("∃")
            .trim_start_matches("exists")
            .trim();
        if part.is_empty() {
            continue;
        }
        // Extract event type (text before '(' or the whole thing)
        let event_type = if let Some(paren) = part.find('(') {
            part[..paren].trim().to_string()
        } else {
            part.to_string()
        };
        if !event_type.is_empty() {
            types.push(event_type);
        }
    }

    types
}

/// Check that type expressions use the correct number of type arguments
/// for user-declared generic types.
fn check_type_expr_arity(
    te: &TypeExpr,
    env: &TypeEnv,
    errors: &mut Vec<TypeCheckError>,
) {
    if let TypeExpr::Named {
        name, type_args, ..
    } = te
    {
        // Skip built-in types (they handle their own arity)
        let builtins = [
            "String", "Int", "Float", "Bool", "Unit", "Any", "Ack", "List", "Map",
            "Interaction", "Observed",
        ];
        if !builtins.contains(&name.as_str()) {
            if let Some(type_info) = env.types.get(name) {
                if type_info.type_param_count > 0 && !type_args.is_empty()
                    && type_args.len() != type_info.type_param_count
                {
                    errors.push(TypeCheckError {
                        message: format!(
                            "type '{}' expects {} type parameter(s) but got {}",
                            name, type_info.type_param_count, type_args.len()
                        ),
                        span: None,
                    });
                }
            }
        }

        // Recursively check type args
        for arg in type_args {
            check_type_expr_arity(&arg.node, env, errors);
        }
    }
}

/// Walk a statement's type expressions to check generic arity.
fn check_stmt_type_arity(stmt: &Stmt, env: &TypeEnv, errors: &mut Vec<TypeCheckError>) {
    match stmt {
        Stmt::Let { ty, value, .. } => {
            if let Some(te) = ty {
                check_type_expr_arity(&te.node, env, errors);
            }
            check_expr_type_arity(&value.node, env, errors);
        }
        Stmt::If {
            condition,
            then_block,
            else_block,
        } => {
            check_expr_type_arity(&condition.node, env, errors);
            for s in then_block {
                check_stmt_type_arity(&s.node, env, errors);
            }
            if let Some(eb) = else_block {
                for s in eb {
                    check_stmt_type_arity(&s.node, env, errors);
                }
            }
        }
        Stmt::Return(Some(expr)) => {
            check_expr_type_arity(&expr.node, env, errors);
        }
        Stmt::Expr(expr) => {
            check_expr_type_arity(&expr.node, env, errors);
        }
        Stmt::For { iterable, body, .. } => {
            check_expr_type_arity(&iterable.node, env, errors);
            for s in body {
                check_stmt_type_arity(&s.node, env, errors);
            }
        }
        Stmt::While { condition, body } => {
            check_expr_type_arity(&condition.node, env, errors);
            for s in body {
                check_stmt_type_arity(&s.node, env, errors);
            }
        }
        Stmt::Match { expr, arms } => {
            check_expr_type_arity(&expr.node, env, errors);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    check_expr_type_arity(&guard.node, env, errors);
                }
                for s in &arm.body {
                    check_stmt_type_arity(&s.node, env, errors);
                }
            }
        }
        _ => {}
    }
}

/// Walk an expression's type expressions to check generic arity.
fn check_expr_type_arity(expr: &Expr, env: &TypeEnv, errors: &mut Vec<TypeCheckError>) {
    match expr {
        Expr::StructLiteral { fields, .. } => {
            for (_, fexpr) in fields {
                check_expr_type_arity(&fexpr.node, env, errors);
            }
        }
        Expr::FnCall { func, args } => {
            check_expr_type_arity(&func.node, env, errors);
            for arg in args {
                check_expr_type_arity(&arg.node, env, errors);
            }
        }
        Expr::Binary { left, right, .. } => {
            check_expr_type_arity(&left.node, env, errors);
            check_expr_type_arity(&right.node, env, errors);
        }
        Expr::Block(stmts) => {
            for s in stmts {
                check_stmt_type_arity(&s.node, env, errors);
            }
        }
        _ => {}
    }
}

fn check_purity_expr(expr: &Expr, fn_name: &str, errors: &mut Vec<String>) {
    match expr {
        Expr::Resolve { .. } => {
            errors.push(format!(
                "pure function '{}' cannot perform capability resolution",
                fn_name
            ));
        }
        Expr::Await { expr } => {
            errors.push(format!(
                "pure function '{}' cannot use 'await' (capability operation)",
                fn_name
            ));
            check_purity_expr(&expr.node, fn_name, errors);
        }
        Expr::MethodCall { receiver, args, .. } => {
            check_purity_expr(&receiver.node, fn_name, errors);
            for arg in args {
                check_purity_expr(&arg.node, fn_name, errors);
            }
        }
        Expr::FnCall { func, args } => {
            check_purity_expr(&func.node, fn_name, errors);
            for arg in args {
                check_purity_expr(&arg.node, fn_name, errors);
            }
        }
        Expr::Binary { left, right, .. } => {
            check_purity_expr(&left.node, fn_name, errors);
            check_purity_expr(&right.node, fn_name, errors);
        }
        Expr::Unary { operand, .. } => {
            check_purity_expr(&operand.node, fn_name, errors);
        }
        Expr::Block(stmts) => {
            for stmt in stmts {
                check_purity_stmt(&stmt.node, fn_name, errors);
            }
        }
        Expr::FieldAccess { receiver, .. } => {
            check_purity_expr(&receiver.node, fn_name, errors);
        }
        Expr::StructLiteral { fields, .. } => {
            for (_, fexpr) in fields {
                check_purity_expr(&fexpr.node, fn_name, errors);
            }
        }
        Expr::ListLiteral { elements } => {
            for elem in elements {
                check_purity_expr(&elem.node, fn_name, errors);
            }
        }
        Expr::MapLiteral { entries } => {
            for (k, v) in entries {
                check_purity_expr(&k.node, fn_name, errors);
                check_purity_expr(&v.node, fn_name, errors);
            }
        }
        Expr::Index { receiver, index } => {
            check_purity_expr(&receiver.node, fn_name, errors);
            check_purity_expr(&index.node, fn_name, errors);
        }
        Expr::ConcurrentAwait { .. } => {
            errors.push(format!(
                "pure function '{}' cannot use 'concurrent' (side-effectful dispatch)",
                fn_name
            ));
        }
        Expr::Select { .. } => {
            errors.push(format!(
                "pure function '{}' cannot use 'select' (I/O multiplexing)",
                fn_name
            ));
        }
        Expr::Closure { body, .. } => {
            for stmt in body {
                check_purity_stmt(&stmt.node, fn_name, errors);
            }
        }
        Expr::DynamicImport { .. } => {
            errors.push(format!(
                "pure function '{}' cannot use 'import()' (I/O operation)",
                fn_name
            ));
        }
        Expr::MacroCall { .. } => {
            errors.push(format!(
                "pure function '{}' cannot use macro calls (macros should be expanded before checking)",
                fn_name
            ));
        }
        Expr::Match { expr, arms } => {
            check_purity_expr(&expr.node, fn_name, errors);
            for arm in arms {
                if let Some(ref guard) = arm.guard {
                    check_purity_expr(&guard.node, fn_name, errors);
                }
                for stmt in &arm.body {
                    check_purity_stmt(&stmt.node, fn_name, errors);
                }
            }
        }
        _ => {}
    }
}

fn check_purity_stmt(stmt: &Stmt, fn_name: &str, errors: &mut Vec<String>) {
    match stmt {
        Stmt::Let { value, .. } => check_purity_expr(&value.node, fn_name, errors),
        Stmt::Return(Some(expr)) => check_purity_expr(&expr.node, fn_name, errors),
        Stmt::Expr(expr) => check_purity_expr(&expr.node, fn_name, errors),
        Stmt::If { condition, then_block, else_block } => {
            check_purity_expr(&condition.node, fn_name, errors);
            for stmt in then_block {
                check_purity_stmt(&stmt.node, fn_name, errors);
            }
            if let Some(else_stmts) = else_block {
                for stmt in else_stmts {
                    check_purity_stmt(&stmt.node, fn_name, errors);
                }
            }
        }
        Stmt::Match { expr, arms } => {
            check_purity_expr(&expr.node, fn_name, errors);
            for arm in arms {
                if let Some(ref guard) = arm.guard {
                    check_purity_expr(&guard.node, fn_name, errors);
                }
                for stmt in &arm.body {
                    check_purity_stmt(&stmt.node, fn_name, errors);
                }
            }
        }
        Stmt::For { iterable, body, .. } => {
            check_purity_expr(&iterable.node, fn_name, errors);
            for stmt in body {
                check_purity_stmt(&stmt.node, fn_name, errors);
            }
        }
        Stmt::While { condition, body } => {
            check_purity_expr(&condition.node, fn_name, errors);
            for stmt in body {
                check_purity_stmt(&stmt.node, fn_name, errors);
            }
        }
        _ => {}
    }
}

/// Detect circular service resolve chains.
/// Builds a directed graph: service → resolved-ports → providing-services.
/// Reports errors for any cycles found via DFS.
fn check_service_cycles(program: &Program, errors: &mut Vec<TypeCheckError>) {
    // Map port name → services that provide it
    let mut port_providers: HashMap<String, Vec<String>> = HashMap::new();
    for item in &program.items {
        if let Item::Service(service) = &item.node {
            port_providers
                .entry(service.provides.clone())
                .or_default()
                .push(service.name.clone());
        }
    }

    // Map service → ports it resolves (from resolve expressions in bodies)
    let mut service_resolves: HashMap<String, HashSet<String>> = HashMap::new();
    for item in &program.items {
        if let Item::Service(service) = &item.node {
            let mut resolved_ports = HashSet::new();
            for svc_item in &service.items {
                match &svc_item.node {
                    ServiceItem::Command(cmd) => {
                        collect_resolve_ports_from_body_cmd(&cmd.body, &mut resolved_ports);
                    }
                    ServiceItem::Query(q) => {
                        collect_resolve_ports_from_body_query(&q.body, &mut resolved_ports);
                    }
                    _ => {}
                }
            }
            if !resolved_ports.is_empty() {
                service_resolves.insert(service.name.clone(), resolved_ports);
            }
        }
    }

    // Build adjacency list: service → set of services it depends on
    let mut adj: HashMap<String, HashSet<String>> = HashMap::new();
    for (service, ports) in &service_resolves {
        for port in ports {
            if let Some(providers) = port_providers.get(port) {
                for provider in providers {
                    if provider != service {
                        adj.entry(service.clone())
                            .or_default()
                            .insert(provider.clone());
                    }
                }
            }
        }
    }

    // DFS cycle detection
    let all_services: Vec<String> = adj.keys().cloned().collect();
    let mut visited: HashSet<String> = HashSet::new();
    let mut in_stack: HashSet<String> = HashSet::new();
    let mut path: Vec<String> = Vec::new();

    for service in &all_services {
        if !visited.contains(service) {
            dfs_cycle_check(
                service,
                &adj,
                &mut visited,
                &mut in_stack,
                &mut path,
                errors,
            );
        }
    }
}

fn dfs_cycle_check(
    node: &str,
    adj: &HashMap<String, HashSet<String>>,
    visited: &mut HashSet<String>,
    in_stack: &mut HashSet<String>,
    path: &mut Vec<String>,
    errors: &mut Vec<TypeCheckError>,
) {
    visited.insert(node.to_string());
    in_stack.insert(node.to_string());
    path.push(node.to_string());

    if let Some(neighbors) = adj.get(node) {
        for next in neighbors {
            if in_stack.contains(next) {
                // Found a cycle — report it
                let cycle_start = path.iter().position(|s| s == next).unwrap();
                let cycle: Vec<&str> = path[cycle_start..].iter().map(|s| s.as_str()).collect();
                errors.push(TypeCheckError {
                    message: format!(
                        "circular service dependency detected: {} -> {}",
                        cycle.join(" -> "),
                        next
                    ),
                    span: None,
                });
            } else if !visited.contains(next) {
                dfs_cycle_check(next, adj, visited, in_stack, path, errors);
            }
        }
    }

    path.pop();
    in_stack.remove(node);
}

/// Collect resolved port names from a command body.
fn collect_resolve_ports_from_body_cmd(body: &CommandBody, ports: &mut HashSet<String>) {
    match body {
        CommandBody::Block(stmts) => {
            for stmt in stmts {
                collect_resolve_ports_from_stmt(&stmt.node, ports);
            }
        }
        CommandBody::ViaSettle { via_expr, .. } => {
            collect_resolve_ports_from_expr(&via_expr.node, ports);
        }
    }
}

/// Collect resolved port names from a query body.
fn collect_resolve_ports_from_body_query(body: &QueryBody, ports: &mut HashSet<String>) {
    match body {
        QueryBody::Block(stmts) => {
            for stmt in stmts {
                collect_resolve_ports_from_stmt(&stmt.node, ports);
            }
        }
        QueryBody::ViaObserve { via_expr, return_expr, .. } => {
            collect_resolve_ports_from_expr(&via_expr.node, ports);
            collect_resolve_ports_from_expr(&return_expr.node, ports);
        }
    }
}

fn collect_resolve_ports_from_stmt(stmt: &Stmt, ports: &mut HashSet<String>) {
    match stmt {
        Stmt::Let { value, .. } => collect_resolve_ports_from_expr(&value.node, ports),
        Stmt::Return(Some(expr)) => collect_resolve_ports_from_expr(&expr.node, ports),
        Stmt::Expr(expr) => collect_resolve_ports_from_expr(&expr.node, ports),
        Stmt::If { condition, then_block, else_block } => {
            collect_resolve_ports_from_expr(&condition.node, ports);
            for s in then_block { collect_resolve_ports_from_stmt(&s.node, ports); }
            if let Some(eb) = else_block {
                for s in eb { collect_resolve_ports_from_stmt(&s.node, ports); }
            }
        }
        Stmt::For { iterable, body, .. } => {
            collect_resolve_ports_from_expr(&iterable.node, ports);
            for s in body { collect_resolve_ports_from_stmt(&s.node, ports); }
        }
        _ => {}
    }
}

fn collect_resolve_ports_from_expr(expr: &Expr, ports: &mut HashSet<String>) {
    match expr {
        Expr::Resolve { port, .. } => { ports.insert(port.clone()); }
        Expr::FnCall { func, args } => {
            collect_resolve_ports_from_expr(&func.node, ports);
            for a in args { collect_resolve_ports_from_expr(&a.node, ports); }
        }
        Expr::MethodCall { receiver, args, .. } => {
            collect_resolve_ports_from_expr(&receiver.node, ports);
            for a in args { collect_resolve_ports_from_expr(&a.node, ports); }
        }
        Expr::Await { expr } => collect_resolve_ports_from_expr(&expr.node, ports),
        Expr::Binary { left, right, .. } => {
            collect_resolve_ports_from_expr(&left.node, ports);
            collect_resolve_ports_from_expr(&right.node, ports);
        }
        Expr::Block(stmts) => {
            for s in stmts { collect_resolve_ports_from_stmt(&s.node, ports); }
        }
        _ => {}
    }
}

/// Validate that `with profile NAME` references exist as declared profiles.
fn check_profile_references(program: &Program, errors: &mut Vec<TypeCheckError>) {
    // Collect declared profile names
    let mut profile_names: HashSet<String> = HashSet::new();
    for item in &program.items {
        if let Item::Profile(p) = &item.node {
            profile_names.insert(p.name.clone());
        }
    }

    // If no profiles are declared, skip validation (profiles may come from balance.toml)
    if profile_names.is_empty() {
        return;
    }

    // Check all Expr::Resolve with profile references
    for item in &program.items {
        match &item.node {
            Item::Entry(entry) => {
                for stmt in &entry.body {
                    check_profile_refs_in_stmt(&stmt.node, &profile_names, errors);
                }
            }
            Item::FnDecl(f) => {
                for stmt in &f.body {
                    check_profile_refs_in_stmt(&stmt.node, &profile_names, errors);
                }
            }
            Item::Service(service) => {
                for svc_item in &service.items {
                    match &svc_item.node {
                        ServiceItem::Command(cmd) => match &cmd.body {
                            CommandBody::Block(stmts) => {
                                for stmt in stmts {
                                    check_profile_refs_in_stmt(&stmt.node, &profile_names, errors);
                                }
                            }
                            CommandBody::ViaSettle { via_expr, .. } => {
                                check_profile_refs_in_expr(&via_expr.node, &profile_names, errors);
                            }
                        },
                        ServiceItem::Query(q) => match &q.body {
                            QueryBody::Block(stmts) => {
                                for stmt in stmts {
                                    check_profile_refs_in_stmt(&stmt.node, &profile_names, errors);
                                }
                            }
                            QueryBody::ViaObserve { via_expr, return_expr, .. } => {
                                check_profile_refs_in_expr(&via_expr.node, &profile_names, errors);
                                check_profile_refs_in_expr(&return_expr.node, &profile_names, errors);
                            }
                        },
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

fn check_profile_refs_in_stmt(stmt: &Stmt, profiles: &HashSet<String>, errors: &mut Vec<TypeCheckError>) {
    match stmt {
        Stmt::Let { value, .. } => check_profile_refs_in_expr(&value.node, profiles, errors),
        Stmt::Return(Some(expr)) => check_profile_refs_in_expr(&expr.node, profiles, errors),
        Stmt::Expr(expr) => check_profile_refs_in_expr(&expr.node, profiles, errors),
        Stmt::If { condition, then_block, else_block } => {
            check_profile_refs_in_expr(&condition.node, profiles, errors);
            for s in then_block { check_profile_refs_in_stmt(&s.node, profiles, errors); }
            if let Some(eb) = else_block {
                for s in eb { check_profile_refs_in_stmt(&s.node, profiles, errors); }
            }
        }
        Stmt::For { iterable, body, .. } => {
            check_profile_refs_in_expr(&iterable.node, profiles, errors);
            for s in body { check_profile_refs_in_stmt(&s.node, profiles, errors); }
        }
        _ => {}
    }
}

fn check_profile_refs_in_expr(expr: &Expr, profiles: &HashSet<String>, errors: &mut Vec<TypeCheckError>) {
    match expr {
        Expr::Resolve { profile, .. } => {
            if let Some(ref p) = profile {
                if !profiles.contains(p) {
                    errors.push(TypeCheckError {
                        message: format!(
                            "unknown profile '{}' in resolve expression (declared profiles: {})",
                            p,
                            if profiles.is_empty() {
                                "none".to_string()
                            } else {
                                profiles.iter().cloned().collect::<Vec<_>>().join(", ")
                            }
                        ),
                        span: None,
                    });
                }
            }
        }
        Expr::FnCall { func, args } => {
            check_profile_refs_in_expr(&func.node, profiles, errors);
            for a in args { check_profile_refs_in_expr(&a.node, profiles, errors); }
        }
        Expr::MethodCall { receiver, args, .. } => {
            check_profile_refs_in_expr(&receiver.node, profiles, errors);
            for a in args { check_profile_refs_in_expr(&a.node, profiles, errors); }
        }
        Expr::Await { expr } => check_profile_refs_in_expr(&expr.node, profiles, errors),
        Expr::Binary { left, right, .. } => {
            check_profile_refs_in_expr(&left.node, profiles, errors);
            check_profile_refs_in_expr(&right.node, profiles, errors);
        }
        Expr::Block(stmts) => {
            for s in stmts { check_profile_refs_in_stmt(&s.node, profiles, errors); }
        }
        _ => {}
    }
}

/// Check for duplicate profile declarations in the program.
fn check_duplicate_profiles(program: &Program, errors: &mut Vec<TypeCheckError>) {
    let mut seen: HashSet<String> = HashSet::new();
    for item in &program.items {
        if let Item::Profile(p) = &item.node {
            if !seen.insert(p.name.clone()) {
                errors.push(TypeCheckError {
                    message: format!("duplicate profile declaration: '{}'", p.name),
                    span: None,
                });
            }
        }
    }
}

/// Validate that `resolve Port["name"]` references a service that declares
/// `publish as "name"` within this program. Missing IDs produce a warning
/// (not an error) because the service may be remote/external.
fn check_resolve_publish_ids(program: &Program, warnings: &mut Vec<TypeWarning>) {
    // Collect all declared publish IDs
    let mut publish_ids: HashSet<String> = HashSet::new();
    for item in &program.items {
        if let Item::Service(service) = &item.node {
            for si in &service.items {
                if let ServiceItem::Publish(id) = &si.node {
                    publish_ids.insert(id.clone());
                }
            }
        }
    }

    // If no services are declared, skip — nothing to validate against
    if publish_ids.is_empty() {
        return;
    }

    // Walk all entries, fns, and service bodies for resolve expressions
    for item in &program.items {
        match &item.node {
            Item::Entry(entry) => {
                for stmt in &entry.body {
                    collect_resolve_ids_stmt(&stmt.node, &publish_ids, warnings);
                }
            }
            Item::FnDecl(f) => {
                for stmt in &f.body {
                    collect_resolve_ids_stmt(&stmt.node, &publish_ids, warnings);
                }
            }
            Item::Service(service) => {
                for si in &service.items {
                    match &si.node {
                        ServiceItem::Command(cmd) => match &cmd.body {
                            CommandBody::Block(stmts) => {
                                for stmt in stmts {
                                    collect_resolve_ids_stmt(&stmt.node, &publish_ids, warnings);
                                }
                            }
                            CommandBody::ViaSettle { via_expr, .. } => {
                                collect_resolve_ids_expr(&via_expr.node, &publish_ids, warnings);
                            }
                        },
                        ServiceItem::Query(q) => match &q.body {
                            QueryBody::Block(stmts) => {
                                for stmt in stmts {
                                    collect_resolve_ids_stmt(&stmt.node, &publish_ids, warnings);
                                }
                            }
                            QueryBody::ViaObserve { via_expr, return_expr, .. } => {
                                collect_resolve_ids_expr(&via_expr.node, &publish_ids, warnings);
                                collect_resolve_ids_expr(&return_expr.node, &publish_ids, warnings);
                            }
                        },
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

fn collect_resolve_ids_stmt(stmt: &Stmt, publish_ids: &HashSet<String>, warnings: &mut Vec<TypeWarning>) {
    match stmt {
        Stmt::Let { value, .. } => collect_resolve_ids_expr(&value.node, publish_ids, warnings),
        Stmt::Return(Some(expr)) => collect_resolve_ids_expr(&expr.node, publish_ids, warnings),
        Stmt::Expr(expr) => collect_resolve_ids_expr(&expr.node, publish_ids, warnings),
        Stmt::If { condition, then_block, else_block } => {
            collect_resolve_ids_expr(&condition.node, publish_ids, warnings);
            for s in then_block { collect_resolve_ids_stmt(&s.node, publish_ids, warnings); }
            if let Some(eb) = else_block {
                for s in eb { collect_resolve_ids_stmt(&s.node, publish_ids, warnings); }
            }
        }
        Stmt::For { iterable, body, .. } => {
            collect_resolve_ids_expr(&iterable.node, publish_ids, warnings);
            for s in body { collect_resolve_ids_stmt(&s.node, publish_ids, warnings); }
        }
        Stmt::While { condition, body } => {
            collect_resolve_ids_expr(&condition.node, publish_ids, warnings);
            for s in body { collect_resolve_ids_stmt(&s.node, publish_ids, warnings); }
        }
        Stmt::Match { expr, arms } => {
            collect_resolve_ids_expr(&expr.node, publish_ids, warnings);
            for arm in arms {
                if let Some(ref guard) = arm.guard {
                    collect_resolve_ids_expr(&guard.node, publish_ids, warnings);
                }
                for s in &arm.body { collect_resolve_ids_stmt(&s.node, publish_ids, warnings); }
            }
        }
        _ => {}
    }
}

fn collect_resolve_ids_expr(expr: &Expr, publish_ids: &HashSet<String>, warnings: &mut Vec<TypeWarning>) {
    match expr {
        Expr::Resolve { port, name, .. } => {
            // Only validate string literal service IDs
            if let Expr::Literal(Literal::String(id)) = &name.node {
                if !publish_ids.contains(id) {
                    warnings.push(TypeWarning {
                        message: format!(
                            "resolve {}[\"{}\"] references service ID not declared in this program; \
                             ensure the service is available at runtime",
                            port, id
                        ),
                        span: None,
                    });
                }
            }
        }
        Expr::FnCall { func, args } => {
            collect_resolve_ids_expr(&func.node, publish_ids, warnings);
            for a in args { collect_resolve_ids_expr(&a.node, publish_ids, warnings); }
        }
        Expr::MethodCall { receiver, args, .. } => {
            collect_resolve_ids_expr(&receiver.node, publish_ids, warnings);
            for a in args { collect_resolve_ids_expr(&a.node, publish_ids, warnings); }
        }
        Expr::Await { expr } => collect_resolve_ids_expr(&expr.node, publish_ids, warnings),
        Expr::Binary { left, right, .. } => {
            collect_resolve_ids_expr(&left.node, publish_ids, warnings);
            collect_resolve_ids_expr(&right.node, publish_ids, warnings);
        }
        Expr::Block(stmts) => {
            for s in stmts { collect_resolve_ids_stmt(&s.node, publish_ids, warnings); }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::parse;

    fn check(source: &str) -> TypeCheckResult {
        let tokens = tokenize(source).expect("tokenize failed");
        let (program, errors) = parse(source, &tokens);
        assert!(errors.is_empty(), "parse errors: {:?}", errors);
        check_program(&program)
    }

    #[test]
    fn test_command_with_block_body_is_allowed() {
        // Block body commands manage substrate interactions directly
        // and rely on guarantee laws for invariant enforcement.
        let result = check(
            r#"port KV {
                put(key: String, value: String) -> String [command]
            }
            service MainKV provides KV {
                publish as "kv/main"
                command put(key: String, value: String) -> String {
                    return "done"
                }
            }"#,
        );
        assert!(
            result.errors.is_empty(),
            "block body commands should be allowed, got: {:?}",
            result.errors
        );
    }

    #[test]
    fn test_query_without_observe_is_error() {
        // Services WITH components must declare observation semantics
        let result = check(
            r#"substrate ReplicatedLog<T> {
                op append(entry: T) -> String
                op read(offset: Int) -> T
                emits {
                    append_accepted
                    quorum_committed
                }
            }
            port KV {
                get(key: String) -> String? [query]
            }
            service MainKV provides KV {
                publish as "kv/main"
                component log = spawn ReplicatedLog("log/main")
                query get(key: String) -> String? {
                    return none
                }
            }"#,
        );
        assert!(
            !result.errors.is_empty(),
            "expected error for query without observe"
        );
        assert!(result.errors[0].message.contains("observation semantics"));
    }

    #[test]
    fn test_via_settle_command_passes() {
        let result = check(
            r#"substrate ReplicatedLog<T> {
                op append(entry: T) -> String
                op read(offset: Int) -> T
                emits {
                    append_accepted
                    quorum_committed
                }
            }
            port Log {
                append(data: String) -> String [command]
            }
            service SimpleLog provides Log {
                publish as "log/main"
                component log = spawn ReplicatedLog("log/main")
                command append(data: String) -> String
                    via log.append(data)
                    settle log.quorum_committed by ack.key
            }"#,
        );
        assert!(
            result.errors.is_empty(),
            "expected no errors, got: {:?}",
            result
                .errors
                .iter()
                .map(|e| &e.message)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_via_settle_invalid_component_is_error() {
        let result = check(
            r#"port Log {
                append(data: String) -> String [command]
            }
            service SimpleLog provides Log {
                publish as "log/main"
                command append(data: String) -> String
                    via resolve Log["log/main"].append(data)
                    settle nonexistent.quorum_committed by ack.key
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("not a declared component")),
            "expected component error, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_via_settle_invalid_event_is_error() {
        let result = check(
            r#"substrate ReplicatedLog<T> {
                op append(entry: T) -> String
                op read(offset: Int) -> T
                emits {
                    append_accepted
                    quorum_committed
                }
            }
            port Log {
                append(data: String) -> String [command]
            }
            service SimpleLog provides Log {
                publish as "log/main"
                component log = spawn ReplicatedLog("log/main")
                command append(data: String) -> String
                    via log.append(data)
                    settle log.nonexistent_event by ack.key
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("does not emit")),
            "expected emit error, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_via_observe_query_passes() {
        let result = check(
            r#"substrate KeyValueStore<K, V> {
                op put(key: K, value: V) -> String
                op get(key: K) -> V
                emits {
                    put_committed
                    entry_available
                }
            }
            port Reader {
                get(key: String) -> String? [query]
            }
            service KVReader provides Reader {
                publish as "kv/reader"
                component store = spawn KeyValueStore("kv/data")
                query get(key: String) -> String?
                    via store.get(key)
                    observe store.entry_available by res.frontier
                    return res.value
            }"#,
        );
        assert!(
            result.errors.is_empty(),
            "expected no errors, got: {:?}",
            result
                .errors
                .iter()
                .map(|e| &e.message)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_via_observe_invalid_component_is_error() {
        let result = check(
            r#"port Reader {
                get(key: String) -> String? [query]
            }
            service KVReader provides Reader {
                publish as "kv/reader"
                query get(key: String) -> String?
                    via resolve Reader["kv/reader"].get(key)
                    observe nonexistent.entry_available by res.frontier
                    return res.value
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("not a declared component")),
            "expected component error, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_query_with_visible_annotation_passes() {
        // Service WITH components + visible annotation should pass
        let result = check(
            r#"substrate ReplicatedLog<T> {
                op append(entry: T) -> String
                op read(offset: Int) -> T
                emits {
                    append_accepted
                    quorum_committed
                }
            }
            port KV {
                get(key: String) -> String? [query, visible(quorum_committed)]
            }
            service MainKV provides KV {
                publish as "kv/main"
                component log = spawn ReplicatedLog("log/main")
                query get(key: String) -> String? {
                    return none
                }
            }"#,
        );
        assert!(
            result.errors.is_empty(),
            "expected no errors for visible() query, got: {:?}",
            result
                .errors
                .iter()
                .map(|e| &e.message)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_port_method_without_annotation_warns() {
        let result = check(
            r#"port Hello {
                greet() -> String
            }"#,
        );
        assert!(!result.warnings.is_empty());
        assert!(result.warnings[0].message.contains("no [command] or [query]"));
    }

    #[test]
    fn test_cap_without_qualifier_is_error() {
        let result = check(
            r#"entry(out: cap Stdout) {
                return none
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("authority qualifier")),
            "expected authority qualifier error, got errors: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_cap_with_delegate_no_qualifier_warning() {
        let result = check(
            r#"entry(out: cap Stdout @delegate) {
                return none
            }"#,
        );
        assert!(
            !result.warnings.iter().any(|w| w.message.contains("authority qualifier")),
            "expected no authority qualifier warning with @delegate, got: {:?}",
            result.warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_entry_value_type_param_warns() {
        let result = check(
            r#"entry(name: String) {
                return name
            }"#,
        );
        assert!(!result.warnings.is_empty());
        assert!(result.warnings[0].message.contains("value type"));
    }

    #[test]
    fn test_inference_catches_type_mismatch() {
        let result = check(
            r#"fn add(a: Int, b: Int) -> Int {
                return a + b
            }
            entry() {
                let x = add(1, 2)
                let y = add("hello", "world")
                return x
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("type mismatch")),
            "expected type mismatch error from inference, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_inference_undefined_variable() {
        let result = check(
            r#"entry() {
                return x
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("undefined variable")),
            "expected undefined variable error, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_inference_fn_arity_mismatch() {
        let result = check(
            r#"fn add(a: Int, b: Int) -> Int {
                return a + b
            }
            entry() {
                let x = add(1, 2, 3)
                return x
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("expects 2 args")),
            "expected arity error from inference, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_dynamic_import_warning_emitted() {
        let result = check(
            r#"entry() {
                let m = import("helpers/math.bl")
                return m
            }"#,
        );
        assert!(
            result.warnings.iter().any(|w| w.message.contains("dynamic import types are not statically checked")),
            "expected dynamic import warning, got: {:?}",
            result.warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_dynamic_import_no_undefined_variable_error() {
        // After importing dynamically, the binding should be available (type Any)
        let result = check(
            r#"entry() {
                let m = import("helpers/math.bl")
                return m
            }"#,
        );
        assert!(
            !result.errors.iter().any(|e| e.message.contains("undefined variable")),
            "should not have undefined variable error for dynamic import binding, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_dynamic_import_type_is_unit() {
        // Dynamic import returns Unit (merges fns into scope, returns nothing useful)
        let result = check(
            r#"entry() {
                let m = import("helpers/math.bl")
                return m
            }"#,
        );
        // Should not produce errors about import itself — Unit is a valid return
        assert!(
            !result.errors.iter().any(|e| e.message.contains("import")),
            "dynamic import should not produce import-specific errors, got errors: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_param_arity_mismatch() {
        let result = check(
            r#"substrate ReplicatedLog<T> {
                op append(entry: T) -> String
                op read(offset: Int) -> T
                emits {
                    append_accepted
                    quorum_committed
                }
            }
            port KV {
                put(key: String, value: String) -> String [command]
            }
            service MainKV provides KV {
                publish as "kv/main"
                component log = spawn ReplicatedLog("log/main")
                command put(key: String) -> String
                    via log.append(key)
                    settle log.quorum_committed by ack.key
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("params")),
            "expected arity error, got: {:?}",
            result
                .errors
                .iter()
                .map(|e| &e.message)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_guarantee_valid_event_types() {
        let result = check(
            r#"substrate ReplicatedLog<T> {
                op append(entry: T) -> String
                emits {
                    append_accepted
                    quorum_committed
                }
            }
            guarantee commit_consistency {
                law: append_accepted(key) => quorum_committed(key)
            }"#,
        );
        assert!(
            !result.errors.iter().any(|e| e.message.contains("not emitted")),
            "expected no guarantee errors, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_guarantee_invalid_event_type() {
        let result = check(
            r#"substrate ReplicatedLog<T> {
                op append(entry: T) -> String
                emits {
                    append_accepted
                    quorum_committed
                }
            }
            guarantee bad_guarantee {
                law: nonexistent_event(key) => quorum_committed(key)
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("nonexistent_event") && e.message.contains("not emitted")),
            "expected guarantee event type error, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_guarantee_mixed_valid_invalid() {
        let result = check(
            r#"substrate ReplicatedLog<T> {
                op append(entry: T) -> String
                emits {
                    append_accepted
                    quorum_committed
                }
            }
            guarantee mixed {
                law: append_accepted(key) => fake_event(key)
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("fake_event")),
            "expected error for fake_event, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        // append_accepted should NOT be reported as invalid (only fake_event should be)
        assert!(
            !result.errors.iter().any(|e| e.message.contains("event type 'append_accepted'")),
            "should not error on valid event type"
        );
    }

    #[test]
    fn test_pure_fn_with_resolve_is_error() {
        let result = check(
            r#"port KV {
                get(key: String) -> String [query]
            }
            pure fn bad() -> String {
                let kv = resolve KV["kv/main"]
                return "fail"
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("pure function") && e.message.contains("capability resolution")),
            "expected purity error, got errors: {:?}, warnings: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>(),
            result.warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_pure_fn_with_await_is_error() {
        let result = check(
            r#"pure fn bad2() -> Int {
                let x = await 42
                return x
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("pure function") && e.message.contains("await")),
            "expected purity error for await, got errors: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_pure_fn_without_side_effects_ok() {
        let result = check(
            r#"pure fn good() -> Int {
                return 1 + 2
            }"#,
        );
        assert!(
            !result.errors.iter().any(|e| e.message.contains("pure function")),
            "expected no purity errors, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_generic_type_correct_arity() {
        let result = check(
            r#"type Container<T> {
                item: T
            }
            fn make() -> Container<Int> {
                Container { item: 42 }
            }"#,
        );
        assert!(
            !result.errors.iter().any(|e| e.message.contains("type parameter")),
            "expected no arity error, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_generic_type_wrong_arity() {
        let result = check(
            r#"type Container<T> {
                item: T
            }
            fn make() -> Container<Int, String> {
                Container { item: 42 }
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("type parameter")),
            "expected arity error for Container<Int, String>, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_generic_type_no_args_ok() {
        // Using a generic type without args is allowed (treated as raw type)
        let result = check(
            r#"type Container<T> {
                item: T
            }
            fn make() -> Container {
                Container { item: 42 }
            }"#,
        );
        assert!(
            !result.errors.iter().any(|e| e.message.contains("type parameter")),
            "expected no arity error when using Container without args, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // === Gap 7: @delegate required for replicated service cap params ===

    #[test]
    fn test_unqualified_cap_on_replicated_service_is_error() {
        let result = check(
            r#"port KV {
                put(key: String, value: String) -> String [command]
            }
            substrate ReplicatedLog<T> {
                op append(entry: T) -> String
                emits { quorum_committed }
            }
            service RepKV provides KV {
                publish as "kv/rep"
                on replicated(3)
                component log = spawn ReplicatedLog("log/main")
                command put(key: String, value: String) -> String
                    via log.append(key)
                    settle log.quorum_committed by ack.key
            }"#,
        );
        // No cap params in this service, so no error expected
        assert!(
            !result.errors.iter().any(|e| e.message.contains("requires @delegate")),
            "expected no @delegate error when no cap params exist"
        );
    }

    #[test]
    fn test_unqualified_cap_on_local_service_is_error() {
        let result = check(
            r#"port KV {
                put(key: String, value: cap KV) -> String [command]
            }
            substrate ReplicatedLog<T> {
                op append(entry: T) -> String
                emits { quorum_committed }
            }
            service LocalKV provides KV {
                publish as "kv/local"
                component log = spawn ReplicatedLog("log/main")
                command put(key: String, value: cap KV) -> String
                    via log.append(key)
                    settle log.quorum_committed by ack.key
            }"#,
        );
        // Should now be an error (bare cap requires authority qualifier)
        assert!(
            result.errors.iter().any(|e| e.message.contains("authority qualifier")),
            "expected authority qualifier error, got errors: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // === Gap 8: visible() annotation validation ===

    #[test]
    fn test_visible_valid_event_type_passes() {
        let result = check(
            r#"substrate KeyValueStore<K, V> {
                op put(key: K, value: V) -> String
                op get(key: K) -> V
                emits { put_committed  entry_available }
            }
            port Reader {
                get(key: String) -> String? [query, visible(entry_available)]
            }
            service KVReader provides Reader {
                publish as "kv/reader"
                component store = spawn KeyValueStore("kv/data")
                query get(key: String) -> String? {
                    return none
                }
            }"#,
        );
        assert!(
            !result.errors.iter().any(|e| e.message.contains("visible")),
            "expected no visible errors, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_visible_invalid_event_type_is_error() {
        let result = check(
            r#"substrate KeyValueStore<K, V> {
                op put(key: K, value: V) -> String
                op get(key: K) -> V
                emits { put_committed  entry_available }
            }
            port Reader {
                get(key: String) -> String? [query, visible(nonexistent)]
            }
            service KVReader provides Reader {
                publish as "kv/reader"
                component store = spawn KeyValueStore("kv/data")
                query get(key: String) -> String? {
                    return none
                }
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("visible") && e.message.contains("nonexistent")),
            "expected visible event type error, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // === Gap 9: Service dependency cycle detection ===

    #[test]
    fn test_service_cycle_detected() {
        let result = check(
            r#"port PA {
                do_a() -> String [command]
            }
            port PB {
                do_b() -> String [command]
            }
            substrate ReplicatedLog<T> {
                op append(entry: T) -> String
                emits { quorum_committed }
            }
            service SvcA provides PA {
                publish as "svc/a"
                component log = spawn ReplicatedLog("log/a")
                command do_a() -> String
                    via resolve PB["svc/b"].do_b()
                    settle log.quorum_committed by ack.key
            }
            service SvcB provides PB {
                publish as "svc/b"
                component log = spawn ReplicatedLog("log/b")
                command do_b() -> String
                    via resolve PA["svc/a"].do_a()
                    settle log.quorum_committed by ack.key
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("circular service dependency")),
            "expected cycle detection error, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_service_dag_no_cycle() {
        let result = check(
            r#"port PA {
                do_a() -> String [command]
            }
            port PB {
                do_b() -> String [command]
            }
            substrate ReplicatedLog<T> {
                op append(entry: T) -> String
                emits { quorum_committed }
            }
            service SvcA provides PA {
                publish as "svc/a"
                component log = spawn ReplicatedLog("log/a")
                command do_a() -> String
                    via resolve PB["svc/b"].do_b()
                    settle log.quorum_committed by ack.key
            }
            service SvcB provides PB {
                publish as "svc/b"
                component log = spawn ReplicatedLog("log/b")
                command do_b() -> String
                    via log.append("data")
                    settle log.quorum_committed by ack.key
            }"#,
        );
        assert!(
            !result.errors.iter().any(|e| e.message.contains("circular")),
            "expected no cycle error in DAG, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // === Gap 12: Profile reference validation ===

    #[test]
    fn test_profile_reference_valid() {
        let result = check(
            r#"port KV {
                get(key: String) -> String? [query]
            }
            profile Production {
                preferred_transport: "tcp"
            }
            entry() {
                let kv = resolve KV["kv/main"] with profile Production
                return none
            }"#,
        );
        assert!(
            !result.errors.iter().any(|e| e.message.contains("unknown profile")),
            "expected no profile error, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_profile_reference_invalid() {
        let result = check(
            r#"port KV {
                get(key: String) -> String? [query]
            }
            profile Production {
                preferred_transport: "tcp"
            }
            entry() {
                let kv = resolve KV["kv/main"] with profile Typo
                return none
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("unknown profile") && e.message.contains("Typo")),
            "expected unknown profile error, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // === Gap 14: Dynamic import path validation ===

    #[test]
    fn test_dynamic_import_string_literal_path() {
        let result = check(
            r#"entry() {
                let m = import("helpers/math.bl")
                return m
            }"#,
        );
        // String literal path: should get standard dynamic import warning, NOT the path warning
        assert!(
            !result.warnings.iter().any(|w| w.message.contains("should be a string literal")),
            "string literal import should not get string literal warning, got: {:?}",
            result.warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_dynamic_import_variable_path_warns() {
        let result = check(
            r#"entry() {
                let path = "helpers/math.bl"
                let m = import(path)
                return m
            }"#,
        );
        assert!(
            result.warnings.iter().any(|w| w.message.contains("should be a string literal")),
            "expected string literal warning for variable path, got: {:?}",
            result.warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    // === Gap 4: Pure function enforcement holes ===

    #[test]
    fn test_pure_fn_with_concurrent_is_error() {
        let result = check(
            r#"pure fn bad() -> Int {
                let results = concurrent {
                    42
                }
                return 1
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("pure function") && e.message.contains("concurrent")),
            "expected purity error for concurrent, got errors: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_pure_fn_with_closure_resolve_is_error() {
        let result = check(
            r#"port KV {
                get(key: String) -> String [query]
            }
            pure fn bad() -> Int {
                let f = |x: Int| {
                    let kv = resolve KV["kv/main"]
                    return x
                }
                return 1
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("pure function") && e.message.contains("capability resolution")),
            "expected purity error for resolve inside closure, got errors: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_pure_fn_with_dynamic_import_is_error() {
        let result = check(
            r#"pure fn bad() -> Int {
                let m = import("helpers/math.bl")
                return 1
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("pure function") && e.message.contains("import")),
            "expected purity error for dynamic import, got errors: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_pure_fn_with_match_resolve_is_error() {
        let result = check(
            r#"port KV {
                get(key: String) -> String [query]
            }
            pure fn bad(x: Int) -> Int {
                let result = match x {
                    1 => {
                        let kv = resolve KV["kv/main"]
                        1
                    }
                    _ => 0
                }
                return result
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("pure function") && e.message.contains("capability resolution")),
            "expected purity error for resolve inside match, got errors: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    // === Gap 7-warning: Closed-world resolve check ===

    #[test]
    fn test_resolve_missing_publish_id_warns() {
        let result = check(
            r#"port KV {
                get(key: String) -> String? [query]
            }
            service MainKV provides KV {
                publish as "kv/main"
                query get(key: String) -> String? {
                    return none
                }
            }
            entry() {
                let kv = resolve KV["kv/other"]
                return none
            }"#,
        );
        assert!(
            result.warnings.iter().any(|w| w.message.contains("kv/other") && w.message.contains("not declared")),
            "expected warning for undeclared publish ID, got warnings: {:?}",
            result.warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    // === Gap 6: Duplicate profile detection ===

    #[test]
    fn test_duplicate_profile_is_error() {
        let result = check(
            r#"profile Fast {
                preferred_transport: "tcp"
            }
            profile Fast {
                preferred_transport: "quic"
            }
            entry() {
                return none
            }"#,
        );
        assert!(
            result.errors.iter().any(|e| e.message.contains("duplicate profile") && e.message.contains("Fast")),
            "expected duplicate profile error, got errors: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_single_profile_ok() {
        let result = check(
            r#"profile Fast {
                preferred_transport: "tcp"
            }
            entry() {
                return none
            }"#,
        );
        assert!(
            !result.errors.iter().any(|e| e.message.contains("duplicate profile")),
            "expected no duplicate profile error, got: {:?}",
            result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_resolve_valid_publish_id_no_warning() {
        let result = check(
            r#"port KV {
                get(key: String) -> String? [query]
            }
            service MainKV provides KV {
                publish as "kv/main"
                query get(key: String) -> String? {
                    return none
                }
            }
            entry() {
                let kv = resolve KV["kv/main"]
                return none
            }"#,
        );
        assert!(
            !result.warnings.iter().any(|w| w.message.contains("not declared")),
            "should not warn for valid publish ID, got warnings: {:?}",
            result.warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_serialization_boundary_warning_borrow_cap_to_service() {
        // Passing @borrow cap as argument to a service method call should warn
        let result = check(
            r#"
            port KV {
                put(key: String, val: String) -> String [command]
            }
            port Logger {
                log(msg: String) -> String [query]
            }
            service MyKV provides KV {
                publish as "kv/main"
                command put(key: String, val: String) -> String
                    via kv.append(key)
                    settle kv.quorum_committed by ack.key
            }
            entry main(kv: cap KV @delegate, logger: cap Logger @borrow) {
                await kv.put("key", logger)
            }
            "#,
        );
        assert!(
            result.warnings.iter().any(|w| w.message.contains("@borrow") && w.message.contains("service boundaries")),
            "expected serialization boundary warning for @borrow cap, got warnings: {:?}",
            result.warnings.iter().map(|w| &w.message).collect::<Vec<_>>()
        );
    }
}
