use std::path::Path;

use balance_lang::ast::Program;
use balance_lang::lexer::span::Span;
use balance_lang::module::ModuleLoader;
use balance_lang::parser::error::ParseError;
use balance_lang::types::check::TypeCheckResult;

pub struct DocumentState {
    pub source: String,
    pub program: Program,
    pub parse_errors: Vec<ParseError>,
    pub lex_error_spans: Vec<Span>,
    pub type_result: TypeCheckResult,
}

pub fn analyze(source: String, path: Option<&Path>, loader: Option<&mut ModuleLoader>) -> DocumentState {
    let (tokens, lex_error_spans) = match balance_lang::lexer::tokenize(&source) {
        Ok(t) => (t, Vec::new()),
        Err(spans) => (Vec::new(), spans),
    };

    let (mut program, parse_errors) = if tokens.is_empty() && !lex_error_spans.is_empty() {
        (
            Program {
                module_decl: None,
                imports: Vec::new(),
                items: Vec::new(),
            },
            Vec::new(),
        )
    } else {
        balance_lang::parser::parse(&source, &tokens)
    };

    balance_lang::macro_expand::expand_program(&mut program);

    if let (Some(path), Some(loader)) = (path, loader) {
        merge_imports(&mut program, path, loader);
    }

    let type_result = if let Some(path) = path {
        let root = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        balance_lang::types::check::check_program_with_root(&program, root)
    } else {
        balance_lang::types::check::check_program(&program)
    };

    DocumentState {
        source,
        program,
        parse_errors,
        lex_error_spans,
        type_result,
    }
}

fn merge_imports(program: &mut Program, file: &Path, loader: &mut ModuleLoader) {
    if program.imports.is_empty() {
        return;
    }
    let file_path = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    let mod_name = match loader.load_file(&file_path) {
        Ok(n) => n,
        Err(_) => return,
    };
    if let Ok(imported_items) = loader.resolve_imports(&mod_name) {
        let existing = std::mem::take(&mut program.items);
        for item in imported_items {
            program.items.push(balance_lang::lexer::span::Spanned {
                node: item,
                span: Span::new(0, 0),
            });
        }
        program.items.extend(existing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_program_has_no_errors() {
        let src = r#"
fn greet(name: String) -> String {
    return "hello"
}
"#;
        let doc = analyze(src.to_string(), None, None);
        assert!(doc.lex_error_spans.is_empty(), "unexpected lex errors");
        assert!(doc.parse_errors.is_empty(), "unexpected parse errors: {:?}", doc.parse_errors);
        assert!(
            doc.type_result.errors.is_empty(),
            "unexpected type errors: {:?}",
            doc.type_result.errors.iter().map(|e| &e.message).collect::<Vec<_>>()
        );
        assert_eq!(doc.program.items.len(), 1);
    }

    #[test]
    fn lex_error_is_collected() {
        // `@` outside of an authority qualifier position with no following keyword: produces an Ident-like context with a stray char.
        // Actually `@` alone is a valid token; use a character the lexer rejects — backtick.
        let src = "fn foo() { `bad` }";
        let doc = analyze(src.to_string(), None, None);
        assert!(
            !doc.lex_error_spans.is_empty(),
            "expected lex errors for stray backtick, got none"
        );
    }

    #[test]
    fn parse_error_is_collected_but_program_is_partial() {
        // Incomplete function declaration triggers parse recovery
        let src = "fn";
        let doc = analyze(src.to_string(), None, None);
        assert!(!doc.parse_errors.is_empty(), "expected parse errors");
        // Parser recovery still yields a Program struct (possibly with no items)
        assert_eq!(doc.program.imports.len(), 0);
    }

    #[test]
    fn type_error_is_collected() {
        let src = r#"
fn greet() -> String {
    return "hi"
}

entry() {
    let x: Int = greet()
    x
}
"#;
        let doc = analyze(src.to_string(), None, None);
        assert!(doc.parse_errors.is_empty(), "unexpected parse errors");
        assert!(
            !doc.type_result.errors.is_empty(),
            "expected type mismatch error between String and Int"
        );
    }

    #[test]
    fn preserves_source_for_diagnostic_formatting() {
        let src = "fn f() -> Int { return 1 }";
        let doc = analyze(src.to_string(), None, None);
        assert_eq!(doc.source, src);
    }
}
