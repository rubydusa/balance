use balance_lang::ast::{Item, Program};
use balance_lang::lexer::span::{Span, Spanned};
use tower_lsp::lsp_types::{DocumentSymbol, SymbolKind};

use crate::diagnostics::span_to_range;

#[allow(deprecated)]
pub fn document_symbols(source: &str, program: &Program) -> Vec<DocumentSymbol> {
    let mut out = Vec::new();
    for item in &program.items {
        if let Some(sym) = item_symbol(source, item) {
            out.push(sym);
        }
    }
    out
}

#[allow(deprecated)]
fn item_symbol(source: &str, item: &Spanned<Item>) -> Option<DocumentSymbol> {
    let item_span = item.span;
    let (name, name_span, kind, children) = match &item.node {
        Item::Port(p) => {
            let children = p
                .methods
                .iter()
                .map(|m| DocumentSymbol {
                    name: m.node.name.clone(),
                    detail: None,
                    kind: SymbolKind::METHOD,
                    tags: None,
                    deprecated: None,
                    range: span_to_range(source, m.span),
                    selection_range: span_to_range(source, m.span),
                    children: None,
                })
                .collect::<Vec<_>>();
            (
                p.name.clone(),
                p.name_span,
                SymbolKind::INTERFACE,
                if children.is_empty() { None } else { Some(children) },
            )
        }
        Item::Service(s) => {
            let children = s
                .items
                .iter()
                .filter_map(|si| service_child(source, si))
                .collect::<Vec<_>>();
            (
                s.name.clone(),
                s.name_span,
                SymbolKind::CLASS,
                if children.is_empty() { None } else { Some(children) },
            )
        }
        Item::Entry(e) => {
            let name = e.name.clone().unwrap_or_else(|| "entry".to_string());
            (name, e.name_span, SymbolKind::FUNCTION, None)
        }
        Item::TypeDecl(t) => (t.name.clone(), t.name_span, SymbolKind::STRUCT, None),
        Item::FnDecl(f) => (f.name.clone(), f.name_span, SymbolKind::FUNCTION, None),
        Item::Substrate(s) => (s.name.clone(), s.name_span, SymbolKind::CLASS, None),
        Item::Guarantee(g) => (g.name.clone(), g.name_span, SymbolKind::NAMESPACE, None),
        Item::Profile(p) => (p.name.clone(), p.name_span, SymbolKind::CONSTANT, None),
        Item::Macro(m) => (m.name.clone(), m.name_span, SymbolKind::FUNCTION, None),
        Item::Stmt(_) => return None,
    };

    Some(DocumentSymbol {
        name,
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range: span_to_range(source, item_span),
        selection_range: span_to_range(source, safe_span(name_span, item_span)),
        children,
    })
}

#[allow(deprecated)]
fn service_child(source: &str, si: &Spanned<balance_lang::ast::ServiceItem>) -> Option<DocumentSymbol> {
    use balance_lang::ast::ServiceItem;
    let (name, kind) = match &si.node {
        ServiceItem::Command(c) => (c.name.clone(), SymbolKind::METHOD),
        ServiceItem::Query(q) => (q.name.clone(), SymbolKind::METHOD),
        ServiceItem::Component(c) => (c.name.clone(), SymbolKind::FIELD),
        _ => return None,
    };
    Some(DocumentSymbol {
        name,
        detail: None,
        kind,
        tags: None,
        deprecated: None,
        range: span_to_range(source, si.span),
        selection_range: span_to_range(source, si.span),
        children: None,
    })
}

fn safe_span(inner: Span, outer: Span) -> Span {
    if inner.start == 0 && inner.end == 0 {
        outer
    } else {
        inner
    }
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
    fn function_becomes_function_symbol() {
        let (src, program) = parse("fn greet() -> Int { return 1 }");
        let syms = document_symbols(&src, &program);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "greet");
        assert_eq!(syms[0].kind, SymbolKind::FUNCTION);
    }

    #[test]
    fn port_becomes_interface_with_method_children() {
        let src = r#"
port Hello {
    greet() -> String [query]
    wave() -> String [query]
}
"#;
        let (src, program) = parse(src);
        let syms = document_symbols(&src, &program);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Hello");
        assert_eq!(syms[0].kind, SymbolKind::INTERFACE);
        let children = syms[0].children.as_ref().expect("port should have method children");
        let names: Vec<_> = children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["greet", "wave"]);
    }

    #[test]
    fn service_becomes_class_with_method_children() {
        let src = r#"
port Hello { greet() -> String [query, visible(committed)] }
service HelloWorld provides Hello {
    publish as "hello/world"
    query greet() -> String { return "hi" }
}
"#;
        let (src, program) = parse(src);
        let syms = document_symbols(&src, &program);
        let svc = syms.iter().find(|s| s.name == "HelloWorld").expect("service symbol");
        assert_eq!(svc.kind, SymbolKind::CLASS);
        let children = svc.children.as_ref().expect("service should have children");
        assert!(children.iter().any(|c| c.name == "greet" && c.kind == SymbolKind::METHOD));
    }

    #[test]
    fn type_decl_becomes_struct_symbol() {
        let src = "type Point {\n    x: Int\n    y: Int\n}";
        let (src, program) = parse(src);
        let syms = document_symbols(&src, &program);
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "Point");
        assert_eq!(syms[0].kind, SymbolKind::STRUCT);
    }

    #[test]
    fn selection_range_is_name_span_not_whole_item() {
        let src = "fn greet() -> Int { return 1 }";
        let (src, program) = parse(src);
        let syms = document_symbols(&src, &program);
        let sym = &syms[0];
        // range covers the whole decl; selection_range should cover just "greet"
        assert_eq!(sym.range.start.character, 0);
        assert_eq!(sym.selection_range.start.character, 3); // after "fn "
        assert_eq!(sym.selection_range.end.character, 8);   // greet ends at 8
    }
}
