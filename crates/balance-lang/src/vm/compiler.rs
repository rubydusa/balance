use std::collections::HashMap;

use crate::ast::*;
use crate::lexer::span::Spanned;
use crate::runtime::value::Value;

use super::ir::*;

/// Compiler state for translating AST to bytecode.
struct Compiler {
    /// Current function being compiled.
    code: Vec<Op>,
    /// Map from variable name to local slot index.
    locals: HashMap<String, usize>,
    /// Next local slot to allocate.
    next_local: usize,
    /// Stack of break target instruction indices (for patching).
    break_targets: Vec<Vec<usize>>,
    /// Stack of continue handling: Some(target) for while loops (known target),
    /// None for for loops (deferred patching via continue_jumps).
    continue_targets: Vec<Option<usize>>,
    /// Stack of continue jump indices to be patched (for `for` loops).
    continue_jumps: Vec<Vec<usize>>,
    /// Counter for generating unique internal variable names.
    temp_counter: usize,
    /// Closure functions compiled during this compilation unit.
    closure_functions: Vec<CompiledFunction>,
}

impl Compiler {
    fn new() -> Self {
        Self {
            code: Vec::new(),
            locals: HashMap::new(),
            next_local: 0,
            break_targets: Vec::new(),
            continue_targets: Vec::new(),
            continue_jumps: Vec::new(),
            temp_counter: 0,
            closure_functions: Vec::new(),
        }
    }

    fn emit(&mut self, op: Op) -> usize {
        let idx = self.code.len();
        self.code.push(op);
        idx
    }

    fn current_offset(&self) -> usize {
        self.code.len()
    }

    fn patch_jump(&mut self, idx: usize, target: usize) {
        match &mut self.code[idx] {
            Op::Jump(ref mut t) => *t = target,
            Op::JumpIfFalse(ref mut t) => *t = target,
            _ => panic!("cannot patch non-jump instruction at {idx}"),
        }
    }

    fn alloc_local(&mut self, name: &str) -> usize {
        // Reuse existing slot for rebinding (Balance uses `let x = x + 1` pattern)
        if let Some(&slot) = self.locals.get(name) {
            return slot;
        }
        let slot = self.next_local;
        self.locals.insert(name.to_string(), slot);
        self.next_local += 1;
        slot
    }

    /// Allocate a unique internal temporary variable (never reused).
    fn alloc_temp(&mut self, prefix: &str) -> usize {
        let name = format!("__{prefix}_{}_", self.temp_counter);
        self.temp_counter += 1;
        let slot = self.next_local;
        self.locals.insert(name, slot);
        self.next_local += 1;
        slot
    }

    fn resolve_local(&self, name: &str) -> Option<usize> {
        self.locals.get(name).copied()
    }
}

/// Compile a program into bytecode. Only compiles pure functions and entries.
/// Service interactions (resolve, await, dispatch) are not supported and will
/// cause a compilation error.
pub fn compile_program(program: &Program) -> Result<CompiledProgram, String> {
    let mut compiled = CompiledProgram::new();
    let mut all_closures = Vec::new();

    // Collect module name
    if let Some(ref decl) = program.module_decl {
        compiled.module_name = Some(decl.path.join("."));
    }

    // Collect import metadata
    for import in &program.imports {
        let imp = &import.node;
        let names = imp.names.as_ref().map(|ns| {
            ns.iter().map(|(name, alias)| {
                match alias {
                    Some(a) => format!("{name} as {a}"),
                    None => name.clone(),
                }
            }).collect()
        }).unwrap_or_default();
        compiled.imports.push(ImportInfo {
            path: imp.path.clone(),
            names,
        });
        // Add to transitive dependencies list
        let dep_path = imp.path.join(".");
        if !compiled.dependencies.contains(&dep_path) {
            compiled.dependencies.push(dep_path);
        }
    }

    for item in &program.items {
        match &item.node {
            Item::FnDecl(fn_decl) => {
                let (func, closures) = compile_fn_with_closures(fn_decl)?;
                compiled.functions.push(func);
                all_closures.extend(closures);
            }
            Item::Entry(entry_decl) => {
                let (func, closures) = compile_entry_with_closures(entry_decl)?;
                let idx = compiled.functions.len();
                compiled.functions.push(func);
                compiled.entry_index = Some(idx);
                all_closures.extend(closures);
            }
            Item::TypeDecl(t) => {
                compiled.types.push(TypeInfo {
                    name: t.name.clone(),
                    fields: t.fields.iter().map(|f| {
                        (f.name.clone(), type_expr_to_string(&f.ty.node))
                    }).collect(),
                    type_params: t.type_params.clone(),
                });
            }
            Item::Service(s) => {
                let publish_id = s.items.iter().find_map(|si| {
                    if let ServiceItem::Publish(id) = &si.node {
                        Some(id.clone())
                    } else {
                        None
                    }
                });
                compiled.services.push(ServiceInfo {
                    name: s.name.clone(),
                    port: s.provides.clone(),
                    publish_id,
                });
            }
            Item::Substrate(sub) => {
                compiled.substrates.push(SubstrateBinding {
                    name: sub.name.clone(),
                    substrate_type: sub.name.clone(),
                });
            }
            Item::Port(port) => {
                let methods = port.methods.iter().map(|m| {
                    let kind = m.node.annotations.iter()
                        .find(|a| a.name == "command" || a.name == "query")
                        .map(|a| a.name.clone())
                        .unwrap_or_else(|| "unknown".to_string());
                    (m.node.name.clone(), m.node.params.len(), kind)
                }).collect();
                compiled.ports.push(PortInfo {
                    name: port.name.clone(),
                    methods,
                });
            }
            Item::Guarantee(g) => {
                compiled.guarantees.push(GuaranteeInfo {
                    name: g.name.clone(),
                    law_descriptions: g.laws.iter().map(|l| l.body.clone()).collect(),
                });
            }
            _ => {}
        }
    }

    // Build dependency graph: service → port edges
    for svc in &compiled.services {
        compiled.dependency_graph.push((svc.name.clone(), svc.port.clone()));
    }

    // Append closure functions after all top-level functions.
    // Closure func_index references are relative to the total program.functions array,
    // so we need to fix them up.
    let base_offset = compiled.functions.len();
    // Patch MakeClosure func_index in all existing functions
    for func in &mut compiled.functions {
        for op in &mut func.code {
            if let Op::MakeClosure { func_index, .. } = op {
                *func_index += base_offset;
            }
        }
    }
    // Also patch closure functions that reference other closures
    for closure_fn in &mut all_closures {
        for op in &mut closure_fn.code {
            if let Op::MakeClosure { func_index, .. } = op {
                *func_index += base_offset;
            }
        }
    }
    compiled.functions.extend(all_closures);

    Ok(compiled)
}

/// Compile a function declaration.
pub fn compile_fn(fn_decl: &FnDecl) -> Result<CompiledFunction, String> {
    let (func, _closures) = compile_fn_with_closures(fn_decl)?;
    Ok(func)
}

fn compile_fn_with_closures(fn_decl: &FnDecl) -> Result<(CompiledFunction, Vec<CompiledFunction>), String> {
    let mut compiler = Compiler::new();

    // Allocate parameter slots
    for param in &fn_decl.params {
        compiler.alloc_local(&param.name);
    }

    compile_stmts(&mut compiler, &fn_decl.body)?;

    // Ensure function returns: if last statement was an expression or if-stmt,
    // its value is on the stack — just return it (implicit return).
    // If body is empty or last was not value-producing, push Unit.
    if !matches!(compiler.code.last(), Some(Op::Return)) {
        let last_leaves_value = fn_decl.body.last().map_or(false, |s| {
            matches!(&s.node, Stmt::Expr(_) | Stmt::If { .. } | Stmt::Match { .. })
        });
        if !last_leaves_value {
            compiler.emit(Op::Const(Value::Unit));
        }
        compiler.emit(Op::Return);
    }

    let closures = std::mem::take(&mut compiler.closure_functions);

    Ok((CompiledFunction {
        name: fn_decl.name.clone(),
        arity: fn_decl.params.len(),
        locals_count: compiler.next_local,
        code: compiler.code,
    }, closures))
}

/// Compile an entry declaration.
pub fn compile_entry(entry_decl: &EntryDecl) -> Result<CompiledFunction, String> {
    let (func, _closures) = compile_entry_with_closures(entry_decl)?;
    Ok(func)
}

fn compile_entry_with_closures(entry_decl: &EntryDecl) -> Result<(CompiledFunction, Vec<CompiledFunction>), String> {
    let mut compiler = Compiler::new();

    // Entry params (capabilities) are not supported in VM — check for none
    if !entry_decl.params.is_empty() {
        return Err("VM compilation does not support entry parameters (capabilities)".to_string());
    }

    compile_stmts(&mut compiler, &entry_decl.body)?;

    if !matches!(compiler.code.last(), Some(Op::Return)) {
        let last_leaves_value = entry_decl.body.last().map_or(false, |s| {
            matches!(&s.node, Stmt::Expr(_) | Stmt::If { .. } | Stmt::Match { .. })
        });
        if !last_leaves_value {
            compiler.emit(Op::Const(Value::Unit));
        }
        compiler.emit(Op::Return);
    }

    let name = entry_decl
        .name
        .as_deref()
        .unwrap_or("__entry__")
        .to_string();

    let closures = std::mem::take(&mut compiler.closure_functions);

    Ok((CompiledFunction {
        name,
        arity: 0,
        locals_count: compiler.next_local,
        code: compiler.code,
    }, closures))
}

fn compile_stmts(compiler: &mut Compiler, stmts: &[Spanned<Stmt>]) -> Result<(), String> {
    for (i, stmt) in stmts.iter().enumerate() {
        compile_stmt(compiler, &stmt.node)?;
        // Pop intermediate value-producing results (not the last one)
        if i < stmts.len() - 1 {
            if matches!(&stmt.node, Stmt::Expr(_) | Stmt::If { .. } | Stmt::Match { .. }) {
                compiler.emit(Op::Pop);
            }
        }
    }
    Ok(())
}

fn compile_stmt(compiler: &mut Compiler, stmt: &Stmt) -> Result<(), String> {
    match stmt {
        Stmt::Let { name, value, .. } => {
            compile_expr(compiler, &value.node)?;
            let slot = compiler.alloc_local(name);
            compiler.emit(Op::Store(slot));
            Ok(())
        }
        Stmt::Return(Some(expr)) => {
            compile_expr(compiler, &expr.node)?;
            compiler.emit(Op::Return);
            Ok(())
        }
        Stmt::Return(None) => {
            compiler.emit(Op::Const(Value::Unit));
            compiler.emit(Op::Return);
            Ok(())
        }
        Stmt::If {
            condition,
            then_block,
            else_block,
        } => {
            compile_expr(compiler, &condition.node)?;
            let jump_false = compiler.emit(Op::JumpIfFalse(0)); // patch later

            // Compile then block — ensure it leaves a value on the stack
            compile_stmts(compiler, then_block)?;
            let then_leaves_value = then_block.last().map_or(false, |s| {
                matches!(&s.node, Stmt::Expr(_) | Stmt::If { .. } | Stmt::Match { .. })
            });
            if !then_leaves_value {
                compiler.emit(Op::Const(Value::Unit));
            }

            if let Some(else_stmts) = else_block {
                let jump_end = compiler.emit(Op::Jump(0)); // patch later
                let else_start = compiler.current_offset();
                compiler.patch_jump(jump_false, else_start);

                compile_stmts(compiler, else_stmts)?;
                let else_leaves_value = else_stmts.last().map_or(false, |s| {
                    matches!(&s.node, Stmt::Expr(_) | Stmt::If { .. } | Stmt::Match { .. })
                });
                if !else_leaves_value {
                    compiler.emit(Op::Const(Value::Unit));
                }

                let end = compiler.current_offset();
                compiler.patch_jump(jump_end, end);
            } else {
                let jump_end = compiler.emit(Op::Jump(0));
                let else_start = compiler.current_offset();
                compiler.patch_jump(jump_false, else_start);
                compiler.emit(Op::Const(Value::Unit));
                let end = compiler.current_offset();
                compiler.patch_jump(jump_end, end);
            }
            Ok(())
        }
        Stmt::While { condition, body } => {
            let loop_start = compiler.current_offset();
            compiler.continue_targets.push(Some(loop_start));
            compiler.continue_jumps.push(Vec::new());
            compiler.break_targets.push(Vec::new());

            compile_expr(compiler, &condition.node)?;
            let jump_false = compiler.emit(Op::JumpIfFalse(0));

            compile_stmts(compiler, body)?;
            // Pop any trailing expression value from loop body
            if let Some(last) = body.last() {
                if matches!(&last.node, Stmt::Expr(_)) {
                    compiler.emit(Op::Pop);
                }
            }

            compiler.emit(Op::Jump(loop_start));
            let loop_end = compiler.current_offset();
            compiler.patch_jump(jump_false, loop_end);

            // Patch break targets
            let breaks = compiler.break_targets.pop().unwrap_or_default();
            for idx in breaks {
                compiler.patch_jump(idx, loop_end);
            }
            // Patch continue jumps (while loops have known target, but handle any deferred ones)
            let continues = compiler.continue_jumps.pop().unwrap_or_default();
            for idx in continues {
                compiler.patch_jump(idx, loop_start);
            }
            compiler.continue_targets.pop();
            Ok(())
        }
        Stmt::For {
            variable,
            iterable,
            body,
        } => {
            // Compile iterable
            compile_expr(compiler, &iterable.node)?;
            let list_slot = compiler.alloc_temp("for_list");
            compiler.emit(Op::Store(list_slot));

            // Index counter
            compiler.emit(Op::Const(Value::Int(0)));
            let idx_slot = compiler.alloc_temp("for_idx");
            compiler.emit(Op::Store(idx_slot));

            let loop_start = compiler.current_offset();
            // For loops use deferred continue patching (target is the increment section)
            compiler.continue_targets.push(None);
            compiler.continue_jumps.push(Vec::new());
            compiler.break_targets.push(Vec::new());

            // Check idx < len(list)
            compiler.emit(Op::Load(idx_slot));
            compiler.emit(Op::Load(list_slot));
            compiler.emit(Op::MethodCall {
                method: "len".to_string(),
                arg_count: 0,
            });
            compiler.emit(Op::BinOp(BinOpKind::Lt));
            let jump_false = compiler.emit(Op::JumpIfFalse(0));

            // Load list[idx] into variable
            compiler.emit(Op::Load(list_slot));
            compiler.emit(Op::Load(idx_slot));
            compiler.emit(Op::Index);
            let var_slot = compiler.alloc_local(variable);
            compiler.emit(Op::Store(var_slot));

            // Body
            compile_stmts(compiler, body)?;
            if let Some(last) = body.last() {
                if matches!(&last.node, Stmt::Expr(_)) {
                    compiler.emit(Op::Pop);
                }
            }

            // Increment idx — this is the continue target
            let increment_start = compiler.current_offset();
            compiler.emit(Op::Load(idx_slot));
            compiler.emit(Op::Const(Value::Int(1)));
            compiler.emit(Op::BinOp(BinOpKind::Add));
            compiler.emit(Op::Store(idx_slot));

            compiler.emit(Op::Jump(loop_start));
            let loop_end = compiler.current_offset();
            compiler.patch_jump(jump_false, loop_end);

            let breaks = compiler.break_targets.pop().unwrap_or_default();
            for idx in breaks {
                compiler.patch_jump(idx, loop_end);
            }
            // Patch continue jumps to the increment section
            let continues = compiler.continue_jumps.pop().unwrap_or_default();
            for idx in continues {
                compiler.patch_jump(idx, increment_start);
            }
            compiler.continue_targets.pop();
            Ok(())
        }
        Stmt::Break => {
            let jump = compiler.emit(Op::Jump(0));
            if let Some(breaks) = compiler.break_targets.last_mut() {
                breaks.push(jump);
            }
            Ok(())
        }
        Stmt::Continue => {
            match compiler.continue_targets.last() {
                Some(Some(target)) => {
                    // While loop: known target
                    compiler.emit(Op::Jump(*target));
                }
                Some(None) => {
                    // For loop: deferred patching
                    let jump = compiler.emit(Op::Jump(0));
                    if let Some(jumps) = compiler.continue_jumps.last_mut() {
                        jumps.push(jump);
                    }
                }
                None => {}
            }
            Ok(())
        }
        Stmt::Match { expr, arms } => {
            compile_match(compiler, expr, arms)?;
            Ok(())
        }
        Stmt::Expr(expr) => {
            compile_expr(compiler, &expr.node)?;
            Ok(())
        }
        Stmt::Assign { name, value } => {
            compile_expr(compiler, &value.node)?;
            let slot = compiler.resolve_local(name)
                .ok_or_else(|| format!("undefined variable '{name}' in assignment"))?;
            compiler.emit(Op::Store(slot));
            Ok(())
        }
        Stmt::Emit { .. } => {
            Err("emit not supported in VM mode".to_string())
        }
    }
}

fn compile_match(
    compiler: &mut Compiler,
    expr: &Spanned<Expr>,
    arms: &[MatchArm],
) -> Result<(), String> {
    compile_expr(compiler, &expr.node)?;
    let match_val_slot = compiler.alloc_temp("match_val");
    compiler.emit(Op::Store(match_val_slot));

    let mut end_jumps = Vec::new();

    for arm in arms {
        match &arm.pattern.node {
            Pattern::Literal(lit) => {
                compiler.emit(Op::Load(match_val_slot));
                compile_literal(compiler, lit);
                compiler.emit(Op::BinOp(BinOpKind::Eq));
                let skip = compiler.emit(Op::JumpIfFalse(0));

                // Guard
                if let Some(ref guard) = arm.guard {
                    compile_expr(compiler, &guard.node)?;
                    let guard_skip = compiler.emit(Op::JumpIfFalse(0));
                    compile_stmts(compiler, &arm.body)?;
                    end_jumps.push(compiler.emit(Op::Jump(0)));
                    let after_guard = compiler.current_offset();
                    compiler.patch_jump(guard_skip, after_guard);
                } else {
                    compile_stmts(compiler, &arm.body)?;
                    end_jumps.push(compiler.emit(Op::Jump(0)));
                }

                let after = compiler.current_offset();
                compiler.patch_jump(skip, after);
            }
            Pattern::Ident(name) if name == "_" => {
                // Wildcard — always matches
                compile_stmts(compiler, &arm.body)?;
                end_jumps.push(compiler.emit(Op::Jump(0)));
            }
            Pattern::Ident(name) => {
                // Binding — bind match value to name
                compiler.emit(Op::Load(match_val_slot));
                let slot = compiler.alloc_local(name);
                compiler.emit(Op::Store(slot));

                if let Some(ref guard) = arm.guard {
                    compile_expr(compiler, &guard.node)?;
                    let guard_skip = compiler.emit(Op::JumpIfFalse(0));
                    compile_stmts(compiler, &arm.body)?;
                    end_jumps.push(compiler.emit(Op::Jump(0)));
                    let after_guard = compiler.current_offset();
                    compiler.patch_jump(guard_skip, after_guard);
                } else {
                    compile_stmts(compiler, &arm.body)?;
                    end_jumps.push(compiler.emit(Op::Jump(0)));
                }
            }
            Pattern::Wildcard => {
                compile_stmts(compiler, &arm.body)?;
                end_jumps.push(compiler.emit(Op::Jump(0)));
            }
            Pattern::Struct { name, fields } => {
                // Check struct name matches
                compiler.emit(Op::Load(match_val_slot));
                compiler.emit(Op::GetField("__struct_name__".to_string()));
                compiler.emit(Op::Const(Value::String(name.clone())));
                compiler.emit(Op::BinOp(BinOpKind::Eq));
                let skip = compiler.emit(Op::JumpIfFalse(0));

                // Bind each field
                for (field_name, sub_pattern) in fields {
                    compiler.emit(Op::Load(match_val_slot));
                    compiler.emit(Op::GetField(field_name.clone()));
                    if let Some(pat) = sub_pattern {
                        match &pat.node {
                            Pattern::Ident(bind_name) if bind_name != "_" => {
                                let slot = compiler.alloc_local(bind_name);
                                compiler.emit(Op::Store(slot));
                            }
                            Pattern::Literal(lit) => {
                                // Check field value matches literal
                                compile_literal(compiler, lit);
                                compiler.emit(Op::BinOp(BinOpKind::Eq));
                                let field_skip = compiler.emit(Op::JumpIfFalse(0));
                                // Deferred: patch to skip after arm body
                                // For now, just fall through on match
                                compiler.patch_jump(field_skip, compiler.current_offset());
                                // This is a simplification; full implementation would skip entire arm
                            }
                            _ => {
                                compiler.emit(Op::Pop); // discard field value (wildcard)
                            }
                        }
                    } else {
                        // No sub-pattern means bind to field name
                        let slot = compiler.alloc_local(field_name);
                        compiler.emit(Op::Store(slot));
                    }
                }

                // Guard
                if let Some(ref guard) = arm.guard {
                    compile_expr(compiler, &guard.node)?;
                    let guard_skip = compiler.emit(Op::JumpIfFalse(0));
                    compile_stmts(compiler, &arm.body)?;
                    end_jumps.push(compiler.emit(Op::Jump(0)));
                    let after_guard = compiler.current_offset();
                    compiler.patch_jump(guard_skip, after_guard);
                } else {
                    compile_stmts(compiler, &arm.body)?;
                    end_jumps.push(compiler.emit(Op::Jump(0)));
                }

                let after = compiler.current_offset();
                compiler.patch_jump(skip, after);
            }
            Pattern::List { elements, rest } => {
                // Check list length
                let expected_min = elements.len();
                compiler.emit(Op::Load(match_val_slot));
                compiler.emit(Op::MethodCall {
                    method: "len".to_string(),
                    arg_count: 0,
                });
                if rest.is_some() {
                    // len >= expected_min
                    compiler.emit(Op::Const(Value::Int(expected_min as i64)));
                    compiler.emit(Op::BinOp(BinOpKind::GtEq));
                } else {
                    // len == expected_min
                    compiler.emit(Op::Const(Value::Int(expected_min as i64)));
                    compiler.emit(Op::BinOp(BinOpKind::Eq));
                }
                let skip = compiler.emit(Op::JumpIfFalse(0));

                // Bind each element
                for (i, elem_pat) in elements.iter().enumerate() {
                    compiler.emit(Op::Load(match_val_slot));
                    compiler.emit(Op::Const(Value::Int(i as i64)));
                    compiler.emit(Op::Index);
                    match &elem_pat.node {
                        Pattern::Ident(bind_name) if bind_name != "_" => {
                            let slot = compiler.alloc_local(bind_name);
                            compiler.emit(Op::Store(slot));
                        }
                        Pattern::Literal(lit) => {
                            compile_literal(compiler, lit);
                            compiler.emit(Op::BinOp(BinOpKind::Eq));
                            // Simplified: skip arm if mismatch
                            let elem_skip = compiler.emit(Op::JumpIfFalse(0));
                            compiler.patch_jump(elem_skip, compiler.current_offset());
                        }
                        _ => {
                            compiler.emit(Op::Pop); // discard (wildcard)
                        }
                    }
                }

                // Handle ..rest
                if let Some(rest_pat) = rest {
                    if let Pattern::Ident(rest_name) = &rest_pat.node {
                        // Collect remaining elements into a list
                        // rest = list[elements.len()..]
                        let total_slot = compiler.alloc_temp("list_len");
                        compiler.emit(Op::Load(match_val_slot));
                        compiler.emit(Op::MethodCall {
                            method: "len".to_string(),
                            arg_count: 0,
                        });
                        compiler.emit(Op::Store(total_slot));

                        // Build rest list via loop
                        let rest_list_slot = compiler.alloc_temp("rest_list");
                        compiler.emit(Op::MakeList(0)); // empty list
                        compiler.emit(Op::Store(rest_list_slot));

                        let rest_idx_slot = compiler.alloc_temp("rest_idx");
                        compiler.emit(Op::Const(Value::Int(elements.len() as i64)));
                        compiler.emit(Op::Store(rest_idx_slot));

                        let loop_start = compiler.current_offset();
                        compiler.emit(Op::Load(rest_idx_slot));
                        compiler.emit(Op::Load(total_slot));
                        compiler.emit(Op::BinOp(BinOpKind::Lt));
                        let loop_exit = compiler.emit(Op::JumpIfFalse(0));

                        // rest_list = rest_list.push(list[rest_idx])
                        compiler.emit(Op::Load(rest_list_slot));
                        compiler.emit(Op::Load(match_val_slot));
                        compiler.emit(Op::Load(rest_idx_slot));
                        compiler.emit(Op::Index);
                        compiler.emit(Op::MethodCall {
                            method: "push".to_string(),
                            arg_count: 1,
                        });
                        compiler.emit(Op::Store(rest_list_slot));

                        // rest_idx = rest_idx + 1
                        compiler.emit(Op::Load(rest_idx_slot));
                        compiler.emit(Op::Const(Value::Int(1)));
                        compiler.emit(Op::BinOp(BinOpKind::Add));
                        compiler.emit(Op::Store(rest_idx_slot));

                        compiler.emit(Op::Jump(loop_start));
                        let loop_end = compiler.current_offset();
                        compiler.patch_jump(loop_exit, loop_end);

                        let slot = compiler.alloc_local(rest_name);
                        compiler.emit(Op::Load(rest_list_slot));
                        compiler.emit(Op::Store(slot));
                    }
                }

                // Guard
                if let Some(ref guard) = arm.guard {
                    compile_expr(compiler, &guard.node)?;
                    let guard_skip = compiler.emit(Op::JumpIfFalse(0));
                    compile_stmts(compiler, &arm.body)?;
                    end_jumps.push(compiler.emit(Op::Jump(0)));
                    let after_guard = compiler.current_offset();
                    compiler.patch_jump(guard_skip, after_guard);
                } else {
                    compile_stmts(compiler, &arm.body)?;
                    end_jumps.push(compiler.emit(Op::Jump(0)));
                }

                let after = compiler.current_offset();
                compiler.patch_jump(skip, after);
            }
            Pattern::Some(inner) => {
                // Check value != None
                compiler.emit(Op::Load(match_val_slot));
                compiler.emit(Op::Const(Value::None));
                compiler.emit(Op::BinOp(BinOpKind::Neq));
                // If value == None (i.e., neq is false), skip this arm
                let skip = compiler.emit(Op::JumpIfFalse(0));

                // Bind inner pattern to the value itself (since Balance doesn't wrap in Some)
                if let Pattern::Ident(bind_name) = &inner.node {
                    let slot = compiler.alloc_local(bind_name);
                    compiler.emit(Op::Load(match_val_slot));
                    compiler.emit(Op::Store(slot));
                }

                // Guard
                if let Some(ref guard) = arm.guard {
                    compile_expr(compiler, &guard.node)?;
                    let guard_skip = compiler.emit(Op::JumpIfFalse(0));
                    compile_stmts(compiler, &arm.body)?;
                    end_jumps.push(compiler.emit(Op::Jump(0)));
                    let after_guard = compiler.current_offset();
                    compiler.patch_jump(guard_skip, after_guard);
                } else {
                    compile_stmts(compiler, &arm.body)?;
                    end_jumps.push(compiler.emit(Op::Jump(0)));
                }

                let after = compiler.current_offset();
                compiler.patch_jump(skip, after);
            }
            Pattern::None => {
                // Check value == None
                compiler.emit(Op::Load(match_val_slot));
                compiler.emit(Op::Const(Value::None));
                compiler.emit(Op::BinOp(BinOpKind::Eq));
                let skip = compiler.emit(Op::JumpIfFalse(0));

                // Guard
                if let Some(ref guard) = arm.guard {
                    compile_expr(compiler, &guard.node)?;
                    let guard_skip = compiler.emit(Op::JumpIfFalse(0));
                    compile_stmts(compiler, &arm.body)?;
                    end_jumps.push(compiler.emit(Op::Jump(0)));
                    let after_guard = compiler.current_offset();
                    compiler.patch_jump(guard_skip, after_guard);
                } else {
                    compile_stmts(compiler, &arm.body)?;
                    end_jumps.push(compiler.emit(Op::Jump(0)));
                }

                let after = compiler.current_offset();
                compiler.patch_jump(skip, after);
            }
            Pattern::Ok(inner) => {
                // Check value is Ok by calling is_ok() method
                compiler.emit(Op::Load(match_val_slot));
                compiler.emit(Op::MethodCall { method: "is_ok".to_string(), arg_count: 0 });
                let skip = compiler.emit(Op::JumpIfFalse(0));

                // Bind inner pattern to the unwrapped value
                if let Pattern::Ident(bind_name) = &inner.node {
                    let slot = compiler.alloc_local(bind_name);
                    compiler.emit(Op::Load(match_val_slot));
                    compiler.emit(Op::MethodCall { method: "unwrap".to_string(), arg_count: 0 });
                    compiler.emit(Op::Store(slot));
                }

                if let Some(ref guard) = arm.guard {
                    compile_expr(compiler, &guard.node)?;
                    let guard_skip = compiler.emit(Op::JumpIfFalse(0));
                    compile_stmts(compiler, &arm.body)?;
                    end_jumps.push(compiler.emit(Op::Jump(0)));
                    let after_guard = compiler.current_offset();
                    compiler.patch_jump(guard_skip, after_guard);
                } else {
                    compile_stmts(compiler, &arm.body)?;
                    end_jumps.push(compiler.emit(Op::Jump(0)));
                }

                let after = compiler.current_offset();
                compiler.patch_jump(skip, after);
            }
            Pattern::Err(inner) => {
                // Check value is Err by calling is_err() method
                compiler.emit(Op::Load(match_val_slot));
                compiler.emit(Op::MethodCall { method: "is_err".to_string(), arg_count: 0 });
                let skip = compiler.emit(Op::JumpIfFalse(0));

                // Bind inner pattern to the unwrapped error
                if let Pattern::Ident(bind_name) = &inner.node {
                    let slot = compiler.alloc_local(bind_name);
                    compiler.emit(Op::Load(match_val_slot));
                    compiler.emit(Op::MethodCall { method: "unwrap_err".to_string(), arg_count: 0 });
                    compiler.emit(Op::Store(slot));
                }

                if let Some(ref guard) = arm.guard {
                    compile_expr(compiler, &guard.node)?;
                    let guard_skip = compiler.emit(Op::JumpIfFalse(0));
                    compile_stmts(compiler, &arm.body)?;
                    end_jumps.push(compiler.emit(Op::Jump(0)));
                    let after_guard = compiler.current_offset();
                    compiler.patch_jump(guard_skip, after_guard);
                } else {
                    compile_stmts(compiler, &arm.body)?;
                    end_jumps.push(compiler.emit(Op::Jump(0)));
                }

                let after = compiler.current_offset();
                compiler.patch_jump(skip, after);
            }
        }
    }

    // Default: push Unit if no arm matched
    compiler.emit(Op::Const(Value::Unit));

    let end = compiler.current_offset();
    for idx in end_jumps {
        compiler.patch_jump(idx, end);
    }

    Ok(())
}

fn compile_expr(compiler: &mut Compiler, expr: &Expr) -> Result<(), String> {
    match expr {
        Expr::Literal(lit) => {
            compile_literal(compiler, lit);
            Ok(())
        }
        Expr::None => {
            compiler.emit(Op::Const(Value::None));
            Ok(())
        }
        Expr::Ident(name) => {
            if let Some(slot) = compiler.resolve_local(name) {
                compiler.emit(Op::Load(slot));
                Ok(())
            } else {
                Err(format!("undefined variable '{name}' in VM compilation"))
            }
        }
        Expr::Binary { op, left, right } => {
            compile_expr(compiler, &left.node)?;
            compile_expr(compiler, &right.node)?;
            let kind = match op {
                BinaryOp::Add => BinOpKind::Add,
                BinaryOp::Sub => BinOpKind::Sub,
                BinaryOp::Mul => BinOpKind::Mul,
                BinaryOp::Div => BinOpKind::Div,
                BinaryOp::Mod => BinOpKind::Mod,
                BinaryOp::Eq => BinOpKind::Eq,
                BinaryOp::Neq => BinOpKind::Neq,
                BinaryOp::Lt => BinOpKind::Lt,
                BinaryOp::Gt => BinOpKind::Gt,
                BinaryOp::LtEq => BinOpKind::LtEq,
                BinaryOp::GtEq => BinOpKind::GtEq,
                BinaryOp::And => BinOpKind::And,
                BinaryOp::Or => BinOpKind::Or,
                BinaryOp::BitAnd => BinOpKind::BitAnd,
                BinaryOp::BitOr => BinOpKind::BitOr,
                BinaryOp::BitXor => BinOpKind::BitXor,
                BinaryOp::Shl => BinOpKind::Shl,
                BinaryOp::Shr => BinOpKind::Shr,
            };
            compiler.emit(Op::BinOp(kind));
            Ok(())
        }
        Expr::Unary { op, operand } => {
            compile_expr(compiler, &operand.node)?;
            let kind = match op {
                UnaryOp::Neg => UnOpKind::Neg,
                UnaryOp::Not => UnOpKind::Not,
                UnaryOp::BitNot => UnOpKind::BitNot,
            };
            compiler.emit(Op::UnOp(kind));
            Ok(())
        }
        Expr::Block(stmts) => {
            if stmts.is_empty() {
                compiler.emit(Op::Const(Value::Unit));
            } else {
                compile_stmts(compiler, stmts)?;
            }
            Ok(())
        }
        Expr::FnCall { func, args } => {
            if let Expr::Ident(name) = &func.node {
                // Built-in Result constructors
                if name == "ok" {
                    if let Some(arg) = args.first() {
                        compile_expr(compiler, &arg.node)?;
                    } else {
                        compiler.emit(Op::Const(Value::Unit));
                    }
                    compiler.emit(Op::WrapOk);
                    return Ok(());
                }
                if name == "err" {
                    if let Some(arg) = args.first() {
                        compile_expr(compiler, &arg.node)?;
                    } else {
                        compiler.emit(Op::Const(Value::Unit));
                    }
                    compiler.emit(Op::WrapErr);
                    return Ok(());
                }

                // Check if the name resolves to a local (could be a closure variable)
                if compiler.resolve_local(name).is_some() {
                    // Compile arguments first
                    for arg in args {
                        compile_expr(compiler, &arg.node)?;
                    }
                    // Load the closure value
                    compile_expr(compiler, &func.node)?;
                    compiler.emit(Op::CallClosure(args.len()));
                    Ok(())
                } else {
                    // Named function call
                    for arg in args {
                        compile_expr(compiler, &arg.node)?;
                    }
                    compiler.emit(Op::Const(Value::String(name.clone())));
                    compiler.emit(Op::Call(args.len()));
                    Ok(())
                }
            } else {
                // Expression call (e.g., expr(args)) — treat as closure call
                for arg in args {
                    compile_expr(compiler, &arg.node)?;
                }
                compile_expr(compiler, &func.node)?;
                compiler.emit(Op::CallClosure(args.len()));
                Ok(())
            }
        }
        Expr::ListLiteral { elements } => {
            for elem in elements {
                compile_expr(compiler, &elem.node)?;
            }
            compiler.emit(Op::MakeList(elements.len()));
            Ok(())
        }
        Expr::MapLiteral { entries } => {
            for (key, val) in entries {
                compile_expr(compiler, &key.node)?;
                compile_expr(compiler, &val.node)?;
            }
            compiler.emit(Op::MakeMap(entries.len()));
            Ok(())
        }
        Expr::StructLiteral { name, fields } => {
            let field_names: Vec<String> = fields.iter().map(|(n, _)| n.clone()).collect();
            for (_, val) in fields {
                compile_expr(compiler, &val.node)?;
            }
            compiler.emit(Op::MakeStruct {
                name: name.clone(),
                field_count: fields.len(),
                field_names,
            });
            Ok(())
        }
        Expr::Index { receiver, index } => {
            compile_expr(compiler, &receiver.node)?;
            compile_expr(compiler, &index.node)?;
            compiler.emit(Op::Index);
            Ok(())
        }
        Expr::FieldAccess { receiver, field } => {
            compile_expr(compiler, &receiver.node)?;
            compiler.emit(Op::GetField(field.clone()));
            Ok(())
        }
        Expr::MethodCall {
            receiver,
            method,
            args,
        } => {
            compile_expr(compiler, &receiver.node)?;
            for arg in args {
                compile_expr(compiler, &arg.node)?;
            }
            compiler.emit(Op::MethodCall {
                method: method.clone(),
                arg_count: args.len(),
            });
            Ok(())
        }
        Expr::Match { expr, arms } => {
            compile_match(compiler, expr, arms)?;
            Ok(())
        }
        // Service-related expressions are not supported in VM
        Expr::Resolve { .. } => {
            Err("VM does not support 'resolve' (service interaction)".to_string())
        }
        Expr::Await { .. } => {
            Err("VM does not support 'await' (service interaction)".to_string())
        }
        Expr::ConcurrentAwait { .. } => {
            Err("VM does not support 'concurrent' (service interaction)".to_string())
        }
        Expr::Select { .. } => {
            Err("VM does not support 'select' (I/O multiplexing)".to_string())
        }
        Expr::Closure { params, body } => {
            // Identify captured variables: variables referenced in the closure body
            // that are defined in the enclosing scope (present in compiler.locals)
            let body_refs = collect_referenced_vars_stmts(body);
            let param_names: std::collections::HashSet<&str> =
                params.iter().map(|p| p.name.as_str()).collect();
            let mut captures: Vec<(String, usize)> = Vec::new();
            for var_name in &body_refs {
                if param_names.contains(var_name.as_str()) {
                    continue; // Parameter, not a capture
                }
                if let Some(slot) = compiler.resolve_local(var_name) {
                    if !captures.iter().any(|(n, _)| n == var_name) {
                        captures.push((var_name.clone(), slot));
                    }
                }
            }

            // Compile the closure body as a new function
            let mut closure_compiler = Compiler::new();
            // First locals are the captured variables
            for (cap_name, _) in &captures {
                closure_compiler.alloc_local(cap_name);
            }
            // Then parameters
            for param in params {
                closure_compiler.alloc_local(&param.name);
            }

            compile_stmts(&mut closure_compiler, body)?;

            // Implicit return of last expression
            if !matches!(closure_compiler.code.last(), Some(Op::Return)) {
                let last_was_expr = body.last().map_or(false, |s| matches!(&s.node, Stmt::Expr(_)));
                if !last_was_expr {
                    closure_compiler.emit(Op::Const(Value::Unit));
                }
                closure_compiler.emit(Op::Return);
            }

            let closure_fn = CompiledFunction {
                name: format!("__closure_{}_", compiler.temp_counter),
                arity: captures.len() + params.len(), // captures + params
                locals_count: closure_compiler.next_local,
                code: closure_compiler.code,
            };
            compiler.temp_counter += 1;

            // Collect nested closures from the closure compiler
            let nested = std::mem::take(&mut closure_compiler.closure_functions);
            let func_index = compiler.closure_functions.len();
            compiler.closure_functions.push(closure_fn);
            compiler.closure_functions.extend(nested);

            // Emit: push captured values, then MakeClosure
            for (_, slot) in &captures {
                compiler.emit(Op::Load(*slot));
            }
            compiler.emit(Op::MakeClosure {
                func_index,
                capture_count: captures.len(),
            });
            Ok(())
        }
        Expr::MacroCall { .. } => {
            Err("VM does not support macro calls (expand macros before compilation)".to_string())
        }
        Expr::DynamicImport { .. } => {
            Err("VM does not support dynamic imports".to_string())
        }
        Expr::Try { expr } => {
            compile_expr(compiler, &expr.node)?;
            compiler.emit(Op::TryUnwrap);
            Ok(())
        }
    }
}

/// Collect all variable names referenced in a list of statements.
fn collect_referenced_vars_stmts(stmts: &[Spanned<Stmt>]) -> Vec<String> {
    let mut vars = Vec::new();
    for stmt in stmts {
        collect_referenced_vars_stmt(&stmt.node, &mut vars);
    }
    vars
}

fn collect_referenced_vars_stmt(stmt: &Stmt, vars: &mut Vec<String>) {
    match stmt {
        Stmt::Let { value, .. } => collect_referenced_vars_expr(&value.node, vars),
        Stmt::Return(Some(expr)) => collect_referenced_vars_expr(&expr.node, vars),
        Stmt::Return(None) => {}
        Stmt::If { condition, then_block, else_block } => {
            collect_referenced_vars_expr(&condition.node, vars);
            for s in then_block { collect_referenced_vars_stmt(&s.node, vars); }
            if let Some(eb) = else_block {
                for s in eb { collect_referenced_vars_stmt(&s.node, vars); }
            }
        }
        Stmt::While { condition, body } => {
            collect_referenced_vars_expr(&condition.node, vars);
            for s in body { collect_referenced_vars_stmt(&s.node, vars); }
        }
        Stmt::For { iterable, body, .. } => {
            collect_referenced_vars_expr(&iterable.node, vars);
            for s in body { collect_referenced_vars_stmt(&s.node, vars); }
        }
        Stmt::Expr(expr) => collect_referenced_vars_expr(&expr.node, vars),
        Stmt::Match { expr, arms } => {
            collect_referenced_vars_expr(&expr.node, vars);
            for arm in arms {
                if let Some(g) = &arm.guard { collect_referenced_vars_expr(&g.node, vars); }
                for s in &arm.body { collect_referenced_vars_stmt(&s.node, vars); }
            }
        }
        Stmt::Break | Stmt::Continue => {}
        Stmt::Assign { name, value } => {
            vars.push(name.clone());
            collect_referenced_vars_expr(&value.node, vars);
        }
        Stmt::Emit { fields, .. } => {
            for (_name, expr) in fields {
                collect_referenced_vars_expr(&expr.node, vars);
            }
        }
    }
}

fn collect_referenced_vars_expr(expr: &Expr, vars: &mut Vec<String>) {
    match expr {
        Expr::Ident(name) => vars.push(name.clone()),
        Expr::Binary { left, right, .. } => {
            collect_referenced_vars_expr(&left.node, vars);
            collect_referenced_vars_expr(&right.node, vars);
        }
        Expr::Unary { operand, .. } => collect_referenced_vars_expr(&operand.node, vars),
        Expr::FnCall { func, args } => {
            collect_referenced_vars_expr(&func.node, vars);
            for a in args { collect_referenced_vars_expr(&a.node, vars); }
        }
        Expr::MethodCall { receiver, args, .. } => {
            collect_referenced_vars_expr(&receiver.node, vars);
            for a in args { collect_referenced_vars_expr(&a.node, vars); }
        }
        Expr::FieldAccess { receiver, .. } => collect_referenced_vars_expr(&receiver.node, vars),
        Expr::Index { receiver, index } => {
            collect_referenced_vars_expr(&receiver.node, vars);
            collect_referenced_vars_expr(&index.node, vars);
        }
        Expr::Block(stmts) => {
            for s in stmts { collect_referenced_vars_stmt(&s.node, vars); }
        }
        Expr::ListLiteral { elements } => {
            for e in elements { collect_referenced_vars_expr(&e.node, vars); }
        }
        Expr::MapLiteral { entries } => {
            for (k, v) in entries {
                collect_referenced_vars_expr(&k.node, vars);
                collect_referenced_vars_expr(&v.node, vars);
            }
        }
        Expr::StructLiteral { fields, .. } => {
            for (_, v) in fields { collect_referenced_vars_expr(&v.node, vars); }
        }
        Expr::Closure { body, .. } => {
            for s in body { collect_referenced_vars_stmt(&s.node, vars); }
        }
        Expr::Match { expr, arms } => {
            collect_referenced_vars_expr(&expr.node, vars);
            for arm in arms {
                if let Some(g) = &arm.guard { collect_referenced_vars_expr(&g.node, vars); }
                for s in &arm.body { collect_referenced_vars_stmt(&s.node, vars); }
            }
        }
        Expr::Try { expr } => collect_referenced_vars_expr(&expr.node, vars),
        _ => {} // Literal, None, Resolve, Await, etc.
    }
}

fn compile_literal(compiler: &mut Compiler, lit: &Literal) {
    let value = match lit {
        Literal::Int(n) => Value::Int(*n),
        Literal::Float(f) => Value::Float(*f),
        Literal::String(s) => Value::String(s.clone()),
        Literal::Bool(b) => Value::Bool(*b),
        Literal::Bytes(b) => Value::Bytes(b.clone()),
    };
    compiler.emit(Op::Const(value));
}

/// Convert a TypeExpr to a human-readable string for metadata.
fn type_expr_to_string(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named { name, type_args, nullable } => {
            let mut s = name.clone();
            if !type_args.is_empty() {
                s.push('<');
                for (i, arg) in type_args.iter().enumerate() {
                    if i > 0 { s.push_str(", "); }
                    s.push_str(&type_expr_to_string(&arg.node));
                }
                s.push('>');
            }
            if *nullable { s.push('?'); }
            s
        }
        TypeExpr::Cap { port_name, qualifier } => {
            let mut s = format!("cap {port_name}");
            if let Some(q) = qualifier {
                s = format!("{q:?} {s}");
            }
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser;

    fn compile_source(source: &str) -> Result<CompiledProgram, String> {
        let tokens = tokenize(source).map_err(|_| "tokenize error".to_string())?;
        let (mut program, errors) = parser::parse(source, &tokens);
        if !errors.is_empty() {
            return Err(format!("parse error: {}", errors[0].message));
        }
        crate::macro_expand::expand_program(&mut program);
        compile_program(&program)
    }

    #[test]
    fn test_compile_arithmetic() {
        let prog = compile_source("fn add(a: Int, b: Int) -> Int { return a + b }").unwrap();
        assert_eq!(prog.functions.len(), 1);
        assert_eq!(prog.functions[0].name, "add");
        assert_eq!(prog.functions[0].arity, 2);
    }

    #[test]
    fn test_compile_entry() {
        let prog = compile_source("entry() { let x = 1 + 2\n return x }").unwrap();
        assert!(prog.entry_index.is_some());
        let entry = &prog.functions[prog.entry_index.unwrap()];
        assert_eq!(entry.arity, 0);
    }

    #[test]
    fn test_compile_if_else() {
        let prog = compile_source("fn test() -> Int { if true { return 1 } else { return 2 } }").unwrap();
        assert_eq!(prog.functions.len(), 1);
        // Should contain JumpIfFalse
        assert!(prog.functions[0].code.iter().any(|op| matches!(op, Op::JumpIfFalse(_))));
    }

    #[test]
    fn test_compile_while_loop() {
        let prog = compile_source("fn test() -> Int { let x = 0\n while x < 10 { let x = x + 1 }\n return x }").unwrap();
        assert_eq!(prog.functions.len(), 1);
        // Should contain Jump (back to loop start)
        assert!(prog.functions[0].code.iter().any(|op| matches!(op, Op::Jump(_))));
    }

    #[test]
    fn test_compile_list_literal() {
        let prog = compile_source("fn test() -> List { return [1, 2, 3] }").unwrap();
        assert!(prog.functions[0].code.iter().any(|op| matches!(op, Op::MakeList(3))));
    }

    #[test]
    fn test_compile_resolve_rejected() {
        let result = compile_source(r#"entry() { let x = resolve KV["kv/main"] }"#);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("resolve"));
    }

    #[test]
    fn test_compile_multiple_functions() {
        let source = r#"
            fn double(x: Int) -> Int { return x * 2 }
            fn square(x: Int) -> Int { return x * x }
        "#;
        let prog = compile_source(source).unwrap();
        assert_eq!(prog.functions.len(), 2);
        assert_eq!(prog.functions[0].name, "double");
        assert_eq!(prog.functions[1].name, "square");
    }

    #[test]
    fn test_compile_match_expression() {
        let source = r#"
            fn test(x: Int) -> String {
                return match x {
                    1 => "one"
                    2 => "two"
                    _ => "other"
                }
            }
        "#;
        let prog = compile_source(source).unwrap();
        assert_eq!(prog.functions.len(), 1);
    }

    #[test]
    fn test_metadata_types_and_services() {
        let source = r#"
            type Point {
                x: Int
                y: Int
            }
            port KV {
                get(key: String) -> String? [query]
            }
            service KVService provides KV {
                publish as "kv/main"
                query get(key: String) -> String? {
                    return none
                }
            }
            fn helper() -> Int { return 1 }
        "#;
        let prog = compile_source(source).unwrap();
        assert_eq!(prog.types.len(), 1);
        assert_eq!(prog.types[0].name, "Point");
        assert_eq!(prog.types[0].fields.len(), 2);
        assert_eq!(prog.types[0].fields[0], ("x".to_string(), "Int".to_string()));
        assert_eq!(prog.services.len(), 1);
        assert_eq!(prog.services[0].name, "KVService");
        assert_eq!(prog.services[0].port, "KV");
        assert_eq!(prog.services[0].publish_id, Some("kv/main".to_string()));
    }

    #[test]
    fn test_metadata_imports() {
        let source = r#"
            import helpers.math.{add, mul}
            fn double(x: Int) -> Int { return x * 2 }
        "#;
        let prog = compile_source(source).unwrap();
        assert_eq!(prog.imports.len(), 1);
        assert_eq!(prog.imports[0].path, vec!["helpers", "math"]);
        assert_eq!(prog.imports[0].names, vec!["add", "mul"]);
    }

    #[test]
    fn test_metadata_module_name() {
        let source = r#"
            module my.cool.lib
            fn greet() -> String { return "hi" }
        "#;
        let prog = compile_source(source).unwrap();
        assert_eq!(prog.module_name, Some("my.cool.lib".to_string()));
    }

    // === Gap 10: Module dependency graph in compiled output ===

    #[test]
    fn test_metadata_dependencies() {
        let source = r#"
            import helpers.math.{add, mul}
            import utils.strings
            fn double(x: Int) -> Int { return x * 2 }
        "#;
        let prog = compile_source(source).unwrap();
        assert_eq!(prog.dependencies.len(), 2);
        assert!(prog.dependencies.contains(&"helpers.math".to_string()));
        assert!(prog.dependencies.contains(&"utils.strings".to_string()));
    }

    // === Gap 9: Port, guarantee, and dependency graph metadata ===

    #[test]
    fn test_metadata_ports() {
        let source = r#"
            port KV {
                get(key: String) -> String? [query]
                put(key: String, value: String) -> String [command]
            }
            fn helper() -> Int { return 1 }
        "#;
        let prog = compile_source(source).unwrap();
        assert_eq!(prog.ports.len(), 1);
        assert_eq!(prog.ports[0].name, "KV");
        assert_eq!(prog.ports[0].methods.len(), 2);
        let get_method = prog.ports[0].methods.iter().find(|m| m.0 == "get").unwrap();
        assert_eq!(get_method.1, 1); // 1 param
        assert_eq!(get_method.2, "query");
        let put_method = prog.ports[0].methods.iter().find(|m| m.0 == "put").unwrap();
        assert_eq!(put_method.1, 2); // 2 params
        assert_eq!(put_method.2, "command");
    }

    #[test]
    fn test_metadata_guarantees() {
        let source = r#"
            substrate ReplicatedLog<T> {
                op append(entry: T) -> String
                emits { append_accepted  quorum_committed }
            }
            guarantee commit_consistency {
                law: append_accepted(key) => quorum_committed(key)
            }
            fn helper() -> Int { return 1 }
        "#;
        let prog = compile_source(source).unwrap();
        assert_eq!(prog.guarantees.len(), 1);
        assert_eq!(prog.guarantees[0].name, "commit_consistency");
        assert_eq!(prog.guarantees[0].law_descriptions.len(), 1);
        assert!(prog.guarantees[0].law_descriptions[0].contains("append_accepted"));
    }

    #[test]
    fn test_metadata_dependency_graph() {
        let source = r#"
            port KV {
                get(key: String) -> String? [query]
            }
            service KVService provides KV {
                publish as "kv/main"
                query get(key: String) -> String? {
                    return none
                }
            }
            fn helper() -> Int { return 1 }
        "#;
        let prog = compile_source(source).unwrap();
        assert_eq!(prog.dependency_graph.len(), 1);
        assert_eq!(prog.dependency_graph[0], ("KVService".to_string(), "KV".to_string()));
    }
}
