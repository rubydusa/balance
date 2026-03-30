use crate::runtime::value::Value;

/// Bytecode operation. Each instruction operates on a value stack.
#[derive(Debug, Clone)]
pub enum Op {
    /// Push a constant value onto the stack.
    Const(Value),
    /// Load a local variable by slot index onto the stack.
    Load(usize),
    /// Store the top of stack into a local variable slot.
    Store(usize),
    /// Pop two values, apply binary operation, push result.
    BinOp(BinOpKind),
    /// Pop one value, apply unary operation, push result.
    UnOp(UnOpKind),
    /// Unconditional jump to instruction index.
    Jump(usize),
    /// Pop top of stack; if falsy, jump to instruction index.
    JumpIfFalse(usize),
    /// Call function by index with N arguments.
    /// Pops N args + function reference from stack, pushes result.
    Call(usize),
    /// Return from current function with top-of-stack value.
    Return,
    /// Pop and discard the top of stack.
    Pop,
    /// Pop N values from stack, create a list, push it.
    MakeList(usize),
    /// Pop 2*N values (key, value pairs), create a map, push it.
    MakeMap(usize),
    /// Pop N values + struct name, create struct, push it.
    MakeStruct {
        name: String,
        field_count: usize,
        field_names: Vec<String>,
    },
    /// Pop index + receiver, push receiver[index].
    Index,
    /// Pop receiver, push receiver.field.
    GetField(String),
    /// Pop receiver + N args, call receiver.method(args), push result.
    MethodCall {
        method: String,
        arg_count: usize,
    },
    /// Halt execution.
    Halt,
    /// No operation (used for patching).
    Nop,
    /// Duplicate the top of stack.
    Dup,
    /// Create a closure: pop `capture_count` values from stack as captures,
    /// push a ClosureRef. `func_index` is the compiled function index.
    MakeClosure {
        func_index: usize,
        capture_count: usize,
    },
    /// Call a closure with N arguments. Pops N args + closure ref from stack.
    CallClosure(usize),
    /// Pop top, wrap in Ok(...), push.
    WrapOk,
    /// Pop top, wrap in Err(...), push.
    WrapErr,
    /// Pop top: if Ok(v), push v. If Err(e), return Err(e).
    TryUnwrap,
}

/// Binary operation kinds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOpKind {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Neq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

/// Unary operation kinds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnOpKind {
    Neg,
    Not,
    BitNot,
}

/// A compiled function.
#[derive(Debug, Clone)]
pub struct CompiledFunction {
    pub name: String,
    pub arity: usize,
    pub locals_count: usize,
    pub code: Vec<Op>,
}

/// Metadata about a type declaration in the compiled program.
#[derive(Debug, Clone)]
pub struct TypeInfo {
    pub name: String,
    pub fields: Vec<(String, String)>,
    pub type_params: Vec<String>,
}

/// Metadata about a service declaration in the compiled program.
#[derive(Debug, Clone)]
pub struct ServiceInfo {
    pub name: String,
    pub port: String,
    pub publish_id: Option<String>,
}

/// Metadata about a substrate binding in the compiled program.
#[derive(Debug, Clone)]
pub struct SubstrateBinding {
    pub name: String,
    pub substrate_type: String,
}

/// Metadata about an import in the compiled program.
#[derive(Debug, Clone)]
pub struct ImportInfo {
    pub path: Vec<String>,
    pub names: Vec<String>,
}

/// Metadata about a port declaration in the compiled program.
#[derive(Debug, Clone)]
pub struct PortInfo {
    pub name: String,
    /// (method_name, param_count, kind) where kind is "command", "query", or "unknown".
    pub methods: Vec<(String, usize, String)>,
}

/// Metadata about a guarantee declaration in the compiled program.
#[derive(Debug, Clone)]
pub struct GuaranteeInfo {
    pub name: String,
    pub law_descriptions: Vec<String>,
}

/// A compiled program ready for VM execution.
#[derive(Debug, Clone)]
pub struct CompiledProgram {
    pub functions: Vec<CompiledFunction>,
    pub entry_index: Option<usize>,
    pub constants: Vec<Value>,
    /// Module name (from `module` declaration), if present.
    pub module_name: Option<String>,
    /// Type declarations found in the program.
    pub types: Vec<TypeInfo>,
    /// Service declarations found in the program.
    pub services: Vec<ServiceInfo>,
    /// Substrate declarations found in the program.
    pub substrates: Vec<SubstrateBinding>,
    /// Import declarations found in the program.
    pub imports: Vec<ImportInfo>,
    /// Transitive module dependency paths (resolved from imports).
    pub dependencies: Vec<String>,
    /// Port declarations found in the program.
    pub ports: Vec<PortInfo>,
    /// Guarantee declarations found in the program.
    pub guarantees: Vec<GuaranteeInfo>,
    /// Dependency graph edges: (service_name, port_name) — service provides port.
    pub dependency_graph: Vec<(String, String)>,
}

impl CompiledProgram {
    pub fn new() -> Self {
        Self {
            functions: Vec::new(),
            entry_index: None,
            constants: Vec::new(),
            module_name: None,
            types: Vec::new(),
            services: Vec::new(),
            substrates: Vec::new(),
            imports: Vec::new(),
            dependencies: Vec::new(),
            ports: Vec::new(),
            guarantees: Vec::new(),
            dependency_graph: Vec::new(),
        }
    }
}

impl Default for CompiledProgram {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_op_debug() {
        let op = Op::Const(Value::Int(42));
        let s = format!("{op:?}");
        assert!(s.contains("Const"));
        assert!(s.contains("42"));
    }

    #[test]
    fn test_compiled_program_default() {
        let prog = CompiledProgram::new();
        assert!(prog.functions.is_empty());
        assert!(prog.entry_index.is_none());
        assert!(prog.constants.is_empty());
    }

    #[test]
    fn test_compiled_function() {
        let func = CompiledFunction {
            name: "add".to_string(),
            arity: 2,
            locals_count: 2,
            code: vec![
                Op::Load(0),
                Op::Load(1),
                Op::BinOp(BinOpKind::Add),
                Op::Return,
            ],
        };
        assert_eq!(func.name, "add");
        assert_eq!(func.arity, 2);
        assert_eq!(func.code.len(), 4);
    }

    #[test]
    fn test_compiled_program_metadata_defaults() {
        let prog = CompiledProgram::new();
        assert!(prog.module_name.is_none());
        assert!(prog.types.is_empty());
        assert!(prog.services.is_empty());
        assert!(prog.substrates.is_empty());
        assert!(prog.imports.is_empty());
        assert!(prog.ports.is_empty());
        assert!(prog.guarantees.is_empty());
        assert!(prog.dependency_graph.is_empty());
    }
}
