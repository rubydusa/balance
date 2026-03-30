use std::collections::HashMap;

use crate::ast::*;
use crate::lexer::span::Spanned;

/// Expand all macros in a program. Collects macro definitions, removes them
/// from the item list, and replaces all MacroCall expressions with their
/// expanded bodies.
pub fn expand_program(program: &mut Program) {
    // 1. Collect macro definitions
    let mut macros: HashMap<String, MacroDecl> = HashMap::new();
    for item in &program.items {
        if let Item::Macro(m) = &item.node {
            macros.insert(m.name.clone(), m.clone());
        }
    }

    if macros.is_empty() {
        return;
    }

    // 2. Remove macro definitions from items
    program.items.retain(|item| !matches!(&item.node, Item::Macro(_)));

    // 3. Walk all items and expand macro calls
    for item in &mut program.items {
        expand_item(&mut item.node, &macros);
    }
}

fn expand_item(item: &mut Item, macros: &HashMap<String, MacroDecl>) {
    match item {
        Item::Service(s) => {
            for si in &mut s.items {
                expand_service_item(&mut si.node, macros);
            }
        }
        Item::Entry(e) => {
            expand_stmts(&mut e.body, macros);
        }
        Item::FnDecl(f) => {
            expand_stmts(&mut f.body, macros);
        }
        Item::Stmt(s) => {
            expand_stmt(s, macros);
        }
        _ => {}
    }
}

fn expand_service_item(si: &mut ServiceItem, macros: &HashMap<String, MacroDecl>) {
    match si {
        ServiceItem::Command(cmd) => {
            if let CommandBody::Block(stmts) = &mut cmd.body {
                expand_stmts(stmts, macros);
            }
        }
        ServiceItem::Query(q) => {
            if let QueryBody::Block(stmts) = &mut q.body {
                expand_stmts(stmts, macros);
            }
        }
        ServiceItem::On(on) => {
            if let Some(ref mut where_expr) = on.where_clause {
                expand_expr(&mut where_expr.node, macros);
            }
            expand_stmts(&mut on.body, macros);
        }
        _ => {}
    }
}

fn expand_stmts(stmts: &mut Vec<Spanned<Stmt>>, macros: &HashMap<String, MacroDecl>) {
    for stmt in stmts.iter_mut() {
        expand_stmt(&mut stmt.node, macros);
    }
}

fn expand_stmt(stmt: &mut Stmt, macros: &HashMap<String, MacroDecl>) {
    match stmt {
        Stmt::Let { value, .. } => {
            expand_expr(&mut value.node, macros);
        }
        Stmt::Return(Some(expr)) => {
            expand_expr(&mut expr.node, macros);
        }
        Stmt::If {
            condition,
            then_block,
            else_block,
        } => {
            expand_expr(&mut condition.node, macros);
            expand_stmts(then_block, macros);
            if let Some(eb) = else_block {
                expand_stmts(eb, macros);
            }
        }
        Stmt::Match { expr, arms } => {
            expand_expr(&mut expr.node, macros);
            for arm in arms {
                expand_pattern(&mut arm.pattern.node, macros);
                if let Some(ref mut guard) = arm.guard {
                    expand_expr(&mut guard.node, macros);
                }
                expand_stmts(&mut arm.body, macros);
            }
        }
        Stmt::For {
            iterable, body, ..
        } => {
            expand_expr(&mut iterable.node, macros);
            expand_stmts(body, macros);
        }
        Stmt::While { condition, body } => {
            expand_expr(&mut condition.node, macros);
            expand_stmts(body, macros);
        }
        Stmt::Expr(expr) => {
            expand_expr(&mut expr.node, macros);
        }
        _ => {}
    }
}

fn expand_expr(expr: &mut Expr, macros: &HashMap<String, MacroDecl>) {
    // First expand sub-expressions
    match expr {
        Expr::MethodCall {
            receiver, args, ..
        } => {
            expand_expr(&mut receiver.node, macros);
            for arg in args {
                expand_expr(&mut arg.node, macros);
            }
        }
        Expr::FieldAccess { receiver, .. } => {
            expand_expr(&mut receiver.node, macros);
        }
        Expr::FnCall { func, args } => {
            expand_expr(&mut func.node, macros);
            for arg in args {
                expand_expr(&mut arg.node, macros);
            }
        }
        Expr::Block(stmts) => {
            expand_stmts(stmts, macros);
        }
        Expr::Unary { operand, .. } => {
            expand_expr(&mut operand.node, macros);
        }
        Expr::Binary { left, right, .. } => {
            expand_expr(&mut left.node, macros);
            expand_expr(&mut right.node, macros);
        }
        Expr::StructLiteral { fields, .. } => {
            for (_, fexpr) in fields {
                expand_expr(&mut fexpr.node, macros);
            }
        }
        Expr::Await { expr: inner } => {
            expand_expr(&mut inner.node, macros);
        }
        Expr::ConcurrentAwait { exprs } => {
            for e in exprs {
                expand_expr(&mut e.node, macros);
            }
        }
        Expr::ListLiteral { elements } => {
            for e in elements {
                expand_expr(&mut e.node, macros);
            }
        }
        Expr::MapLiteral { entries } => {
            for (k, v) in entries {
                expand_expr(&mut k.node, macros);
                expand_expr(&mut v.node, macros);
            }
        }
        Expr::Closure { body, .. } => {
            expand_stmts(body, macros);
        }
        Expr::DynamicImport { path } => {
            expand_expr(&mut path.node, macros);
        }
        Expr::Resolve { name, .. } => {
            expand_expr(&mut name.node, macros);
        }
        Expr::Index { receiver, index } => {
            expand_expr(&mut receiver.node, macros);
            expand_expr(&mut index.node, macros);
        }
        Expr::Match { expr, arms } => {
            expand_expr(&mut expr.node, macros);
            for arm in arms {
                expand_pattern(&mut arm.pattern.node, macros);
                if let Some(ref mut guard) = arm.guard {
                    expand_expr(&mut guard.node, macros);
                }
                expand_stmts(&mut arm.body, macros);
            }
        }
        Expr::Try { expr } => {
            expand_expr(&mut expr.node, macros);
        }
        _ => {} // Literal, None, Ident, MacroCall (handled below), StructLiteral name — no sub-expressions
    }

    // Then check if this expression itself is a macro call
    if let Expr::MacroCall { name, args } = expr {
        if let Some(macro_def) = macros.get(name.as_str()) {
            if let Some(expanded) = expand_macro_call(macro_def, args, macros) {
                *expr = expanded;
            }
        }
    }
}

/// Expand a single macro call by substituting args into the macro body.
/// Returns the expanded expression (wraps body in a Block expression if
/// the body has more than one statement, otherwise unwraps a single
/// expression statement).
fn expand_macro_call(
    macro_def: &MacroDecl,
    args: &[Spanned<Expr>],
    macros: &HashMap<String, MacroDecl>,
) -> Option<Expr> {
    if args.len() != macro_def.params.len() {
        return None; // Arity mismatch
    }

    // Build substitution map: param_name -> arg_expr
    let mut subst: HashMap<String, &Spanned<Expr>> = HashMap::new();
    for (param, arg) in macro_def.params.iter().zip(args.iter()) {
        subst.insert(param.name.clone(), arg);
    }

    // Clone body and substitute
    let mut body = macro_def.body.clone();
    substitute_stmts(&mut body, &subst);

    // Recursively expand any nested macro calls in the result
    expand_stmts(&mut body, macros);

    // If body is a single expression statement, unwrap it
    if body.len() == 1 {
        if let Stmt::Expr(expr) = &body[0].node {
            return Some(expr.node.clone());
        }
    }

    // Otherwise wrap in a block expression
    Some(Expr::Block(body))
}

fn substitute_stmts(stmts: &mut Vec<Spanned<Stmt>>, subst: &HashMap<String, &Spanned<Expr>>) {
    for stmt in stmts.iter_mut() {
        substitute_stmt(&mut stmt.node, subst);
    }
}

fn substitute_stmt(stmt: &mut Stmt, subst: &HashMap<String, &Spanned<Expr>>) {
    match stmt {
        Stmt::Let { value, .. } => {
            substitute_expr(&mut value.node, subst);
        }
        Stmt::Return(Some(expr)) => {
            substitute_expr(&mut expr.node, subst);
        }
        Stmt::If {
            condition,
            then_block,
            else_block,
        } => {
            substitute_expr(&mut condition.node, subst);
            substitute_stmts(then_block, subst);
            if let Some(eb) = else_block {
                substitute_stmts(eb, subst);
            }
        }
        Stmt::Match { expr, arms } => {
            substitute_expr(&mut expr.node, subst);
            for arm in arms {
                substitute_pattern(&mut arm.pattern.node, subst);
                if let Some(ref mut guard) = arm.guard {
                    substitute_expr(&mut guard.node, subst);
                }
                substitute_stmts(&mut arm.body, subst);
            }
        }
        Stmt::For {
            iterable, body, ..
        } => {
            substitute_expr(&mut iterable.node, subst);
            substitute_stmts(body, subst);
        }
        Stmt::While { condition, body } => {
            substitute_expr(&mut condition.node, subst);
            substitute_stmts(body, subst);
        }
        Stmt::Expr(expr) => {
            substitute_expr(&mut expr.node, subst);
        }
        _ => {}
    }
}

fn substitute_expr(expr: &mut Expr, subst: &HashMap<String, &Spanned<Expr>>) {
    match expr {
        // If this is an ident that matches a macro param, replace it
        Expr::Ident(name) => {
            if let Some(replacement) = subst.get(name.as_str()) {
                *expr = replacement.node.clone();
            }
        }
        Expr::MethodCall {
            receiver, args, ..
        } => {
            substitute_expr(&mut receiver.node, subst);
            for arg in args {
                substitute_expr(&mut arg.node, subst);
            }
        }
        Expr::FieldAccess { receiver, .. } => {
            substitute_expr(&mut receiver.node, subst);
        }
        Expr::FnCall { func, args } => {
            substitute_expr(&mut func.node, subst);
            for arg in args {
                substitute_expr(&mut arg.node, subst);
            }
        }
        Expr::Block(stmts) => {
            substitute_stmts(stmts, subst);
        }
        Expr::Unary { operand, .. } => {
            substitute_expr(&mut operand.node, subst);
        }
        Expr::Binary { left, right, .. } => {
            substitute_expr(&mut left.node, subst);
            substitute_expr(&mut right.node, subst);
        }
        Expr::StructLiteral { fields, .. } => {
            for (_, fexpr) in fields {
                substitute_expr(&mut fexpr.node, subst);
            }
        }
        Expr::Await { expr: inner } => {
            substitute_expr(&mut inner.node, subst);
        }
        Expr::ConcurrentAwait { exprs } => {
            for e in exprs {
                substitute_expr(&mut e.node, subst);
            }
        }
        Expr::ListLiteral { elements } => {
            for e in elements {
                substitute_expr(&mut e.node, subst);
            }
        }
        Expr::MapLiteral { entries } => {
            for (k, v) in entries {
                substitute_expr(&mut k.node, subst);
                substitute_expr(&mut v.node, subst);
            }
        }
        Expr::Closure { body, .. } => {
            substitute_stmts(body, subst);
        }
        Expr::MacroCall { args, .. } => {
            for arg in args {
                substitute_expr(&mut arg.node, subst);
            }
        }
        Expr::DynamicImport { path } => {
            substitute_expr(&mut path.node, subst);
        }
        Expr::Resolve { name, .. } => {
            substitute_expr(&mut name.node, subst);
        }
        Expr::Index { receiver, index } => {
            substitute_expr(&mut receiver.node, subst);
            substitute_expr(&mut index.node, subst);
        }
        Expr::Match { expr, arms } => {
            substitute_expr(&mut expr.node, subst);
            for arm in arms {
                substitute_pattern(&mut arm.pattern.node, subst);
                if let Some(ref mut guard) = arm.guard {
                    substitute_expr(&mut guard.node, subst);
                }
                substitute_stmts(&mut arm.body, subst);
            }
        }
        Expr::Try { expr } => {
            substitute_expr(&mut expr.node, subst);
        }
        _ => {} // Literal, None — nothing to substitute
    }
}

/// Walk patterns recursively during macro expansion.
/// Currently patterns cannot contain macro calls, but this ensures
/// sub-patterns are visited for completeness and future extensibility.
fn expand_pattern(pattern: &mut Pattern, macros: &HashMap<String, MacroDecl>) {
    match pattern {
        Pattern::Struct { fields, .. } => {
            for (_, opt_pat) in fields {
                if let Some(pat) = opt_pat {
                    expand_pattern(&mut pat.node, macros);
                }
            }
        }
        Pattern::List { elements, rest } => {
            for elem in elements {
                expand_pattern(&mut elem.node, macros);
            }
            if let Some(rest_pat) = rest {
                expand_pattern(&mut rest_pat.node, macros);
            }
        }
        Pattern::Some(inner) | Pattern::Ok(inner) | Pattern::Err(inner) => {
            expand_pattern(&mut inner.node, macros);
        }
        Pattern::Ident(_) | Pattern::Literal(_) | Pattern::Wildcard | Pattern::None => {}
    }
}

/// Walk patterns recursively during macro substitution.
fn substitute_pattern(pattern: &mut Pattern, subst: &HashMap<String, &Spanned<Expr>>) {
    match pattern {
        Pattern::Struct { fields, .. } => {
            for (_, opt_pat) in fields {
                if let Some(pat) = opt_pat {
                    substitute_pattern(&mut pat.node, subst);
                }
            }
        }
        Pattern::List { elements, rest } => {
            for elem in elements {
                substitute_pattern(&mut elem.node, subst);
            }
            if let Some(rest_pat) = rest {
                substitute_pattern(&mut rest_pat.node, subst);
            }
        }
        Pattern::Some(inner) | Pattern::Ok(inner) | Pattern::Err(inner) => {
            substitute_pattern(&mut inner.node, subst);
        }
        Pattern::Ident(_) | Pattern::Literal(_) | Pattern::Wildcard | Pattern::None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;
    use crate::parser::parse;

    fn parse_and_expand(source: &str) -> Program {
        let tokens = tokenize(source).unwrap();
        let (mut program, errors) = parse(source, &tokens);
        assert!(errors.is_empty(), "parse errors: {:?}", errors);
        expand_program(&mut program);
        program
    }

    #[test]
    fn test_macro_removed_from_items() {
        let program = parse_and_expand(
            r#"
            macro double(x: Expr) { x + x }
            "hello"
        "#,
        );
        assert!(program
            .items
            .iter()
            .all(|i| !matches!(&i.node, Item::Macro(_))));
    }

    #[test]
    fn test_no_macros_noop() {
        let source = r#""hello""#;
        let tokens = tokenize(source).unwrap();
        let (mut program, _) = parse(source, &tokens);
        let items_before = program.items.len();
        expand_program(&mut program);
        assert_eq!(program.items.len(), items_before);
    }

    #[test]
    fn test_macro_expansion_in_fn() {
        let program = parse_and_expand(
            r#"
            macro double(x: Expr) { x + x }
            fn test() -> Int {
                double!(3)
            }
        "#,
        );
        // The fn should exist and the macro call should be expanded to 3 + 3
        let fn_decl = program
            .items
            .iter()
            .find_map(|i| {
                if let Item::FnDecl(f) = &i.node {
                    Some(f)
                } else {
                    None
                }
            })
            .expect("fn not found");

        // Body should have an expression statement with a binary expression
        assert_eq!(fn_decl.body.len(), 1);
        if let Stmt::Expr(expr) = &fn_decl.body[0].node {
            match &expr.node {
                Expr::Binary { op, .. } => assert_eq!(*op, BinaryOp::Add),
                _ => panic!("expected Binary expr, got {:?}", expr.node),
            }
        } else {
            panic!("expected Expr stmt");
        }
    }

    #[test]
    fn test_macro_expansion_in_entry() {
        let program = parse_and_expand(
            r#"
            macro greet(name: Expr) { "Hello, " + name }
            entry() {
                greet!("World")
            }
        "#,
        );
        let entry = program
            .items
            .iter()
            .find_map(|i| {
                if let Item::Entry(e) = &i.node {
                    Some(e)
                } else {
                    None
                }
            })
            .expect("entry not found");

        assert_eq!(entry.body.len(), 1);
        if let Stmt::Expr(expr) = &entry.body[0].node {
            assert!(matches!(
                &expr.node,
                Expr::Binary {
                    op: BinaryOp::Add,
                    ..
                }
            ));
        } else {
            panic!("expected Expr stmt");
        }
    }

    #[test]
    fn test_nested_macro_expansion() {
        let program = parse_and_expand(
            r#"
            macro inc(x: Expr) { x + 1 }
            macro double_inc(x: Expr) { inc!(x) + inc!(x) }
            fn test() -> Int {
                double_inc!(5)
            }
        "#,
        );
        let fn_decl = program
            .items
            .iter()
            .find_map(|i| {
                if let Item::FnDecl(f) = &i.node {
                    Some(f)
                } else {
                    None
                }
            })
            .expect("fn not found");

        // Should have expanded to (5 + 1) + (5 + 1)
        assert_eq!(fn_decl.body.len(), 1);
        if let Stmt::Expr(expr) = &fn_decl.body[0].node {
            assert!(matches!(
                &expr.node,
                Expr::Binary {
                    op: BinaryOp::Add,
                    ..
                }
            ));
        }
    }

    #[test]
    fn test_arity_mismatch_leaves_macro_call() {
        // When arity doesn't match, the macro call should remain unexpanded
        let source = r#"
            macro double(x: Expr) { x + x }
            fn test() -> Int {
                double!(1, 2)
            }
        "#;
        let tokens = tokenize(source).unwrap();
        let (mut program, errors) = parse(source, &tokens);
        assert!(errors.is_empty(), "parse errors: {:?}", errors);
        expand_program(&mut program);

        let fn_decl = program
            .items
            .iter()
            .find_map(|i| {
                if let Item::FnDecl(f) = &i.node {
                    Some(f)
                } else {
                    None
                }
            })
            .expect("fn not found");

        // The macro call should remain because arity is wrong
        assert_eq!(fn_decl.body.len(), 1);
        if let Stmt::Expr(expr) = &fn_decl.body[0].node {
            assert!(
                matches!(&expr.node, Expr::MacroCall { name, .. } if name == "double"),
                "expected unexpanded MacroCall, got {:?}",
                expr.node,
            );
        } else {
            panic!("expected Expr stmt");
        }
    }

    #[test]
    fn test_undefined_macro_leaves_macro_call() {
        // When macro is not defined, the call should remain
        let source = r#"
            fn test() -> Int {
                unknown!(42)
            }
        "#;
        let tokens = tokenize(source).unwrap();
        let (mut program, errors) = parse(source, &tokens);
        assert!(errors.is_empty(), "parse errors: {:?}", errors);
        expand_program(&mut program);

        let fn_decl = program
            .items
            .iter()
            .find_map(|i| {
                if let Item::FnDecl(f) = &i.node {
                    Some(f)
                } else {
                    None
                }
            })
            .expect("fn not found");

        assert_eq!(fn_decl.body.len(), 1);
        if let Stmt::Expr(expr) = &fn_decl.body[0].node {
            assert!(
                matches!(&expr.node, Expr::MacroCall { name, .. } if name == "unknown"),
                "expected unexpanded MacroCall, got {:?}",
                expr.node,
            );
        } else {
            panic!("expected Expr stmt");
        }
    }

    #[test]
    fn test_macro_expansion_in_let_value() {
        let program = parse_and_expand(
            r#"
            macro double(x: Expr) { x + x }
            fn test() -> Int {
                let y: Int = double!(7)
                y
            }
        "#,
        );
        let fn_decl = program
            .items
            .iter()
            .find_map(|i| {
                if let Item::FnDecl(f) = &i.node {
                    Some(f)
                } else {
                    None
                }
            })
            .expect("fn not found");

        // First statement should be a Let with expanded value
        if let Stmt::Let { value, .. } = &fn_decl.body[0].node {
            assert!(
                matches!(
                    &value.node,
                    Expr::Binary {
                        op: BinaryOp::Add,
                        ..
                    }
                ),
                "expected Binary expr in let value, got {:?}",
                value.node,
            );
        } else {
            panic!("expected Let stmt, got {:?}", fn_decl.body[0].node);
        }
    }

    #[test]
    fn test_multiple_macro_definitions() {
        let program = parse_and_expand(
            r#"
            macro add1(x: Expr) { x + 1 }
            macro sub1(x: Expr) { x - 1 }
            fn test() -> Int {
                add1!(10) + sub1!(10)
            }
        "#,
        );
        // Both macros should be removed from items
        assert!(program
            .items
            .iter()
            .all(|i| !matches!(&i.node, Item::Macro(_))));

        let fn_decl = program
            .items
            .iter()
            .find_map(|i| {
                if let Item::FnDecl(f) = &i.node {
                    Some(f)
                } else {
                    None
                }
            })
            .expect("fn not found");

        // Body should have a single expression with expanded macros
        assert_eq!(fn_decl.body.len(), 1);
        if let Stmt::Expr(expr) = &fn_decl.body[0].node {
            // Top-level should be Add: (10 + 1) + (10 - 1)
            match &expr.node {
                Expr::Binary {
                    op: BinaryOp::Add,
                    left,
                    right,
                } => {
                    assert!(
                        matches!(
                            &left.node,
                            Expr::Binary {
                                op: BinaryOp::Add,
                                ..
                            }
                        ),
                        "expected left to be Add, got {:?}",
                        left.node,
                    );
                    assert!(
                        matches!(
                            &right.node,
                            Expr::Binary {
                                op: BinaryOp::Sub,
                                ..
                            }
                        ),
                        "expected right to be Sub, got {:?}",
                        right.node,
                    );
                }
                _ => panic!("expected Binary Add at top level, got {:?}", expr.node),
            }
        } else {
            panic!("expected Expr stmt");
        }
    }

    #[test]
    fn test_macro_expansion_preserves_other_items() {
        let program = parse_and_expand(
            r#"
            macro noop(x: Expr) { x }
            fn first() -> Int { noop!(1) }
            fn second() -> Int { 2 }
        "#,
        );
        // Macro removed, both fns remain
        assert_eq!(
            program
                .items
                .iter()
                .filter(|i| matches!(&i.node, Item::FnDecl(_)))
                .count(),
            2
        );
        assert!(program
            .items
            .iter()
            .all(|i| !matches!(&i.node, Item::Macro(_))));
    }

    #[test]
    fn test_macro_expansion_in_match_guard() {
        // Macro used in match guard expression should be expanded
        let program = parse_and_expand(
            r#"
            macro is_big(x: Expr) { x > 10 }
            fn test(v: Int) -> String {
                match v {
                    x if is_big!(x) => "big"
                    _ => "small"
                }
            }
        "#,
        );
        let fn_decl = program
            .items
            .iter()
            .find_map(|i| {
                if let Item::FnDecl(f) = &i.node {
                    Some(f)
                } else {
                    None
                }
            })
            .expect("fn not found");

        // Should have a match statement in body
        assert_eq!(fn_decl.body.len(), 1);
        if let Stmt::Match { arms, .. } = &fn_decl.body[0].node {
            assert_eq!(arms.len(), 2);
            // First arm should have a guard that's a Binary expression (expanded from macro)
            let guard = arms[0].guard.as_ref().expect("expected guard");
            assert!(
                matches!(&guard.node, Expr::Binary { op: BinaryOp::Gt, .. }),
                "expected guard to be expanded Binary(Gt), got {:?}",
                guard.node
            );
        } else {
            panic!("expected Match stmt");
        }
    }

    #[test]
    fn test_macro_expansion_in_match_body() {
        // Macro used in match arm body should be expanded
        let program = parse_and_expand(
            r#"
            macro double(x: Expr) { x + x }
            fn test(v: Int) -> Int {
                match v {
                    1 => double!(v)
                    _ => 0
                }
            }
        "#,
        );
        let fn_decl = program
            .items
            .iter()
            .find_map(|i| {
                if let Item::FnDecl(f) = &i.node {
                    Some(f)
                } else {
                    None
                }
            })
            .expect("fn not found");

        if let Stmt::Match { arms, .. } = &fn_decl.body[0].node {
            // First arm's body should contain an expanded Binary(Add) expression
            assert_eq!(arms[0].body.len(), 1);
            if let Stmt::Expr(expr) = &arms[0].body[0].node {
                assert!(
                    matches!(&expr.node, Expr::Binary { op: BinaryOp::Add, .. }),
                    "expected expanded Binary(Add), got {:?}",
                    expr.node
                );
            } else {
                panic!("expected Expr stmt in arm body");
            }
        } else {
            panic!("expected Match stmt");
        }
    }
}
