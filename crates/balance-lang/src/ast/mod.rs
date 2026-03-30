use crate::lexer::span::Spanned;

#[derive(Debug, Clone)]
pub struct Program {
    pub module_decl: Option<ModuleDecl>,
    pub imports: Vec<Spanned<ImportDecl>>,
    pub items: Vec<Spanned<Item>>,
}

#[derive(Debug, Clone)]
pub struct ModuleDecl {
    pub path: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ImportDecl {
    pub path: Vec<String>,
    /// Selective imports: `{A, B as C}` → `Some(vec![("A", None), ("B", Some("C"))])`
    pub names: Option<Vec<(String, Option<String>)>>,
    pub alias: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Port(PortDecl),
    Service(ServiceDecl),
    Entry(EntryDecl),
    TypeDecl(TypeDecl),
    FnDecl(FnDecl),
    Substrate(SubstrateDecl),
    Guarantee(GuaranteeDecl),
    Profile(ProfileDecl),
    Macro(MacroDecl),
    Stmt(Stmt),
}

#[derive(Debug, Clone)]
pub struct PortDecl {
    pub exported: bool,
    pub name: String,
    pub methods: Vec<Spanned<PortMethod>>,
}

#[derive(Debug, Clone)]
pub struct PortMethod {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Spanned<TypeExpr>,
    pub annotations: Vec<Annotation>,
}

#[derive(Debug, Clone)]
pub struct Annotation {
    pub name: String,
    pub arg: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ServiceDecl {
    pub exported: bool,
    pub name: String,
    pub provides: String,
    pub items: Vec<Spanned<ServiceItem>>,
}

#[derive(Debug, Clone)]
pub enum ServiceItem {
    Publish(String),
    On(OnClauseDecl),
    Command(CommandImpl),
    Query(QueryImpl),
    Component(ComponentDecl),
    Replicated(u32),
    Version(String),
}

#[derive(Debug, Clone)]
pub struct OnClauseDecl {
    /// Event filter expression (e.g., `replicated(N)` or `quorum_committed`)
    pub filter: Spanned<Expr>,
    /// Optional predicate expression (e.g., `where event["key"] == "expected"`)
    pub where_clause: Option<Spanned<Expr>>,
    /// Optional body to execute when event matches
    pub body: Vec<Spanned<Stmt>>,
}

#[derive(Debug, Clone)]
pub struct CommandImpl {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Spanned<TypeExpr>,
    pub body: CommandBody,
}

#[derive(Debug, Clone)]
pub enum CommandBody {
    Block(Vec<Spanned<Stmt>>),
    ViaSettle {
        via_expr: Spanned<Expr>,
        settle_event: EventRef,
        settle_by: Spanned<Expr>,
    },
}

#[derive(Debug, Clone)]
pub struct QueryImpl {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Spanned<TypeExpr>,
    pub body: QueryBody,
}

#[derive(Debug, Clone)]
pub enum QueryBody {
    Block(Vec<Spanned<Stmt>>),
    ViaObserve {
        via_expr: Spanned<Expr>,
        observe_event: EventRef,
        observe_by: Spanned<Expr>,
        return_expr: Spanned<Expr>,
    },
}

#[derive(Debug, Clone)]
pub struct EventRef {
    pub source: String,
    pub event: String,
}

#[derive(Debug, Clone)]
pub struct ComponentDecl {
    pub name: String,
    pub service: String,
    pub args: Vec<Spanned<Expr>>,
}

#[derive(Debug, Clone)]
pub struct EntryDecl {
    pub name: Option<String>,
    pub params: Vec<Param>,
    pub body: Vec<Spanned<Stmt>>,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Spanned<TypeExpr>,
}

#[derive(Debug, Clone)]
pub struct TypeDecl {
    pub exported: bool,
    pub name: String,
    pub type_params: Vec<String>,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: String,
    pub ty: Spanned<TypeExpr>,
}

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Option<Spanned<TypeExpr>>,
    pub body: Vec<Spanned<Stmt>>,
    pub pure: bool,
}

#[derive(Debug, Clone)]
pub struct SubstrateDep {
    pub local_name: String,
    pub substrate_type: String,
}

#[derive(Debug, Clone)]
pub struct SubstrateDecl {
    pub name: String,
    pub type_params: Vec<String>,
    pub ops: Vec<SubstrateOp>,
    pub emits: Vec<EmitEvent>,
    pub state: Vec<StateBinding>,
    pub deps: Vec<SubstrateDep>,
    pub fns: Vec<FnDecl>,
    pub on_clauses: Vec<OnClauseDecl>,
}

impl SubstrateDecl {
    /// Returns true if this declaration includes an implementation (state + op bodies).
    pub fn has_implementation(&self) -> bool {
        !self.state.is_empty() || self.ops.iter().any(|op| op.body.is_some())
    }
}

#[derive(Debug, Clone)]
pub struct SubstrateOp {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Spanned<TypeExpr>,
    pub body: Option<Vec<Spanned<Stmt>>>,
}

#[derive(Debug, Clone)]
pub struct StateBinding {
    pub name: String,
    pub ty: Option<Spanned<TypeExpr>>,
    pub initial_value: Spanned<Expr>,
    pub mutable: bool,
}

#[derive(Debug, Clone)]
pub struct EmitEvent {
    pub name: String,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GuaranteeDecl {
    pub name: String,
    pub laws: Vec<LawDecl>,
}

#[derive(Debug, Clone)]
pub struct LawDecl {
    pub body: String,
    pub parsed: Option<LawBody>,
}

/// Authority qualifier for capability types.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AuthorityQualifier {
    /// `@consume` — exclusive ownership, capability is consumed on use
    Consume,
    /// `@borrow` — temporary access, cannot be stored or forwarded
    Borrow,
    /// `@delegate` — can be passed to other services
    Delegate,
}

#[derive(Debug, Clone)]
pub enum TypeExpr {
    Named {
        name: String,
        type_args: Vec<Spanned<TypeExpr>>,
        nullable: bool,
    },
    Cap {
        port_name: String,
        qualifier: Option<AuthorityQualifier>,
    },
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let {
        name: String,
        ty: Option<Spanned<TypeExpr>>,
        value: Spanned<Expr>,
        mutable: bool,
    },
    Return(Option<Spanned<Expr>>),
    If {
        condition: Spanned<Expr>,
        then_block: Vec<Spanned<Stmt>>,
        else_block: Option<Vec<Spanned<Stmt>>>,
    },
    Match {
        expr: Spanned<Expr>,
        arms: Vec<MatchArm>,
    },
    For {
        variable: String,
        iterable: Spanned<Expr>,
        body: Vec<Spanned<Stmt>>,
    },
    While {
        condition: Spanned<Expr>,
        body: Vec<Spanned<Stmt>>,
    },
    Break,
    Continue,
    Expr(Spanned<Expr>),
    Assign {
        name: String,
        value: Spanned<Expr>,
    },
    Emit {
        event_type: String,
        fields: Vec<(String, Spanned<Expr>)>,
    },
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Spanned<Pattern>,
    pub guard: Option<Spanned<Expr>>,
    pub body: Vec<Spanned<Stmt>>,
}

#[derive(Debug, Clone)]
pub struct SelectArm {
    pub binding: String,
    pub expr: Spanned<Expr>,
    pub body: Vec<Spanned<Stmt>>,
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Ident(String),
    Literal(Literal),
    Wildcard,
    Struct {
        name: String,
        fields: Vec<(String, Option<Spanned<Pattern>>)>,
    },
    List {
        elements: Vec<Spanned<Pattern>>,
        rest: Option<Box<Spanned<Pattern>>>,
    },
    /// `Some(inner)` — matches any non-None value and binds inner
    Some(Box<Spanned<Pattern>>),
    /// `None` — matches only Value::None
    None,
    /// `Ok(inner)` — matches Value::Ok(v) and binds inner
    Ok(Box<Spanned<Pattern>>),
    /// `Err(inner)` — matches Value::Err(v) and binds inner
    Err(Box<Spanned<Pattern>>),
}

#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Literal),
    None,
    Ident(String),
    Resolve {
        port: String,
        name: Box<Spanned<Expr>>,
        profile: Option<String>,
    },
    MethodCall {
        receiver: Box<Spanned<Expr>>,
        method: String,
        args: Vec<Spanned<Expr>>,
    },
    FieldAccess {
        receiver: Box<Spanned<Expr>>,
        field: String,
    },
    FnCall {
        func: Box<Spanned<Expr>>,
        args: Vec<Spanned<Expr>>,
    },
    Block(Vec<Spanned<Stmt>>),
    Unary {
        op: UnaryOp,
        operand: Box<Spanned<Expr>>,
    },
    Binary {
        op: BinaryOp,
        left: Box<Spanned<Expr>>,
        right: Box<Spanned<Expr>>,
    },
    StructLiteral {
        name: String,
        fields: Vec<(String, Spanned<Expr>)>,
    },
    Await {
        expr: Box<Spanned<Expr>>,
    },
    ConcurrentAwait {
        exprs: Vec<Spanned<Expr>>,
    },
    ListLiteral {
        elements: Vec<Spanned<Expr>>,
    },
    MapLiteral {
        entries: Vec<(Spanned<Expr>, Spanned<Expr>)>,
    },
    Closure {
        params: Vec<Param>,
        body: Vec<Spanned<Stmt>>,
    },
    MacroCall {
        name: String,
        args: Vec<Spanned<Expr>>,
    },
    DynamicImport {
        path: Box<Spanned<Expr>>,
    },
    Index {
        receiver: Box<Spanned<Expr>>,
        index: Box<Spanned<Expr>>,
    },
    Match {
        expr: Box<Spanned<Expr>>,
        arms: Vec<MatchArm>,
    },
    /// `expr?` — unwrap Ok, propagate Err
    Try {
        expr: Box<Spanned<Expr>>,
    },
    /// `select timeout { binding = expr => { body } ... else => { body } }`
    Select {
        timeout_ms: Option<Box<Spanned<Expr>>>,
        arms: Vec<SelectArm>,
        else_body: Option<Vec<Spanned<Stmt>>>,
    },
}

#[derive(Debug, Clone)]
pub enum Literal {
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinaryOp {
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

#[derive(Debug, Clone)]
pub struct ProfileDecl {
    pub name: String,
    pub preferences: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct MacroDecl {
    pub name: String,
    pub params: Vec<MacroParam>,
    pub body: Vec<Spanned<Stmt>>,
}

#[derive(Debug, Clone)]
pub struct MacroParam {
    pub name: String,
    pub kind: MacroParamKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MacroParamKind {
    Expr,
    Stmt,
    Ident,
    Block,
}

/// Law body: either raw text or parsed structure.
#[derive(Debug, Clone)]
pub enum LawBody {
    Text(String),
    Parsed {
        antecedent: String,
        consequent: String,
        binding: Option<String>,
        window_ms: Option<u64>,
    },
}

impl BinaryOp {
    pub fn precedence(self) -> u8 {
        match self {
            BinaryOp::Or => 1,
            BinaryOp::And => 2,
            BinaryOp::BitOr => 3,
            BinaryOp::BitXor => 4,
            BinaryOp::BitAnd => 5,
            BinaryOp::Eq | BinaryOp::Neq => 6,
            BinaryOp::Lt | BinaryOp::Gt | BinaryOp::LtEq | BinaryOp::GtEq => 7,
            BinaryOp::Shl | BinaryOp::Shr => 8,
            BinaryOp::Add | BinaryOp::Sub => 9,
            BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => 10,
        }
    }
}
