use crate::lexer::span::Span;
use crate::lexer::Token;

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub span: Span,
}

impl ParseError {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }

    pub fn expected(expected: &str, found: Option<&Token>, span: Span) -> Self {
        let found_str = match found {
            Some(tok) => format!("{tok}"),
            None => "end of file".to_string(),
        };
        Self::new(format!("expected {expected}, found {found_str}"), span)
    }

    pub fn display(&self, source: &str) -> String {
        let (line, col) = offset_to_line_col(source, self.span.start);
        format!("error at {}:{}: {}", line, col, self.message)
    }
}

pub fn offset_to_line_col(source: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, ch) in source.char_indices() {
        if i >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}
