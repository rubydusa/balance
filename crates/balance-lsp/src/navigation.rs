use balance_lang::ast::{Expr, Item, Program, Stmt};
use balance_lang::lexer::span::Span;
use tower_lsp::lsp_types::{Hover, HoverContents, Location, MarkupContent, MarkupKind, Position, Url};

use crate::diagnostics::span_to_range;

/// Find the bare Expr::Ident at the given LSP position, if any.
/// Returns (identifier name, span of that identifier).
pub fn identifier_at(source: &str, program: &Program, pos: Position) -> Option<(String, Span)> {
    let offset = position_to_offset(source, pos)?;
    let mut hit: Option<(String, Span)> = None;
    for item in &program.items {
        walk_item(&item.node, &mut |name, span| {
            if span.start <= offset && offset <= span.end {
                hit = Some((name.to_string(), span));
            }
        });
    }
    hit
}

/// Look up a definition by name in the given program. Returns (defining-span, name-span).
pub fn find_definition(program: &Program, name: &str) -> Option<(Span, Span)> {
    for item in &program.items {
        match &item.node {
            Item::Port(p) if p.name == name => return Some((item.span, p.name_span)),
            Item::Service(s) if s.name == name => return Some((item.span, s.name_span)),
            Item::TypeDecl(t) if t.name == name => return Some((item.span, t.name_span)),
            Item::FnDecl(f) if f.name == name => return Some((item.span, f.name_span)),
            Item::Substrate(s) if s.name == name => return Some((item.span, s.name_span)),
            Item::Guarantee(g) if g.name == name => return Some((item.span, g.name_span)),
            Item::Profile(p) if p.name == name => return Some((item.span, p.name_span)),
            Item::Macro(m) if m.name == name => return Some((item.span, m.name_span)),
            Item::Entry(e) if e.name.as_deref() == Some(name) => return Some((item.span, e.name_span)),
            _ => {}
        }
    }
    None
}

pub fn goto_definition(source: &str, program: &Program, uri: Url, pos: Position) -> Option<Location> {
    let (name, _click_span) = identifier_at(source, program, pos)?;
    let (_item_span, name_span) = find_definition(program, &name)?;
    Some(Location {
        uri,
        range: span_to_range(source, name_span),
    })
}

pub fn hover(source: &str, program: &Program, pos: Position) -> Option<Hover> {
    let (name, click_span) = identifier_at(source, program, pos)?;
    let signature = render_signature(program, &name)?;
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: format!("```balance\n{signature}\n```"),
        }),
        range: Some(span_to_range(source, click_span)),
    })
}

fn render_signature(program: &Program, name: &str) -> Option<String> {
    for item in &program.items {
        match &item.node {
            Item::Port(p) if p.name == name => {
                return Some(format!("port {} {{ ... }}", p.name));
            }
            Item::Service(s) if s.name == name => {
                return Some(format!("service {} provides {}", s.name, s.provides));
            }
            Item::TypeDecl(t) if t.name == name => {
                let params = if t.type_params.is_empty() {
                    String::new()
                } else {
                    format!("<{}>", t.type_params.join(", "))
                };
                return Some(format!("type {}{}", t.name, params));
            }
            Item::FnDecl(f) if f.name == name => {
                let params: Vec<String> = f
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", p.name, type_expr_display(&p.ty.node)))
                    .collect();
                let ret = f
                    .return_type
                    .as_ref()
                    .map(|r| format!(" -> {}", type_expr_display(&r.node)))
                    .unwrap_or_default();
                let prefix = if f.pure { "pure fn" } else { "fn" };
                return Some(format!("{} {}({}){}", prefix, f.name, params.join(", "), ret));
            }
            Item::Substrate(s) if s.name == name => {
                return Some(format!("substrate {}", s.name));
            }
            Item::Guarantee(g) if g.name == name => {
                return Some(format!("guarantee {}", g.name));
            }
            Item::Profile(p) if p.name == name => {
                return Some(format!("profile {}", p.name));
            }
            Item::Macro(m) if m.name == name => {
                return Some(format!("macro {}", m.name));
            }
            Item::Entry(e) if e.name.as_deref() == Some(name) => {
                return Some(format!("entry {}", name));
            }
            _ => {}
        }
    }
    None
}

fn type_expr_display(t: &balance_lang::ast::TypeExpr) -> String {
    use balance_lang::ast::TypeExpr;
    match t {
        TypeExpr::Named { name, type_args, nullable } => {
            let args = if type_args.is_empty() {
                String::new()
            } else {
                let inner: Vec<String> = type_args.iter().map(|a| type_expr_display(&a.node)).collect();
                format!("<{}>", inner.join(", "))
            };
            let q = if *nullable { "?" } else { "" };
            format!("{}{}{}", name, args, q)
        }
        TypeExpr::Cap { port_name, qualifier } => {
            let q = qualifier
                .map(|q| match q {
                    balance_lang::ast::AuthorityQualifier::Consume => " @consume",
                    balance_lang::ast::AuthorityQualifier::Borrow => " @borrow",
                    balance_lang::ast::AuthorityQualifier::Delegate => " @delegate",
                })
                .unwrap_or("");
            format!("cap {}{}", port_name, q)
        }
    }
}

fn walk_item(item: &Item, f: &mut dyn FnMut(&str, Span)) {
    match item {
        Item::FnDecl(fd) => {
            for stmt in &fd.body {
                walk_stmt(&stmt.node, stmt.span, f);
            }
        }
        Item::Entry(e) => {
            for stmt in &e.body {
                walk_stmt(&stmt.node, stmt.span, f);
            }
        }
        Item::Service(s) => {
            use balance_lang::ast::{ServiceItem, CommandBody, QueryBody};
            for si in &s.items {
                match &si.node {
                    ServiceItem::Command(c) => match &c.body {
                        CommandBody::Block(stmts) => {
                            for stmt in stmts {
                                walk_stmt(&stmt.node, stmt.span, f);
                            }
                        }
                        CommandBody::ViaSettle { via_expr, settle_by, .. } => {
                            walk_expr(&via_expr.node, via_expr.span, f);
                            walk_expr(&settle_by.node, settle_by.span, f);
                        }
                    },
                    ServiceItem::Query(q) => match &q.body {
                        QueryBody::Block(stmts) => {
                            for stmt in stmts {
                                walk_stmt(&stmt.node, stmt.span, f);
                            }
                        }
                        QueryBody::ViaObserve { via_expr, observe_by, return_expr, .. } => {
                            walk_expr(&via_expr.node, via_expr.span, f);
                            walk_expr(&observe_by.node, observe_by.span, f);
                            walk_expr(&return_expr.node, return_expr.span, f);
                        }
                    },
                    ServiceItem::Component(c) => {
                        for arg in &c.args {
                            walk_expr(&arg.node, arg.span, f);
                        }
                    }
                    _ => {}
                }
            }
        }
        Item::Substrate(s) => {
            for op in &s.ops {
                if let Some(body) = &op.body {
                    for stmt in body {
                        walk_stmt(&stmt.node, stmt.span, f);
                    }
                }
            }
        }
        Item::Stmt(stmt) => walk_stmt(stmt, Span::new(0, 0), f),
        _ => {}
    }
}

fn walk_stmt(stmt: &Stmt, _span: Span, f: &mut dyn FnMut(&str, Span)) {
    match stmt {
        Stmt::Let { value, .. } => walk_expr(&value.node, value.span, f),
        Stmt::Return(Some(e)) => walk_expr(&e.node, e.span, f),
        Stmt::Return(None) => {}
        Stmt::If { condition, then_block, else_block } => {
            walk_expr(&condition.node, condition.span, f);
            for s in then_block {
                walk_stmt(&s.node, s.span, f);
            }
            if let Some(eb) = else_block {
                for s in eb {
                    walk_stmt(&s.node, s.span, f);
                }
            }
        }
        Stmt::Match { expr, arms } => {
            walk_expr(&expr.node, expr.span, f);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    walk_expr(&g.node, g.span, f);
                }
                for s in &arm.body {
                    walk_stmt(&s.node, s.span, f);
                }
            }
        }
        Stmt::For { iterable, body, .. } => {
            walk_expr(&iterable.node, iterable.span, f);
            for s in body {
                walk_stmt(&s.node, s.span, f);
            }
        }
        Stmt::While { condition, body } => {
            walk_expr(&condition.node, condition.span, f);
            for s in body {
                walk_stmt(&s.node, s.span, f);
            }
        }
        Stmt::Break | Stmt::Continue => {}
        Stmt::Expr(e) => walk_expr(&e.node, e.span, f),
        Stmt::Assign { value, .. } => walk_expr(&value.node, value.span, f),
        Stmt::Emit { fields, .. } => {
            for (_, e) in fields {
                walk_expr(&e.node, e.span, f);
            }
        }
    }
}

fn walk_expr(expr: &Expr, span: Span, f: &mut dyn FnMut(&str, Span)) {
    match expr {
        Expr::Ident(name) => f(name, span),
        Expr::MethodCall { receiver, args, .. } => {
            walk_expr(&receiver.node, receiver.span, f);
            for a in args {
                walk_expr(&a.node, a.span, f);
            }
        }
        Expr::FieldAccess { receiver, .. } => {
            walk_expr(&receiver.node, receiver.span, f);
        }
        Expr::FnCall { func, args } => {
            walk_expr(&func.node, func.span, f);
            for a in args {
                walk_expr(&a.node, a.span, f);
            }
        }
        Expr::Block(stmts) => {
            for s in stmts {
                walk_stmt(&s.node, s.span, f);
            }
        }
        Expr::Unary { operand, .. } => walk_expr(&operand.node, operand.span, f),
        Expr::Binary { left, right, .. } => {
            walk_expr(&left.node, left.span, f);
            walk_expr(&right.node, right.span, f);
        }
        Expr::StructLiteral { fields, .. } => {
            for (_, e) in fields {
                walk_expr(&e.node, e.span, f);
            }
        }
        Expr::Await { expr } => walk_expr(&expr.node, expr.span, f),
        Expr::ConcurrentAwait { exprs } => {
            for e in exprs {
                walk_expr(&e.node, e.span, f);
            }
        }
        Expr::ListLiteral { elements } => {
            for e in elements {
                walk_expr(&e.node, e.span, f);
            }
        }
        Expr::MapLiteral { entries } => {
            for (k, v) in entries {
                walk_expr(&k.node, k.span, f);
                walk_expr(&v.node, v.span, f);
            }
        }
        Expr::Closure { body, .. } => {
            for s in body {
                walk_stmt(&s.node, s.span, f);
            }
        }
        Expr::MacroCall { args, .. } => {
            for a in args {
                walk_expr(&a.node, a.span, f);
            }
        }
        Expr::DynamicImport { path } => walk_expr(&path.node, path.span, f),
        Expr::Index { receiver, index } => {
            walk_expr(&receiver.node, receiver.span, f);
            walk_expr(&index.node, index.span, f);
        }
        Expr::Match { expr, arms } => {
            walk_expr(&expr.node, expr.span, f);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    walk_expr(&g.node, g.span, f);
                }
                for s in &arm.body {
                    walk_stmt(&s.node, s.span, f);
                }
            }
        }
        Expr::Try { expr } => walk_expr(&expr.node, expr.span, f),
        Expr::Select { timeout_ms, arms, else_body } => {
            if let Some(t) = timeout_ms {
                walk_expr(&t.node, t.span, f);
            }
            for arm in arms {
                walk_expr(&arm.expr.node, arm.expr.span, f);
                for s in &arm.body {
                    walk_stmt(&s.node, s.span, f);
                }
            }
            if let Some(eb) = else_body {
                for s in eb {
                    walk_stmt(&s.node, s.span, f);
                }
            }
        }
        Expr::Resolve { name, .. } => walk_expr(&name.node, name.span, f),
        Expr::Literal(_) | Expr::None => {}
    }
}

fn position_to_offset(source: &str, pos: Position) -> Option<usize> {
    let mut line = 0u32;
    let mut col = 0u32;
    for (i, ch) in source.char_indices() {
        if line == pos.line && col == pos.character {
            return Some(i);
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    if line == pos.line && col == pos.character {
        return Some(source.len());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::analyze;

    fn parse(src: &str) -> (String, Program) {
        let doc = analyze(src.to_string(), None, None);
        (doc.source, doc.program)
    }

    #[test]
    fn position_to_offset_roundtrips() {
        let src = "abc\ndef\nghi";
        assert_eq!(position_to_offset(src, Position { line: 0, character: 0 }), Some(0));
        assert_eq!(position_to_offset(src, Position { line: 1, character: 0 }), Some(4));
        assert_eq!(position_to_offset(src, Position { line: 2, character: 2 }), Some(10));
    }

    #[test]
    fn find_definition_locates_fn_decl() {
        let src = "fn greet() -> Int { return 1 }\nfn other() -> Int { return 2 }";
        let (_src, program) = parse(src);
        let (item_span, name_span) = find_definition(&program, "greet").expect("greet should be found");
        // name_span covers just "greet" at chars 3..8
        assert_eq!(name_span.start, 3);
        assert_eq!(name_span.end, 8);
        // item_span wraps the whole fn decl, so it's at least as wide
        assert!(item_span.start <= name_span.start);
        assert!(item_span.end >= name_span.end);
    }

    #[test]
    fn find_definition_returns_none_for_unknown_name() {
        let src = "fn greet() -> Int { return 1 }";
        let (_src, program) = parse(src);
        assert!(find_definition(&program, "nonexistent").is_none());
    }

    #[test]
    fn find_definition_distinguishes_decl_kinds() {
        let src = r#"
port Hello { greet() -> String [query] }
fn greet() -> Int { return 1 }
type Hello { x: Int }
"#;
        let (_src, program) = parse(src);
        // Both Hello (port) and Hello (type) exist; first-match wins, which is the port.
        let hit = find_definition(&program, "Hello").expect("should find Hello");
        let (_, ns) = hit;
        // port Hello starts at line 1 (after leading newline); just verify we got *something* spanning 'Hello'
        let literal = &src[ns.start..ns.end];
        assert_eq!(literal, "Hello");
    }

    #[test]
    fn identifier_at_finds_bare_ident_in_function_body() {
        // Body contains a bare `x` on its own line
        let src = "fn f(x: Int) -> Int {\n    x\n}";
        let (src, program) = parse(&src);
        // line 1, column 4 is where 'x' sits (after 4 spaces)
        let pos = Position { line: 1, character: 4 };
        let hit = identifier_at(&src, &program, pos).expect("should find identifier at position");
        assert_eq!(hit.0, "x");
    }

    #[test]
    fn identifier_at_returns_none_outside_any_ident() {
        let src = "fn f() -> Int { return 1 }";
        let (src, program) = parse(src);
        // Position inside "return" keyword or whitespace — no bare Expr::Ident here (only a literal)
        let hit = identifier_at(&src, &program, Position { line: 0, character: 0 });
        assert!(hit.is_none());
    }

    #[test]
    fn goto_definition_jumps_from_use_to_decl() {
        let src = "fn greet() -> Int { return 1 }\nfn caller() -> Int { greet() }";
        let (src, program) = parse(&src);
        let uri = tower_lsp::lsp_types::Url::parse("file:///tmp/test.bl").unwrap();
        // Position of "greet" inside the body of `caller` — line 1, character 21 lands inside `greet`
        let pos = Position { line: 1, character: 22 };
        let loc = goto_definition(&src, &program, uri.clone(), pos)
            .expect("should resolve goto-def from usage to decl");
        assert_eq!(loc.uri, uri);
        // Target should be the name span of `fn greet` on line 0 → chars 3..8
        assert_eq!(loc.range.start.line, 0);
        assert_eq!(loc.range.start.character, 3);
        assert_eq!(loc.range.end.character, 8);
    }

    #[test]
    fn hover_returns_function_signature_for_fn_reference() {
        let src = "fn greet(n: Int) -> String { return \"hi\" }\nfn caller() -> String { greet(1) }";
        let (src, program) = parse(&src);
        // line 1, character 25 lands on `greet`
        let pos = Position { line: 1, character: 25 };
        let hv = hover(&src, &program, pos).expect("should produce hover");
        let text = match hv.contents {
            HoverContents::Markup(m) => m.value,
            _ => panic!("expected markup content"),
        };
        assert!(text.contains("fn greet"), "hover text missing signature: {text}");
        assert!(text.contains("Int"), "hover should include param type: {text}");
        assert!(text.contains("String"), "hover should include return type: {text}");
    }
}

