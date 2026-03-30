pub mod span;

use logos::Logos;
pub use span::{Span, Spanned};

#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(skip r"[ \t\r\n]+")]
#[logos(skip r"//[^\n]*")]
#[logos(skip r"/\*([^*]|\*[^/])*\*/")]
pub enum Token {
    // Keywords
    #[token("port")]
    Port,
    #[token("service")]
    Service,
    #[token("provides")]
    Provides,
    #[token("publish")]
    Publish,
    #[token("as")]
    As,
    #[token("command")]
    Command,
    #[token("query")]
    Query,
    #[token("via")]
    Via,
    #[token("settle")]
    Settle,
    #[token("observe")]
    Observe,
    #[token("by")]
    By,
    #[token("component")]
    Component,
    #[token("spawn")]
    Spawn,
    #[token("entry")]
    Entry,
    #[token("type")]
    TypeKw,
    #[token("substrate")]
    Substrate,
    #[token("guarantee")]
    Guarantee,
    #[token("resolve")]
    Resolve,
    #[token("with")]
    With,
    #[token("profile")]
    Profile,
    #[token("cap")]
    Cap,
    #[token("let")]
    Let,
    #[token("return")]
    Return,
    #[token("if")]
    If,
    #[token("else")]
    Else,
    #[token("match")]
    Match,
    #[token("fn")]
    Fn,
    #[token("on")]
    On,
    #[token("op")]
    Op,
    #[token("emits")]
    Emits,
    #[token("law")]
    Law,
    #[token("module")]
    Module,
    #[token("import")]
    Import,
    #[token("export")]
    Export,
    #[token("true")]
    True,
    #[token("false")]
    False,
    #[token("none")]
    None,
    #[token("await")]
    Await,
    #[token("concurrent")]
    Concurrent,
    #[token("consume")]
    Consume,
    #[token("borrow")]
    Borrow,
    #[token("delegate")]
    Delegate,
    #[token("for")]
    For,
    #[token("while")]
    While,
    #[token("in")]
    In,
    #[token("break")]
    Break,
    #[token("continue")]
    Continue,
    #[token("pure")]
    Pure,
    #[token("macro")]
    Macro,
    #[token("where")]
    Where,
    #[token("mut")]
    Mut,
    #[token("emit")]
    Emit,
    #[token("state")]
    State,
    #[token("select")]
    Select,

    // Punctuation
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token(",")]
    Comma,
    #[token(":")]
    Colon,
    #[token(";")]
    Semicolon,
    #[token(".")]
    Dot,
    #[token("?")]
    Question,
    #[token("=")]
    Eq,
    #[token("@")]
    At,

    #[token("..")]
    DotDot,

    // Multi-char operators
    #[token("->")]
    Arrow,
    #[token("=>")]
    FatArrow,
    #[token("==")]
    EqEq,
    #[token("!=")]
    BangEq,
    #[token("<=")]
    LtEq,
    #[token(">=")]
    GtEq,
    #[token("&&")]
    AmpAmp,
    #[token("||")]
    PipePipe,
    #[token("<<")]
    LtLt,
    #[token(">>")]
    GtGt,

    // Single-char operators
    #[token("<")]
    Lt,
    #[token(">")]
    Gt,
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("!")]
    Bang,
    #[token("%")]
    Percent,
    #[token("|")]
    Pipe,
    #[token("&")]
    Ampersand,
    #[token("^")]
    Caret,
    #[token("~")]
    Tilde,

    // Literals
    #[regex(r#""([^"\\]|\\.)*""#)]
    String,
    #[regex(r"[0-9]+\.[0-9]+")]
    Float,
    #[regex(r"[0-9]+")]
    Int,
    #[regex(r"0x[0-9a-fA-F]+")]
    HexInt,
    #[regex(r#"b"([0-9a-fA-F][0-9a-fA-F])*""#)]
    BytesLiteral,

    // Identifier
    #[regex(r"[a-zA-Z_][a-zA-Z0-9_]*")]
    Ident,
}

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Token::Port => write!(f, "port"),
            Token::Service => write!(f, "service"),
            Token::Provides => write!(f, "provides"),
            Token::Publish => write!(f, "publish"),
            Token::As => write!(f, "as"),
            Token::Command => write!(f, "command"),
            Token::Query => write!(f, "query"),
            Token::Via => write!(f, "via"),
            Token::Settle => write!(f, "settle"),
            Token::Observe => write!(f, "observe"),
            Token::By => write!(f, "by"),
            Token::Component => write!(f, "component"),
            Token::Spawn => write!(f, "spawn"),
            Token::Entry => write!(f, "entry"),
            Token::TypeKw => write!(f, "type"),
            Token::Substrate => write!(f, "substrate"),
            Token::Guarantee => write!(f, "guarantee"),
            Token::Resolve => write!(f, "resolve"),
            Token::With => write!(f, "with"),
            Token::Profile => write!(f, "profile"),
            Token::Cap => write!(f, "cap"),
            Token::Let => write!(f, "let"),
            Token::Return => write!(f, "return"),
            Token::If => write!(f, "if"),
            Token::Else => write!(f, "else"),
            Token::Match => write!(f, "match"),
            Token::Fn => write!(f, "fn"),
            Token::On => write!(f, "on"),
            Token::Op => write!(f, "op"),
            Token::Emits => write!(f, "emits"),
            Token::Law => write!(f, "law"),
            Token::Module => write!(f, "module"),
            Token::Import => write!(f, "import"),
            Token::Export => write!(f, "export"),
            Token::True => write!(f, "true"),
            Token::False => write!(f, "false"),
            Token::None => write!(f, "none"),
            Token::Await => write!(f, "await"),
            Token::Concurrent => write!(f, "concurrent"),
            Token::Consume => write!(f, "consume"),
            Token::Borrow => write!(f, "borrow"),
            Token::Delegate => write!(f, "delegate"),
            Token::For => write!(f, "for"),
            Token::While => write!(f, "while"),
            Token::In => write!(f, "in"),
            Token::Break => write!(f, "break"),
            Token::Continue => write!(f, "continue"),
            Token::Pure => write!(f, "pure"),
            Token::Macro => write!(f, "macro"),
            Token::Where => write!(f, "where"),
            Token::Mut => write!(f, "mut"),
            Token::Emit => write!(f, "emit"),
            Token::State => write!(f, "state"),
            Token::Select => write!(f, "select"),
            Token::DotDot => write!(f, ".."),
            Token::LParen => write!(f, "("),
            Token::RParen => write!(f, ")"),
            Token::LBrace => write!(f, "{{"),
            Token::RBrace => write!(f, "}}"),
            Token::LBracket => write!(f, "["),
            Token::RBracket => write!(f, "]"),
            Token::Comma => write!(f, ","),
            Token::Colon => write!(f, ":"),
            Token::Semicolon => write!(f, ";"),
            Token::Dot => write!(f, "."),
            Token::Question => write!(f, "?"),
            Token::Eq => write!(f, "="),
            Token::At => write!(f, "@"),
            Token::Arrow => write!(f, "->"),
            Token::FatArrow => write!(f, "=>"),
            Token::EqEq => write!(f, "=="),
            Token::BangEq => write!(f, "!="),
            Token::LtEq => write!(f, "<="),
            Token::GtEq => write!(f, ">="),
            Token::AmpAmp => write!(f, "&&"),
            Token::PipePipe => write!(f, "||"),
            Token::LtLt => write!(f, "<<"),
            Token::GtGt => write!(f, ">>"),
            Token::Lt => write!(f, "<"),
            Token::Gt => write!(f, ">"),
            Token::Plus => write!(f, "+"),
            Token::Minus => write!(f, "-"),
            Token::Star => write!(f, "*"),
            Token::Slash => write!(f, "/"),
            Token::Bang => write!(f, "!"),
            Token::Percent => write!(f, "%"),
            Token::Pipe => write!(f, "|"),
            Token::Ampersand => write!(f, "&"),
            Token::Caret => write!(f, "^"),
            Token::Tilde => write!(f, "~"),
            Token::String => write!(f, "string"),
            Token::Float => write!(f, "float"),
            Token::Int => write!(f, "int"),
            Token::HexInt => write!(f, "hex int"),
            Token::BytesLiteral => write!(f, "bytes literal"),
            Token::Ident => write!(f, "identifier"),
        }
    }
}

pub fn tokenize(source: &str) -> Result<Vec<Spanned<Token>>, Vec<Span>> {
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    let mut lexer = Token::lexer(source);

    while let Some(result) = lexer.next() {
        let span = lexer.span();
        let span = Span::new(span.start, span.end);
        match result {
            Ok(token) => tokens.push(Spanned::new(token, span)),
            Err(()) => errors.push(span),
        }
    }

    if errors.is_empty() {
        Ok(tokens)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_hello() {
        let tokens = tokenize(r#""Hello, World!""#).unwrap();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].node, Token::String);
    }

    #[test]
    fn test_tokenize_entry() {
        let tokens = tokenize("entry(out: cap Stdout) { }").unwrap();
        let kinds: Vec<_> = tokens.iter().map(|t| &t.node).collect();
        assert_eq!(
            kinds,
            vec![
                &Token::Entry,
                &Token::LParen,
                &Token::Ident,
                &Token::Colon,
                &Token::Cap,
                &Token::Ident,
                &Token::RParen,
                &Token::LBrace,
                &Token::RBrace,
            ]
        );
    }

    #[test]
    fn test_tokenize_operators() {
        let tokens = tokenize("-> => == != <= >= && ||").unwrap();
        let kinds: Vec<_> = tokens.iter().map(|t| &t.node).collect();
        assert_eq!(
            kinds,
            vec![
                &Token::Arrow,
                &Token::FatArrow,
                &Token::EqEq,
                &Token::BangEq,
                &Token::LtEq,
                &Token::GtEq,
                &Token::AmpAmp,
                &Token::PipePipe,
            ]
        );
    }

    #[test]
    fn test_tokenize_literals() {
        let tokens = tokenize("42 3.14 true false none").unwrap();
        let kinds: Vec<_> = tokens.iter().map(|t| &t.node).collect();
        assert_eq!(
            kinds,
            vec![
                &Token::Int,
                &Token::Float,
                &Token::True,
                &Token::False,
                &Token::None,
            ]
        );
    }

    #[test]
    fn test_line_comment() {
        let tokens = tokenize("port // this is a comment\nKV").unwrap();
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].node, Token::Port);
        assert_eq!(tokens[1].node, Token::Ident);
    }

    #[test]
    fn test_block_comment() {
        let tokens = tokenize("port /* block comment */ KV").unwrap();
        assert_eq!(tokens.len(), 2);
        assert_eq!(tokens[0].node, Token::Port);
        assert_eq!(tokens[1].node, Token::Ident);
    }
}
