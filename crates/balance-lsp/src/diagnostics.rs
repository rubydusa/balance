use balance_lang::lexer::span::Span;
use balance_lang::parser::error::offset_to_line_col;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, Position, Range};

use crate::analysis::DocumentState;

pub fn to_diagnostics(doc: &DocumentState) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let src = &doc.source;

    for span in &doc.lex_error_spans {
        out.push(mk(src, *span, "unrecognized token", DiagnosticSeverity::ERROR));
    }
    for pe in &doc.parse_errors {
        out.push(mk(src, pe.span, &pe.message, DiagnosticSeverity::ERROR));
    }
    for te in &doc.type_result.errors {
        let span = te.span.unwrap_or(Span::new(0, 0));
        out.push(mk(src, span, &te.message, DiagnosticSeverity::ERROR));
    }
    for tw in &doc.type_result.warnings {
        let span = tw.span.unwrap_or(Span::new(0, 0));
        out.push(mk(src, span, &tw.message, DiagnosticSeverity::WARNING));
    }

    out
}

fn mk(source: &str, span: Span, message: &str, severity: DiagnosticSeverity) -> Diagnostic {
    Diagnostic {
        range: span_to_range(source, span),
        severity: Some(severity),
        source: Some("balance".to_string()),
        message: message.to_string(),
        ..Default::default()
    }
}

pub fn span_to_range(source: &str, span: Span) -> Range {
    Range {
        start: offset_to_position(source, span.start),
        end: offset_to_position(source, span.end),
    }
}

pub fn offset_to_position(source: &str, offset: usize) -> Position {
    let (line, col) = offset_to_line_col(source, offset);
    Position {
        line: (line as u32).saturating_sub(1),
        character: (col as u32).saturating_sub(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::analyze;

    #[test]
    fn offset_zero_is_position_zero_zero() {
        let src = "abc";
        let p = offset_to_position(src, 0);
        assert_eq!(p.line, 0);
        assert_eq!(p.character, 0);
    }

    #[test]
    fn offset_across_newline_advances_line() {
        let src = "abc\ndef";
        // Offset 4 is 'd' on line 1, col 0
        let p = offset_to_position(src, 4);
        assert_eq!(p.line, 1);
        assert_eq!(p.character, 0);
    }

    #[test]
    fn span_to_range_spans_both_endpoints() {
        let src = "abcdef";
        let r = span_to_range(src, Span::new(1, 4));
        assert_eq!(r.start.line, 0);
        assert_eq!(r.start.character, 1);
        assert_eq!(r.end.line, 0);
        assert_eq!(r.end.character, 4);
    }

    #[test]
    fn type_error_becomes_error_diagnostic_with_balance_source() {
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
        let diags = to_diagnostics(&doc);
        assert!(!diags.is_empty(), "expected at least one diagnostic");
        let first = &diags[0];
        assert_eq!(first.source.as_deref(), Some("balance"));
        assert_eq!(first.severity, Some(DiagnosticSeverity::ERROR));
    }

    #[test]
    fn valid_source_produces_no_diagnostics() {
        let src = "fn f() -> Int { return 1 }";
        let doc = analyze(src.to_string(), None, None);
        let diags = to_diagnostics(&doc);
        assert!(
            diags.is_empty(),
            "expected no diagnostics, got: {:?}",
            diags.iter().map(|d| &d.message).collect::<Vec<_>>()
        );
    }
}
