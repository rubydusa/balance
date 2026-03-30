use std::collections::HashMap;

use crate::runtime::value::Value;

use super::ir::*;

/// Stack frame for function execution.
#[derive(Debug)]
struct Frame {
    /// Index of the function in CompiledProgram.functions
    func_index: usize,
    /// Instruction pointer within the function's code
    ip: usize,
    /// Local variable slots
    locals: Vec<Value>,
    /// Stack base offset (where this frame's stack starts)
    stack_base: usize,
}

/// Closure data stored in the VM.
#[derive(Debug, Clone)]
struct ClosureData {
    func_index: usize,
    captured: Vec<Value>,
}

/// Virtual machine for executing compiled Balance bytecode.
/// Handles pure computation only; service interactions remain in the tree-walker.
pub struct VirtualMachine {
    /// Value stack
    stack: Vec<Value>,
    /// Call frame stack
    frames: Vec<Frame>,
    /// Closure storage: indexed by closure ref id
    closures: Vec<ClosureData>,
}

impl VirtualMachine {
    pub fn new() -> Self {
        Self {
            stack: Vec::new(),
            frames: Vec::new(),
            closures: Vec::new(),
        }
    }

    /// Execute a compiled program, starting from the entry function.
    pub fn execute(&mut self, program: &CompiledProgram) -> Result<Value, String> {
        let entry_index = program
            .entry_index
            .ok_or_else(|| "no entry function in compiled program".to_string())?;

        self.call_function(program, entry_index, &[])?;
        self.run(program)
    }

    /// Execute a specific function by name with given arguments.
    pub fn call_named(
        &mut self,
        program: &CompiledProgram,
        name: &str,
        args: &[Value],
    ) -> Result<Value, String> {
        let func_index = program
            .functions
            .iter()
            .position(|f| f.name == name)
            .ok_or_else(|| format!("function '{name}' not found"))?;

        self.call_function(program, func_index, args)?;
        self.run(program)
    }

    fn call_function(
        &mut self,
        program: &CompiledProgram,
        func_index: usize,
        args: &[Value],
    ) -> Result<(), String> {
        let func = &program.functions[func_index];
        if args.len() != func.arity {
            return Err(format!(
                "function '{}' expects {} args, got {}",
                func.name,
                func.arity,
                args.len()
            ));
        }

        let stack_base = self.stack.len();
        let mut locals = vec![Value::Unit; func.locals_count];

        // Copy arguments into local slots
        for (i, arg) in args.iter().enumerate() {
            locals[i] = arg.clone();
        }

        self.frames.push(Frame {
            func_index,
            ip: 0,
            locals,
            stack_base,
        });

        Ok(())
    }

    fn run(&mut self, program: &CompiledProgram) -> Result<Value, String> {
        loop {
            let frame = self.frames.last_mut().ok_or("no active frame")?;
            let func = &program.functions[frame.func_index];

            if frame.ip >= func.code.len() {
                // Implicit return Unit
                let result = self.stack.pop().unwrap_or(Value::Unit);
                self.frames.pop();
                if self.frames.is_empty() {
                    return Ok(result);
                }
                self.stack.push(result);
                continue;
            }

            let op = func.code[frame.ip].clone();
            frame.ip += 1;

            match op {
                Op::Const(value) => {
                    self.stack.push(value);
                }
                Op::Load(slot) => {
                    let frame = self.frames.last().unwrap();
                    let value = frame.locals.get(slot).cloned().unwrap_or(Value::Unit);
                    self.stack.push(value);
                }
                Op::Store(slot) => {
                    let value = self.stack.pop().unwrap_or(Value::Unit);
                    let frame = self.frames.last_mut().unwrap();
                    if slot >= frame.locals.len() {
                        frame.locals.resize(slot + 1, Value::Unit);
                    }
                    frame.locals[slot] = value;
                }
                Op::BinOp(kind) => {
                    let right = self.stack.pop().unwrap_or(Value::Unit);
                    let left = self.stack.pop().unwrap_or(Value::Unit);
                    let result = eval_binop(kind, &left, &right)?;
                    self.stack.push(result);
                }
                Op::UnOp(kind) => {
                    let operand = self.stack.pop().unwrap_or(Value::Unit);
                    let result = eval_unop(kind, &operand)?;
                    self.stack.push(result);
                }
                Op::Jump(target) => {
                    let frame = self.frames.last_mut().unwrap();
                    frame.ip = target;
                }
                Op::JumpIfFalse(target) => {
                    let cond = self.stack.pop().unwrap_or(Value::Bool(false));
                    if !cond.is_truthy() {
                        let frame = self.frames.last_mut().unwrap();
                        frame.ip = target;
                    }
                }
                Op::Call(arg_count) => {
                    let func_name = self.stack.pop().unwrap_or(Value::Unit);
                    match &func_name {
                        Value::String(name) => {
                            // Pop args from stack
                            let mut args = Vec::with_capacity(arg_count);
                            for _ in 0..arg_count {
                                args.push(self.stack.pop().unwrap_or(Value::Unit));
                            }
                            args.reverse();

                            // Find function
                            let func_idx = program
                                .functions
                                .iter()
                                .position(|f| f.name == *name)
                                .ok_or_else(|| format!("function '{name}' not found"))?;

                            self.call_function(program, func_idx, &args)?;
                        }
                        Value::ClosureRef(id) => {
                            // Calling a closure via Call (fallback)
                            let closure = self.closures.get(*id as usize)
                                .ok_or_else(|| format!("closure {id} not found"))?
                                .clone();
                            let mut args = Vec::with_capacity(arg_count);
                            for _ in 0..arg_count {
                                args.push(self.stack.pop().unwrap_or(Value::Unit));
                            }
                            args.reverse();

                            // Build full args: captures + call args
                            let mut full_args = closure.captured;
                            full_args.extend(args);
                            self.call_function(program, closure.func_index, &full_args)?;
                        }
                        _ => return Err("Call target must be a function name or closure".to_string()),
                    }
                }
                Op::Return => {
                    let result = self.stack.pop().unwrap_or(Value::Unit);
                    // Pop frame
                    let frame = self.frames.pop().unwrap();
                    // Trim stack back to frame's base
                    self.stack.truncate(frame.stack_base);

                    if self.frames.is_empty() {
                        return Ok(result);
                    }
                    self.stack.push(result);
                }
                Op::Pop => {
                    self.stack.pop();
                }
                Op::MakeList(count) => {
                    let mut elements = Vec::with_capacity(count);
                    for _ in 0..count {
                        elements.push(self.stack.pop().unwrap_or(Value::Unit));
                    }
                    elements.reverse();
                    self.stack.push(Value::List(elements));
                }
                Op::MakeMap(count) => {
                    let mut entries = Vec::with_capacity(count);
                    for _ in 0..count {
                        let val = self.stack.pop().unwrap_or(Value::Unit);
                        let key = self.stack.pop().unwrap_or(Value::Unit);
                        let key_str = match key {
                            Value::String(s) => s,
                            other => format!("{other}"),
                        };
                        entries.push((key_str, val));
                    }
                    entries.reverse();
                    let map: std::collections::HashMap<String, Value> =
                        entries.into_iter().collect();
                    self.stack.push(Value::Map(map));
                }
                Op::MakeStruct {
                    name,
                    field_count,
                    field_names,
                } => {
                    let mut fields = HashMap::new();
                    let mut values = Vec::with_capacity(field_count);
                    for _ in 0..field_count {
                        values.push(self.stack.pop().unwrap_or(Value::Unit));
                    }
                    values.reverse();
                    for (fname, val) in field_names.into_iter().zip(values) {
                        fields.insert(fname, val);
                    }
                    self.stack.push(Value::Struct { name, fields });
                }
                Op::Index => {
                    let index = self.stack.pop().unwrap_or(Value::Unit);
                    let receiver = self.stack.pop().unwrap_or(Value::Unit);
                    let result = eval_index(&receiver, &index)?;
                    self.stack.push(result);
                }
                Op::GetField(field) => {
                    let receiver = self.stack.pop().unwrap_or(Value::Unit);
                    let result = eval_field_access(&receiver, &field)?;
                    self.stack.push(result);
                }
                Op::MethodCall { method, arg_count } => {
                    let mut args = Vec::with_capacity(arg_count);
                    for _ in 0..arg_count {
                        args.push(self.stack.pop().unwrap_or(Value::Unit));
                    }
                    args.reverse();
                    let receiver = self.stack.pop().unwrap_or(Value::Unit);
                    let result = eval_method_call(&receiver, &method, &args)?;
                    self.stack.push(result);
                }
                Op::MakeClosure { func_index, capture_count } => {
                    let mut captured = Vec::with_capacity(capture_count);
                    for _ in 0..capture_count {
                        captured.push(self.stack.pop().unwrap_or(Value::Unit));
                    }
                    captured.reverse();
                    let id = self.closures.len() as u64;
                    self.closures.push(ClosureData { func_index, captured });
                    self.stack.push(Value::ClosureRef(id));
                }
                Op::CallClosure(arg_count) => {
                    let closure_val = self.stack.pop().unwrap_or(Value::Unit);
                    let closure_id = match closure_val {
                        Value::ClosureRef(id) => id,
                        _ => return Err(format!("expected closure, got {closure_val}")),
                    };
                    let closure = self.closures.get(closure_id as usize)
                        .ok_or_else(|| format!("closure {closure_id} not found"))?
                        .clone();
                    let mut args = Vec::with_capacity(arg_count);
                    for _ in 0..arg_count {
                        args.push(self.stack.pop().unwrap_or(Value::Unit));
                    }
                    args.reverse();

                    // Build full args: captures + call args
                    let mut full_args = closure.captured;
                    full_args.extend(args);
                    self.call_function(program, closure.func_index, &full_args)?;
                }
                Op::Halt => {
                    let result = self.stack.pop().unwrap_or(Value::Unit);
                    return Ok(result);
                }
                Op::Nop => {}
                Op::Dup => {
                    let top = self.stack.last().cloned().unwrap_or(Value::Unit);
                    self.stack.push(top);
                }
                Op::WrapOk => {
                    let val = self.stack.pop().unwrap_or(Value::Unit);
                    self.stack.push(Value::Ok(Box::new(val)));
                }
                Op::WrapErr => {
                    let val = self.stack.pop().unwrap_or(Value::Unit);
                    self.stack.push(Value::Err(Box::new(val)));
                }
                Op::TryUnwrap => {
                    let val = self.stack.pop().unwrap_or(Value::Unit);
                    match val {
                        Value::Ok(v) => self.stack.push(*v),
                        Value::Err(_) => return Ok(val),
                        _ => return Err(format!("? operator requires Ok or Err, got {}", val.type_name())),
                    }
                }
            }
        }
    }
}

impl Default for VirtualMachine {
    fn default() -> Self {
        Self::new()
    }
}

fn eval_binop(kind: BinOpKind, left: &Value, right: &Value) -> Result<Value, String> {
    match (kind, left, right) {
        (BinOpKind::Add, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a + b)),
        (BinOpKind::Sub, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a - b)),
        (BinOpKind::Mul, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a * b)),
        (BinOpKind::Div, Value::Int(a), Value::Int(b)) => {
            if *b == 0 {
                Err("division by zero".to_string())
            } else {
                Ok(Value::Int(a / b))
            }
        }
        (BinOpKind::Mod, Value::Int(a), Value::Int(b)) => {
            if *b == 0 {
                Err("modulo by zero".to_string())
            } else {
                Ok(Value::Int(a % b))
            }
        }
        (BinOpKind::Add, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
        (BinOpKind::Sub, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a - b)),
        (BinOpKind::Mul, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a * b)),
        (BinOpKind::Div, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a / b)),
        (BinOpKind::Mod, Value::Float(a), Value::Float(b)) => Ok(Value::Float(a % b)),
        (BinOpKind::Add, Value::String(a), Value::String(b)) => {
            Ok(Value::String(format!("{a}{b}")))
        }
        (BinOpKind::Eq, a, b) => Ok(Value::Bool(a == b)),
        (BinOpKind::Neq, a, b) => Ok(Value::Bool(a != b)),
        (BinOpKind::Lt, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a < b)),
        (BinOpKind::Gt, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a > b)),
        (BinOpKind::LtEq, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a <= b)),
        (BinOpKind::GtEq, Value::Int(a), Value::Int(b)) => Ok(Value::Bool(a >= b)),
        (BinOpKind::Lt, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a < b)),
        (BinOpKind::Gt, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a > b)),
        (BinOpKind::LtEq, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a <= b)),
        (BinOpKind::GtEq, Value::Float(a), Value::Float(b)) => Ok(Value::Bool(a >= b)),
        (BinOpKind::And, Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a && *b)),
        (BinOpKind::Or, Value::Bool(a), Value::Bool(b)) => Ok(Value::Bool(*a || *b)),
        (BinOpKind::BitAnd, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a & b)),
        (BinOpKind::BitOr, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a | b)),
        (BinOpKind::BitXor, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a ^ b)),
        (BinOpKind::Shl, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a << b)),
        (BinOpKind::Shr, Value::Int(a), Value::Int(b)) => Ok(Value::Int(a >> b)),
        _ => Err(format!("unsupported binary operation {kind:?} on {left} and {right}")),
    }
}

fn eval_unop(kind: UnOpKind, operand: &Value) -> Result<Value, String> {
    match (kind, operand) {
        (UnOpKind::Neg, Value::Int(n)) => Ok(Value::Int(-n)),
        (UnOpKind::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
        (UnOpKind::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
        (UnOpKind::BitNot, Value::Int(n)) => Ok(Value::Int(!n)),
        _ => Err(format!("unsupported unary operation {kind:?} on {operand}")),
    }
}

fn eval_index(receiver: &Value, index: &Value) -> Result<Value, String> {
    match (receiver, index) {
        (Value::List(list), Value::Int(i)) => {
            let idx = *i as usize;
            Ok(list.get(idx).cloned().unwrap_or(Value::None))
        }
        (Value::Map(map), Value::String(key)) => {
            Ok(map.get(key).cloned().unwrap_or(Value::None))
        }
        (Value::Bytes(bytes), Value::Int(i)) => {
            let idx = *i as usize;
            Ok(bytes.get(idx).map(|b| Value::Int(*b as i64)).unwrap_or(Value::None))
        }
        (Value::String(s), Value::Int(i)) => {
            let idx = *i as usize;
            Ok(s.chars()
                .nth(idx)
                .map(|c| Value::String(c.to_string()))
                .unwrap_or(Value::None))
        }
        _ => Err(format!("cannot index {receiver} with {index}")),
    }
}

fn eval_field_access(receiver: &Value, field: &str) -> Result<Value, String> {
    match receiver {
        Value::Struct { name, fields } => {
            if field == "__struct_name__" {
                return Ok(Value::String(name.clone()));
            }
            Ok(fields.get(field).cloned().unwrap_or(Value::None))
        }
        Value::Map(map) => {
            Ok(map.get(field).cloned().unwrap_or(Value::None))
        }
        _ => Err(format!("cannot access field '{field}' on {receiver}")),
    }
}

fn eval_method_call(receiver: &Value, method: &str, args: &[Value]) -> Result<Value, String> {
    match receiver {
        Value::String(s) => match method {
            "len" => Ok(Value::Int(s.len() as i64)),
            "to_upper" => Ok(Value::String(s.to_uppercase())),
            "to_lower" => Ok(Value::String(s.to_lowercase())),
            "trim" => Ok(Value::String(s.trim().to_string())),
            "contains" => {
                let substr = match args.first() {
                    Some(Value::String(sub)) => sub,
                    _ => return Err("contains requires a String argument".to_string()),
                };
                Ok(Value::Bool(s.contains(substr.as_str())))
            }
            "split" => {
                let sep = match args.first() {
                    Some(Value::String(sep)) => sep.as_str(),
                    _ => return Err("split requires a String argument".to_string()),
                };
                Ok(Value::List(
                    s.split(sep).map(|p| Value::String(p.to_string())).collect(),
                ))
            }
            "to_bytes" => Ok(Value::Bytes(s.as_bytes().to_vec())),
            _ => Err(format!("String has no method '{method}'")),
        },
        Value::List(list) => match method {
            "len" => Ok(Value::Int(list.len() as i64)),
            "contains" => {
                let target = args.first().unwrap_or(&Value::None);
                Ok(Value::Bool(list.contains(target)))
            }
            "push" => {
                let mut new_list = list.clone();
                for arg in args {
                    new_list.push(arg.clone());
                }
                Ok(Value::List(new_list))
            }
            "reverse" => {
                let mut rev = list.clone();
                rev.reverse();
                Ok(Value::List(rev))
            }
            "first" => Ok(list.first().cloned().unwrap_or(Value::None)),
            "last" => Ok(list.last().cloned().unwrap_or(Value::None)),
            "to_bytes" => {
                let bytes: Result<Vec<u8>, String> = list
                    .iter()
                    .map(|v| match v {
                        Value::Int(n) => Ok((*n & 0xFF) as u8),
                        _ => Err("to_bytes: all list elements must be Int (0-255)".to_string()),
                    })
                    .collect();
                Ok(Value::Bytes(bytes?))
            }
            _ => Err(format!("List has no method '{method}'")),
        },
        Value::Bytes(bytes) => match method {
            "len" => Ok(Value::Int(bytes.len() as i64)),
            "at" => {
                let i = match args.first() {
                    Some(Value::Int(i)) => *i,
                    _ => return Err("at requires an Int argument".to_string()),
                };
                if i < 0 || i as usize >= bytes.len() {
                    Err(format!("bytes index {i} out of bounds (length {})", bytes.len()))
                } else {
                    Ok(Value::Int(bytes[i as usize] as i64))
                }
            }
            "slice" => {
                let start = match args.first() {
                    Some(Value::Int(i)) => *i as usize,
                    _ => return Err("slice requires Int start".to_string()),
                };
                let end = match args.get(1) {
                    Some(Value::Int(i)) => *i as usize,
                    _ => return Err("slice requires Int end".to_string()),
                };
                if start > bytes.len() || end > bytes.len() || start > end {
                    Err(format!("bytes slice [{start}..{end}] out of bounds"))
                } else {
                    Ok(Value::Bytes(bytes[start..end].to_vec()))
                }
            }
            "concat" => {
                let other = match args.first() {
                    Some(Value::Bytes(b)) => b,
                    _ => return Err("concat requires a Bytes argument".to_string()),
                };
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
                    return Err("to_int: Bytes length must be <= 8".to_string());
                }
                let mut arr = [0u8; 8];
                arr[8 - bytes.len()..].copy_from_slice(bytes);
                Ok(Value::Int(i64::from_be_bytes(arr)))
            }
            _ => Err(format!("Bytes has no method '{method}'")),
        },
        Value::Int(n) => match method {
            "to_bytes" => {
                let width = match args.first() {
                    Some(Value::Int(w)) => *w,
                    _ => return Err("to_bytes requires an Int width argument".to_string()),
                };
                if width < 1 || width > 8 {
                    return Err("to_bytes width must be 1-8".to_string());
                }
                let bytes = n.to_be_bytes();
                Ok(Value::Bytes(bytes[8 - width as usize..].to_vec()))
            }
            "to_float" => Ok(Value::Float(*n as f64)),
            "to_string" => Ok(Value::String(n.to_string())),
            _ => Err(format!("Int has no method '{method}'")),
        },
        Value::Float(f) => match method {
            "floor" => Ok(Value::Float(f.floor())),
            "ceil" => Ok(Value::Float(f.ceil())),
            "round" => Ok(Value::Float(f.round())),
            "abs" => Ok(Value::Float(f.abs())),
            "sqrt" => Ok(Value::Float(f.sqrt())),
            "to_int" => Ok(Value::Int(*f as i64)),
            "to_string" => Ok(Value::String(f.to_string())),
            _ => Err(format!("Float has no method '{method}'")),
        },
        Value::Ok(inner) => match method {
            "is_ok" => Ok(Value::Bool(true)),
            "is_err" => Ok(Value::Bool(false)),
            "unwrap" => Ok(*inner.clone()),
            "unwrap_err" => Err("called unwrap_err on Ok value".to_string()),
            "unwrap_or" => Ok(*inner.clone()),
            _ => Err(format!("Ok has no method '{method}'")),
        },
        Value::Err(inner) => match method {
            "is_ok" => Ok(Value::Bool(false)),
            "is_err" => Ok(Value::Bool(true)),
            "unwrap" => Err(format!("called unwrap on Err: {}", inner)),
            "unwrap_err" => Ok(*inner.clone()),
            "unwrap_or" => {
                let default = args.first().cloned().unwrap_or(Value::None);
                Ok(default)
            }
            _ => Err(format!("Err has no method '{method}'")),
        },
        Value::Map(map) => match method {
            "len" => Ok(Value::Int(map.len() as i64)),
            "keys" => {
                let keys: Vec<Value> = map.keys().map(|k| Value::String(k.clone())).collect();
                Ok(Value::List(keys))
            }
            "values" => {
                let vals: Vec<Value> = map.values().cloned().collect();
                Ok(Value::List(vals))
            }
            "contains_key" => {
                let key = match args.first() {
                    Some(Value::String(k)) => k,
                    _ => return Err("contains_key requires a String argument".to_string()),
                };
                Ok(Value::Bool(map.contains_key(key)))
            }
            _ => Err(format!("Map has no method '{method}'")),
        },
        _ => Err(format!("cannot call method '{method}' on {receiver}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::compiler::compile_program;

    fn compile_and_run(source: &str) -> Result<Value, String> {
        let tokens = crate::lexer::tokenize(source).map_err(|_| "tokenize error".to_string())?;
        let (mut program, errors) = crate::parser::parse(source, &tokens);
        if !errors.is_empty() {
            return Err(format!("parse error: {}", errors[0].message));
        }
        crate::macro_expand::expand_program(&mut program);
        let compiled = compile_program(&program)?;
        let mut vm = VirtualMachine::new();
        vm.execute(&compiled)
    }

    #[test]
    fn test_vm_arithmetic() {
        let result = compile_and_run("entry() { return 2 + 3 * 4 }").unwrap();
        assert_eq!(result, Value::Int(14));
    }

    #[test]
    fn test_vm_variables() {
        let result = compile_and_run("entry() { let x = 10\n let y = 20\n return x + y }").unwrap();
        assert_eq!(result, Value::Int(30));
    }

    #[test]
    fn test_vm_if_else() {
        let result = compile_and_run("entry() { if true { return 1 } else { return 2 } }").unwrap();
        assert_eq!(result, Value::Int(1));

        let result = compile_and_run("entry() { if false { return 1 } else { return 2 } }").unwrap();
        assert_eq!(result, Value::Int(2));
    }

    #[test]
    fn test_vm_while_loop() {
        let source = r#"
            entry() {
                let sum = 0
                let i = 0
                while i < 5 {
                    let sum = sum + i
                    let i = i + 1
                }
                return sum
            }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(10)); // 0+1+2+3+4
    }

    #[test]
    fn test_vm_function_call() {
        let source = r#"
            fn double(x: Int) -> Int { return x * 2 }
            entry() { return double(21) }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_vm_nested_function_calls() {
        let source = r#"
            fn add(a: Int, b: Int) -> Int { return a + b }
            fn mul(a: Int, b: Int) -> Int { return a * b }
            entry() { return add(mul(2, 3), mul(4, 5)) }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(26)); // 6 + 20
    }

    #[test]
    fn test_vm_string_ops() {
        let result = compile_and_run(r#"entry() { return "hello" + " " + "world" }"#).unwrap();
        assert_eq!(result, Value::String("hello world".to_string()));
    }

    #[test]
    fn test_vm_comparison() {
        let result = compile_and_run("entry() { return 5 > 3 }").unwrap();
        assert_eq!(result, Value::Bool(true));

        let result = compile_and_run("entry() { return 5 == 5 }").unwrap();
        assert_eq!(result, Value::Bool(true));

        let result = compile_and_run("entry() { return 5 != 3 }").unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn test_vm_list_construction() {
        let result = compile_and_run("entry() { return [1, 2, 3] }").unwrap();
        match result {
            Value::List(elems) => {
                assert_eq!(elems.len(), 3);
                assert_eq!(elems[0], Value::Int(1));
                assert_eq!(elems[1], Value::Int(2));
                assert_eq!(elems[2], Value::Int(3));
            }
            other => panic!("expected List, got {other}"),
        }
    }

    #[test]
    fn test_vm_list_index() {
        let result = compile_and_run("entry() { let xs = [10, 20, 30]\n return xs[1] }").unwrap();
        assert_eq!(result, Value::Int(20));
    }

    #[test]
    fn test_vm_map_construction() {
        let source = "entry() {\n  let m = {\"a\": 1, \"b\": 2}\n  return m[\"a\"]\n}";
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(1));
    }

    #[test]
    fn test_vm_unary_ops() {
        let result = compile_and_run("entry() { return -42 }").unwrap();
        assert_eq!(result, Value::Int(-42));

        let result = compile_and_run("entry() { return !true }").unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn test_vm_modulo() {
        let result = compile_and_run("entry() { return 10 % 3 }").unwrap();
        assert_eq!(result, Value::Int(1));
    }

    #[test]
    fn test_vm_match_expression() {
        let source = r#"
            fn describe(x: Int) -> String {
                return match x {
                    1 => "one"
                    2 => "two"
                    _ => "other"
                }
            }
            entry() { return describe(2) }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::String("two".to_string()));
    }

    #[test]
    fn test_vm_match_wildcard() {
        let source = r#"
            fn classify(x: Int) -> String {
                return match x {
                    _ => "anything"
                }
            }
            entry() { return classify(42) }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::String("anything".to_string()));
    }

    #[test]
    fn test_vm_boolean_logic() {
        let result = compile_and_run("entry() { return true && false }").unwrap();
        assert_eq!(result, Value::Bool(false));

        let result = compile_and_run("entry() { return true || false }").unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn test_vm_recursive_function() {
        let source = r#"
            fn factorial(n: Int) -> Int {
                if n <= 1 { return 1 }
                return n * factorial(n - 1)
            }
            entry() { return factorial(5) }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(120));
    }

    #[test]
    fn test_vm_fibonacci() {
        let source = r#"
            fn fib(n: Int) -> Int {
                if n <= 1 { return n }
                return fib(n - 1) + fib(n - 2)
            }
            entry() { return fib(10) }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(55));
    }

    #[test]
    fn test_vm_string_method() {
        let result = compile_and_run(r#"entry() { return "hello".len() }"#).unwrap();
        assert_eq!(result, Value::Int(5));

        let result = compile_and_run(r#"entry() { return "hello".to_upper() }"#).unwrap();
        assert_eq!(result, Value::String("HELLO".to_string()));
    }

    #[test]
    fn test_vm_list_method() {
        let result = compile_and_run("entry() { return [1, 2, 3].len() }").unwrap();
        assert_eq!(result, Value::Int(3));

        let result = compile_and_run("entry() { return [1, 2, 3].contains(2) }").unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn test_vm_service_interaction_rejected() {
        let result = compile_and_run(r#"entry() { let x = resolve KV["kv/main"] }"#);
        assert!(result.is_err());
    }

    #[test]
    fn test_vm_division_by_zero() {
        let result = compile_and_run("entry() { return 10 / 0 }");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("division by zero"));
    }

    #[test]
    fn test_vm_none_value() {
        let result = compile_and_run("entry() { return none }").unwrap();
        assert_eq!(result, Value::None);
    }

    #[test]
    fn test_vm_struct_literal() {
        let source = r#"
            type Point { x: Int  y: Int }
            entry() {
                let p = Point { x: 10, y: 20 }
                return p.x + p.y
            }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(30));
    }

    #[test]
    fn test_vm_closure_basic() {
        let source = r#"
            entry() {
                let f = |x| x * 2
                return f(21)
            }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_vm_closure_capture() {
        let source = r#"
            entry() {
                let y = 10
                let f = |x| x + y
                return f(5)
            }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(15));
    }

    #[test]
    fn test_vm_closure_multi_capture() {
        let source = r#"
            entry() {
                let a = 1
                let b = 2
                let f = |x| x + a + b
                return f(10)
            }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(13));
    }

    #[test]
    fn test_vm_closure_passed_to_fn() {
        let source = r#"
            fn apply(f: Any, x: Int) -> Int { return f(x) }
            entry() {
                let double = |x| x * 2
                return apply(double, 21)
            }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_vm_closure_no_params() {
        let source = r#"
            entry() {
                let x = 42
                let f = || x
                return f()
            }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_vm_closure_multi_params() {
        let source = r#"
            entry() {
                let add = |a, b| a + b
                return add(3, 4)
            }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(7));
    }

    #[test]
    fn test_vm_if_as_expression() {
        // If as last statement = implicit return value
        let source = r#"
            fn choose(b: Bool) -> Int {
                if b { 1 } else { 2 }
            }
            entry() { return choose(true) }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(1));
    }

    #[test]
    fn test_vm_if_as_expression_false() {
        let source = r#"
            fn choose(b: Bool) -> Int {
                if b { 1 } else { 2 }
            }
            entry() { return choose(false) }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(2));
    }

    #[test]
    fn test_vm_if_as_expression_nested() {
        let source = r#"
            fn classify(x: Int) -> Int {
                if x > 3 {
                    if x > 10 { 100 } else { 50 }
                } else {
                    0
                }
            }
            entry() { return classify(5) }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(50));
    }

    #[test]
    fn test_vm_if_implicit_return() {
        let source = r#"
            fn classify(x: Int) -> String {
                if x > 0 { "positive" } else { "non-positive" }
            }
            entry() { return classify(5) }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::String("positive".to_string()));
    }

    #[test]
    fn test_vm_if_no_else_implicit_return() {
        // If without else produces Unit on false branch
        let source = r#"
            fn maybe(b: Bool) -> Int {
                if b { return 42 }
            }
            entry() { return maybe(false) }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Unit);
    }

    #[test]
    fn test_vm_struct_pattern_match() {
        let source = r#"
            type Point { x: Int  y: Int }
            fn describe(p: Point) -> Int {
                match p {
                    Point { x, y } => x + y
                }
            }
            entry() {
                let p = Point { x: 10, y: 20 }
                return describe(p)
            }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(30));
    }

    #[test]
    fn test_vm_list_pattern_match() {
        let source = r#"
            fn first_two(xs: List) -> Int {
                match xs {
                    [a, b] => a + b
                    _ => 0
                }
            }
            entry() { return first_two([3, 7]) }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(10));
    }

    #[test]
    fn test_vm_list_pattern_no_match() {
        let source = r#"
            fn first_two(xs: List) -> Int {
                match xs {
                    [a, b] => a + b
                    _ => 0
                }
            }
            entry() { return first_two([1, 2, 3]) }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(0));
    }

    #[test]
    fn test_vm_list_pattern_rest() {
        let source = r#"
            fn head_and_rest(xs: List) -> Int {
                match xs {
                    [first, ..rest] => first
                    _ => 0
                }
            }
            entry() { return head_and_rest([42, 10, 20]) }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::Int(42));
    }

    #[test]
    fn test_vm_match_guard() {
        let source = r#"
            fn classify(x: Int) -> String {
                match x {
                    n if n > 100 => "big"
                    n if n > 0 => "small"
                    _ => "zero or negative"
                }
            }
            entry() { return classify(50) }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::String("small".to_string()));
    }

    #[test]
    fn test_vm_some_none_pattern() {
        let source = r#"
            fn check(x: Int) -> String {
                match x {
                    Some(v) => "has_value"
                    none => "is_none"
                }
            }
            entry() { return check(42) }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::String("has_value".to_string()));
    }

    #[test]
    fn test_vm_none_pattern_match() {
        let source = r#"
            entry() {
                let x = none
                let result = match x {
                    Some(v) => "has"
                    none => "empty"
                }
                return result
            }
        "#;
        let result = compile_and_run(source).unwrap();
        assert_eq!(result, Value::String("empty".to_string()));
    }
}
