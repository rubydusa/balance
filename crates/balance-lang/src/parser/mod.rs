pub mod error;

use crate::ast::*;
use crate::lexer::span::{Span, Spanned};
use crate::lexer::Token;
use error::ParseError;

pub struct Parser<'a> {
    tokens: &'a [Spanned<Token>],
    source: &'a str,
    pos: usize,
    pub errors: Vec<ParseError>,
}

impl<'a> Parser<'a> {
    pub fn new(tokens: &'a [Spanned<Token>], source: &'a str) -> Self {
        Self {
            tokens,
            source,
            pos: 0,
            errors: Vec::new(),
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos).map(|t| &t.node)
    }

    fn peek_span(&self) -> Span {
        self.tokens
            .get(self.pos)
            .map(|t| t.span)
            .unwrap_or(Span::new(self.source.len(), self.source.len()))
    }

    fn advance(&mut self) -> &Spanned<Token> {
        let tok = &self.tokens[self.pos];
        self.pos += 1;
        tok
    }

    fn check(&self, token: &Token) -> bool {
        self.peek() == Some(token)
    }

    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn text_of(&self, span: Span) -> &'a str {
        &self.source[span.start..span.end]
    }

    fn expect(&mut self, token: Token) -> Result<Span, ParseError> {
        if self.check(&token) {
            Ok(self.advance().span)
        } else {
            let span = self.peek_span();
            Err(ParseError::expected(
                &format!("'{token}'"),
                self.peek(),
                span,
            ))
        }
    }

    fn eat(&mut self, token: Token) -> bool {
        if self.check(&token) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn error(&mut self, err: ParseError) {
        self.errors.push(err);
    }

    /// Accept a token as an identifier even if it's a contextual keyword.
    /// Many Balance keywords (command, query, visible, etc.) also appear as identifiers
    /// in annotations, event refs, and other contexts.
    fn expect_ident_or_keyword(&mut self) -> Result<Span, ParseError> {
        match self.peek() {
            Some(
                Token::Ident
                | Token::Command
                | Token::Query
                | Token::Via
                | Token::Settle
                | Token::Observe
                | Token::By
                | Token::On
                | Token::Op
                | Token::Emits
                | Token::Law
                | Token::As
                | Token::With
                | Token::Profile
                | Token::Publish
                | Token::Component
                | Token::Spawn
                | Token::Provides
                | Token::Module
                | Token::Import
                | Token::Export
                | Token::Entry
                | Token::Await
                | Token::Concurrent
                | Token::Consume
                | Token::Borrow
                | Token::Delegate
                | Token::For
                | Token::While
                | Token::In
                | Token::Break
                | Token::Continue
                | Token::Pure
                | Token::Macro
                | Token::Where
                | Token::Mut
                | Token::Emit
                | Token::State
                | Token::Select
            ) => Ok(self.advance().span),
            _ => {
                let span = self.peek_span();
                Err(ParseError::expected("identifier", self.peek(), span))
            }
        }
    }

    pub fn parse_program(&mut self) -> Program {
        let module_decl = if self.check(&Token::Module) {
            match self.parse_module_decl() {
                Ok(m) => Some(m),
                Err(e) => {
                    self.error(e);
                    None
                }
            }
        } else {
            None
        };

        let mut imports = Vec::new();
        while self.check(&Token::Import) && self.tokens.get(self.pos + 1).map(|t| &t.node) != Some(&Token::LParen) {
            match self.parse_import_decl() {
                Ok(imp) => imports.push(imp),
                Err(e) => {
                    self.error(e);
                    self.advance();
                }
            }
        }

        let mut items = Vec::new();
        while !self.at_end() {
            match self.parse_item() {
                Ok(item) => items.push(item),
                Err(e) => {
                    self.error(e);
                    if !self.at_end() {
                        self.advance();
                    }
                }
            }
        }

        Program {
            module_decl,
            imports,
            items,
        }
    }

    fn parse_module_decl(&mut self) -> Result<ModuleDecl, ParseError> {
        self.expect(Token::Module)?;
        let mut path = Vec::new();
        let span = self.expect(Token::Ident)?;
        path.push(self.text_of(span).to_string());
        while self.eat(Token::Dot) {
            let span = self.expect(Token::Ident)?;
            path.push(self.text_of(span).to_string());
        }
        Ok(ModuleDecl { path })
    }

    fn parse_import_decl(&mut self) -> Result<Spanned<ImportDecl>, ParseError> {
        let start = self.expect(Token::Import)?;
        let mut path = Vec::new();
        let span = self.expect(Token::Ident)?;
        path.push(self.text_of(span).to_string());
        while self.eat(Token::Dot) {
            if self.check(&Token::LBrace) {
                break;
            }
            let span = self.expect(Token::Ident)?;
            path.push(self.text_of(span).to_string());
            if self.check(&Token::Dot) && self.tokens.get(self.pos + 1).map(|t| &t.node) == Some(&Token::LBrace) {
                self.advance(); // consume the dot before {
                break;
            }
        }

        let mut names = None;
        let mut alias = None;

        if self.eat(Token::LBrace) {
            let mut n = Vec::new();
            loop {
                let span = self.expect(Token::Ident)?;
                let original = self.text_of(span).to_string();
                let alias_name = if self.eat(Token::As) {
                    let alias_span = self.expect(Token::Ident)?;
                    Some(self.text_of(alias_span).to_string())
                } else {
                    None
                };
                n.push((original, alias_name));
                if !self.eat(Token::Comma) {
                    break;
                }
            }
            self.expect(Token::RBrace)?;
            names = Some(n);
        } else if self.eat(Token::As) {
            let span = self.expect(Token::Ident)?;
            alias = Some(self.text_of(span).to_string());
        }

        let end = self.tokens.get(self.pos.saturating_sub(1)).map(|t| t.span).unwrap_or(start);
        Ok(Spanned::new(
            ImportDecl { path, names, alias },
            start.merge(end),
        ))
    }

    fn parse_item(&mut self) -> Result<Spanned<Item>, ParseError> {
        let exported = self.eat(Token::Export);
        let start = self.peek_span();

        match self.peek() {
            Some(Token::Port) => {
                let decl = self.parse_port_decl(exported)?;
                Ok(Spanned::new(Item::Port(decl), start.merge(self.prev_span())))
            }
            Some(Token::Service) => {
                let decl = self.parse_service_decl(exported)?;
                Ok(Spanned::new(Item::Service(decl), start.merge(self.prev_span())))
            }
            Some(Token::Entry) => {
                let decl = self.parse_entry_decl()?;
                Ok(Spanned::new(Item::Entry(decl), start.merge(self.prev_span())))
            }
            Some(Token::TypeKw) => {
                let decl = self.parse_type_decl(exported)?;
                Ok(Spanned::new(Item::TypeDecl(decl), start.merge(self.prev_span())))
            }
            Some(Token::Pure) => {
                self.advance(); // consume 'pure'
                let decl = self.parse_fn_decl_inner(true)?;
                Ok(Spanned::new(Item::FnDecl(decl), start.merge(self.prev_span())))
            }
            Some(Token::Fn) => {
                let decl = self.parse_fn_decl()?;
                Ok(Spanned::new(Item::FnDecl(decl), start.merge(self.prev_span())))
            }
            Some(Token::Substrate) => {
                let decl = self.parse_substrate_decl()?;
                Ok(Spanned::new(Item::Substrate(decl), start.merge(self.prev_span())))
            }
            Some(Token::Guarantee) => {
                let decl = self.parse_guarantee_decl()?;
                Ok(Spanned::new(Item::Guarantee(decl), start.merge(self.prev_span())))
            }
            Some(Token::Profile) => {
                let decl = self.parse_profile_decl()?;
                Ok(Spanned::new(Item::Profile(decl), start.merge(self.prev_span())))
            }
            Some(Token::Macro) => {
                let decl = self.parse_macro_decl()?;
                Ok(Spanned::new(Item::Macro(decl), start.merge(self.prev_span())))
            }
            _ => {
                if exported {
                    return Err(ParseError::new("expected declaration after 'export'", start));
                }
                let stmt = self.parse_stmt()?;
                let span = stmt.span;
                Ok(Spanned::new(Item::Stmt(stmt.node), span))
            }
        }
    }

    fn prev_span(&self) -> Span {
        self.tokens
            .get(self.pos.saturating_sub(1))
            .map(|t| t.span)
            .unwrap_or(Span::new(0, 0))
    }

    fn parse_port_decl(&mut self, exported: bool) -> Result<PortDecl, ParseError> {
        self.expect(Token::Port)?;
        let name_span = self.expect(Token::Ident)?;
        let name = self.text_of(name_span).to_string();
        self.expect(Token::LBrace)?;

        let mut methods = Vec::new();
        while !self.check(&Token::RBrace) && !self.at_end() {
            let method = self.parse_port_method()?;
            methods.push(method);
        }
        self.expect(Token::RBrace)?;

        Ok(PortDecl {
            exported,
            name,
            methods,
        })
    }

    fn parse_port_method(&mut self) -> Result<Spanned<PortMethod>, ParseError> {
        let start = self.peek_span();
        let name_span = self.expect_ident_or_keyword()?;
        let name = self.text_of(name_span).to_string();
        self.expect(Token::LParen)?;
        let params = self.parse_param_list()?;
        self.expect(Token::RParen)?;
        self.expect(Token::Arrow)?;
        let return_type = self.parse_type_expr()?;

        let mut annotations = Vec::new();
        if self.eat(Token::LBracket) {
            loop {
                let ann = self.parse_annotation()?;
                annotations.push(ann);
                if !self.eat(Token::Comma) {
                    break;
                }
            }
            self.expect(Token::RBracket)?;
        }

        let end = self.prev_span();
        Ok(Spanned::new(
            PortMethod {
                name,
                params,
                return_type,
                annotations,
            },
            start.merge(end),
        ))
    }

    fn parse_annotation(&mut self) -> Result<Annotation, ParseError> {
        let name_span = self.expect_ident_or_keyword()?;
        let name = self.text_of(name_span).to_string();
        let arg = if self.eat(Token::LParen) {
            let arg_span = self.expect_ident_or_keyword()?;
            let arg = self.text_of(arg_span).to_string();
            self.expect(Token::RParen)?;
            Some(arg)
        } else {
            None
        };
        Ok(Annotation { name, arg })
    }

    fn parse_service_decl(&mut self, exported: bool) -> Result<ServiceDecl, ParseError> {
        self.expect(Token::Service)?;
        let name_span = self.expect(Token::Ident)?;
        let name = self.text_of(name_span).to_string();
        let provides = if self.eat(Token::Provides) {
            let p_span = self.expect(Token::Ident)?;
            self.text_of(p_span).to_string()
        } else {
            String::new()
        };
        self.expect(Token::LBrace)?;

        let mut items = Vec::new();
        while !self.check(&Token::RBrace) && !self.at_end() {
            let start = self.peek_span();
            let item = self.parse_service_item()?;
            let end = self.prev_span();
            items.push(Spanned::new(item, start.merge(end)));
        }
        self.expect(Token::RBrace)?;

        Ok(ServiceDecl {
            exported,
            name,
            provides,
            items,
        })
    }

    fn parse_service_item(&mut self) -> Result<ServiceItem, ParseError> {
        match self.peek() {
            Some(Token::Publish) => {
                self.advance();
                self.expect(Token::As)?;
                let s = self.expect(Token::String)?;
                let raw = self.text_of(s);
                let value = raw[1..raw.len() - 1].to_string();
                Ok(ServiceItem::Publish(value))
            }
            Some(Token::On) => {
                self.advance();
                // Detect `on replicated(N)` before parsing as general expression
                if let Some(Token::Ident) = self.peek() {
                    let text = self.text_of(self.peek_span());
                    if text == "replicated" && self.pos + 1 < self.tokens.len() && self.tokens[self.pos + 1].node == Token::LParen {
                        self.advance(); // consume "replicated"
                        self.expect(Token::LParen)?;
                        let n_span = self.expect(Token::Int)?;
                        let n: u32 = self.text_of(n_span).parse().unwrap_or(1);
                        self.expect(Token::RParen)?;
                        return Ok(ServiceItem::Replicated(n));
                    }
                }
                let expr = self.parse_expr(0)?;
                let where_clause = if self.eat(Token::Where) {
                    Some(self.parse_expr(0)?)
                } else {
                    None
                };
                let body = if self.eat(Token::LBrace) {
                    let mut stmts = Vec::new();
                    while !self.check(&Token::RBrace) && !self.at_end() {
                        match self.parse_stmt() {
                            Ok(stmt) => stmts.push(stmt),
                            Err(e) => {
                                self.error(e);
                                if !self.at_end() {
                                    self.advance();
                                }
                            }
                        }
                    }
                    self.expect(Token::RBrace)?;
                    stmts
                } else {
                    Vec::new()
                };
                Ok(ServiceItem::On(OnClauseDecl { filter: expr, where_clause, body }))
            }
            Some(Token::Command) => {
                let cmd = self.parse_command_impl()?;
                Ok(ServiceItem::Command(cmd))
            }
            Some(Token::Query) => {
                let q = self.parse_query_impl()?;
                Ok(ServiceItem::Query(q))
            }
            Some(Token::Component) => {
                let c = self.parse_component_decl()?;
                Ok(ServiceItem::Component(c))
            }
            Some(Token::Ident) if self.text_of(self.peek_span()) == "version" => {
                self.advance(); // consume "version"
                let s = self.expect(Token::String)?;
                let raw = self.text_of(s);
                let value = raw[1..raw.len() - 1].to_string();
                Ok(ServiceItem::Version(value))
            }
            _ => Err(ParseError::new(
                "expected service item (publish, on, command, query, component, version)",
                self.peek_span(),
            )),
        }
    }

    fn parse_command_impl(&mut self) -> Result<CommandImpl, ParseError> {
        self.expect(Token::Command)?;
        let name_span = self.expect(Token::Ident)?;
        let name = self.text_of(name_span).to_string();
        self.expect(Token::LParen)?;
        let params = self.parse_param_list()?;
        self.expect(Token::RParen)?;
        self.expect(Token::Arrow)?;
        let return_type = self.parse_type_expr()?;

        let body = if self.check(&Token::LBrace) {
            let stmts = self.parse_block()?;
            CommandBody::Block(stmts)
        } else {
            self.expect(Token::Via)?;
            let via_expr = self.parse_expr(0)?;
            self.expect(Token::Settle)?;
            let settle_event = self.parse_event_ref()?;
            self.expect(Token::By)?;
            let settle_by = self.parse_expr(0)?;
            CommandBody::ViaSettle {
                via_expr,
                settle_event,
                settle_by,
            }
        };

        Ok(CommandImpl {
            name,
            params,
            return_type,
            body,
        })
    }

    fn parse_query_impl(&mut self) -> Result<QueryImpl, ParseError> {
        self.expect(Token::Query)?;
        let name_span = self.expect(Token::Ident)?;
        let name = self.text_of(name_span).to_string();
        self.expect(Token::LParen)?;
        let params = self.parse_param_list()?;
        self.expect(Token::RParen)?;
        self.expect(Token::Arrow)?;
        let return_type = self.parse_type_expr()?;

        let body = if self.check(&Token::LBrace) {
            let stmts = self.parse_block()?;
            QueryBody::Block(stmts)
        } else {
            self.expect(Token::Via)?;
            let via_expr = self.parse_expr(0)?;
            self.expect(Token::Observe)?;
            let observe_event = self.parse_event_ref()?;
            self.expect(Token::By)?;
            let observe_by = self.parse_expr(0)?;
            self.expect(Token::Return)?;
            let return_expr = self.parse_expr(0)?;
            QueryBody::ViaObserve {
                via_expr,
                observe_event,
                observe_by,
                return_expr,
            }
        };

        Ok(QueryImpl {
            name,
            params,
            return_type,
            body,
        })
    }

    fn parse_event_ref(&mut self) -> Result<EventRef, ParseError> {
        let source_span = self.expect(Token::Ident)?;
        let source = self.text_of(source_span).to_string();
        self.expect(Token::Dot)?;
        let event_span = self.expect(Token::Ident)?;
        let event = self.text_of(event_span).to_string();
        Ok(EventRef { source, event })
    }

    fn parse_component_decl(&mut self) -> Result<ComponentDecl, ParseError> {
        self.expect(Token::Component)?;
        let name_span = self.expect(Token::Ident)?;
        let name = self.text_of(name_span).to_string();
        self.expect(Token::Eq)?;
        self.expect(Token::Spawn)?;
        let service_span = self.expect(Token::Ident)?;
        let service = self.text_of(service_span).to_string();
        self.expect(Token::LParen)?;
        let args = self.parse_arg_list()?;
        self.expect(Token::RParen)?;
        Ok(ComponentDecl {
            name,
            service,
            args,
        })
    }

    fn parse_entry_decl(&mut self) -> Result<EntryDecl, ParseError> {
        self.expect(Token::Entry)?;
        let name = if self.check(&Token::Ident) {
            let span = self.advance().span;
            Some(self.text_of(span).to_string())
        } else {
            None
        };
        self.expect(Token::LParen)?;
        let params = self.parse_param_list()?;
        self.expect(Token::RParen)?;
        let body = self.parse_block()?;
        Ok(EntryDecl { name, params, body })
    }

    fn parse_type_decl(&mut self, exported: bool) -> Result<TypeDecl, ParseError> {
        self.expect(Token::TypeKw)?;
        let name_span = self.expect(Token::Ident)?;
        let name = self.text_of(name_span).to_string();

        let mut type_params = Vec::new();
        if self.eat(Token::Lt) {
            loop {
                let p_span = self.expect(Token::Ident)?;
                type_params.push(self.text_of(p_span).to_string());
                if !self.eat(Token::Comma) {
                    break;
                }
            }
            self.expect(Token::Gt)?;
        }

        self.expect(Token::LBrace)?;
        let mut fields = Vec::new();
        while !self.check(&Token::RBrace) && !self.at_end() {
            let f_name_span = self.expect_ident_or_keyword()?;
            let f_name = self.text_of(f_name_span).to_string();
            self.expect(Token::Colon)?;
            let ty = self.parse_type_expr()?;
            fields.push(Field { name: f_name, ty });
        }
        self.expect(Token::RBrace)?;

        Ok(TypeDecl {
            exported,
            name,
            type_params,
            fields,
        })
    }

    fn parse_fn_decl(&mut self) -> Result<FnDecl, ParseError> {
        self.parse_fn_decl_inner(false)
    }

    fn parse_fn_decl_inner(&mut self, pure: bool) -> Result<FnDecl, ParseError> {
        self.expect(Token::Fn)?;
        let name_span = self.expect(Token::Ident)?;
        let name = self.text_of(name_span).to_string();
        self.expect(Token::LParen)?;
        let params = self.parse_param_list()?;
        self.expect(Token::RParen)?;

        let return_type = if self.eat(Token::Arrow) {
            Some(self.parse_type_expr()?)
        } else {
            None
        };

        let body = self.parse_block()?;
        Ok(FnDecl {
            name,
            params,
            return_type,
            body,
            pure,
        })
    }

    fn parse_substrate_decl(&mut self) -> Result<SubstrateDecl, ParseError> {
        self.expect(Token::Substrate)?;
        let name_span = self.expect(Token::Ident)?;
        let name = self.text_of(name_span).to_string();

        let mut type_params = Vec::new();
        if self.eat(Token::Lt) {
            loop {
                let p_span = self.expect(Token::Ident)?;
                type_params.push(self.text_of(p_span).to_string());
                if !self.eat(Token::Comma) {
                    break;
                }
            }
            self.expect(Token::Gt)?;
        }

        self.expect(Token::LBrace)?;
        let mut ops = Vec::new();
        let mut emits = Vec::new();
        let mut state = Vec::new();
        let mut deps = Vec::new();
        let mut fns = Vec::new();
        let mut on_clauses = Vec::new();

        while !self.check(&Token::RBrace) && !self.at_end() {
            // Parse `fn` declarations inside substrate
            if self.check(&Token::Fn) {
                fns.push(self.parse_fn_decl()?);
                continue;
            }
            // Parse `on` clauses inside substrate
            if self.check(&Token::On) {
                self.advance();
                let expr = self.parse_expr(0)?;
                let where_clause = if self.eat(Token::Where) {
                    Some(self.parse_expr(0)?)
                } else {
                    None
                };
                let body = if self.eat(Token::LBrace) {
                    let mut stmts = Vec::new();
                    while !self.check(&Token::RBrace) && !self.at_end() {
                        match self.parse_stmt() {
                            Ok(stmt) => stmts.push(stmt),
                            Err(e) => {
                                self.error(e);
                                if !self.at_end() {
                                    self.advance();
                                }
                            }
                        }
                    }
                    self.expect(Token::RBrace)?;
                    stmts
                } else {
                    Vec::new()
                };
                on_clauses.push(OnClauseDecl { filter: expr, where_clause, body });
                continue;
            }
            // Parse `uses local_name: SubstrateType`
            if self.check(&Token::Ident) && self.text_of(self.peek_span()) == "uses" {
                self.advance(); // consume "uses"
                let local_span = self.expect(Token::Ident)?;
                let local_name = self.text_of(local_span).to_string();
                self.expect(Token::Colon)?;
                let type_span = self.expect(Token::Ident)?;
                let substrate_type = self.text_of(type_span).to_string();
                deps.push(SubstrateDep { local_name, substrate_type });
                continue;
            }
            if self.check(&Token::State) {
                self.advance();
                self.expect(Token::LBrace)?;
                while !self.check(&Token::RBrace) && !self.at_end() {
                    self.expect(Token::Let)?;
                    let mutable = self.eat(Token::Mut);
                    let name_span = self.expect_ident_or_keyword()?;
                    let s_name = self.text_of(name_span).to_string();
                    let ty = if self.eat(Token::Colon) {
                        Some(self.parse_type_expr()?)
                    } else {
                        None
                    };
                    self.expect(Token::Eq)?;
                    let initial_value = self.parse_expr(0)?;
                    state.push(StateBinding { name: s_name, ty, initial_value, mutable });
                }
                self.expect(Token::RBrace)?;
            } else if self.check(&Token::Op) {
                self.advance();
                let op_name_span = self.expect(Token::Ident)?;
                let op_name = self.text_of(op_name_span).to_string();
                self.expect(Token::LParen)?;
                let params = self.parse_param_list()?;
                self.expect(Token::RParen)?;
                self.expect(Token::Arrow)?;
                let return_type = self.parse_type_expr()?;
                let body = if self.check(&Token::LBrace) {
                    Some(self.parse_block()?)
                } else {
                    None
                };
                ops.push(SubstrateOp {
                    name: op_name,
                    params,
                    return_type,
                    body,
                });
            } else if self.check(&Token::Emits) {
                self.advance();
                self.expect(Token::LBrace)?;
                while !self.check(&Token::RBrace) && !self.at_end() {
                    let ev_name_span = self.expect(Token::Ident)?;
                    let ev_name = self.text_of(ev_name_span).to_string();
                    let mut fields = Vec::new();
                    if self.eat(Token::LBrace) {
                        while !self.check(&Token::RBrace) && !self.at_end() {
                            let f_span = self.expect(Token::Ident)?;
                            fields.push(self.text_of(f_span).to_string());
                            self.eat(Token::Comma);
                        }
                        self.expect(Token::RBrace)?;
                    }
                    emits.push(EmitEvent {
                        name: ev_name,
                        fields,
                    });
                }
                self.expect(Token::RBrace)?;
            } else {
                return Err(ParseError::new(
                    "expected 'op', 'emits', 'state', 'fn', or 'on' in substrate",
                    self.peek_span(),
                ));
            }
        }
        self.expect(Token::RBrace)?;

        Ok(SubstrateDecl {
            name,
            type_params,
            ops,
            emits,
            state,
            deps,
            fns,
            on_clauses,
        })
    }

    fn parse_guarantee_decl(&mut self) -> Result<GuaranteeDecl, ParseError> {
        self.expect(Token::Guarantee)?;
        let name_span = self.expect(Token::Ident)?;
        let name = self.text_of(name_span).to_string();
        self.expect(Token::LBrace)?;

        let mut laws = Vec::new();
        while !self.check(&Token::RBrace) && !self.at_end() {
            if self.eat(Token::Law) {
                self.expect(Token::Colon)?;
                // Collect text until we hit a closing brace or another 'law'
                let start_pos = self.peek_span().start;
                let mut depth = 0;
                while !self.at_end() {
                    if self.check(&Token::RBrace) && depth == 0 {
                        break;
                    }
                    if self.check(&Token::Law) {
                        break;
                    }
                    if self.check(&Token::LBrace) {
                        depth += 1;
                    }
                    if self.check(&Token::RBrace) {
                        depth -= 1;
                    }
                    self.advance();
                }
                let end_pos = self.prev_span().end;
                let body = self.source[start_pos..end_pos].trim().to_string();
                // Try to parse structured law: antecedent => consequent
                let parsed = if let Some(arrow_idx) = body.find("=>") {
                    let antecedent = body[..arrow_idx].trim().to_string();
                    let cons_text = body[arrow_idx + 2..].trim();
                    let (consequent, window_ms) = if let Some(within_idx) = cons_text.find("within ") {
                        let cons = cons_text[..within_idx].trim().to_string();
                        let window_str = cons_text[within_idx + 7..].trim().trim_end_matches("ms");
                        let ms = window_str.parse::<u64>().ok();
                        (cons, ms)
                    } else {
                        (cons_text.to_string(), None)
                    };
                    Some(LawBody::Parsed {
                        antecedent,
                        consequent,
                        binding: None,
                        window_ms,
                    })
                } else {
                    None
                };
                laws.push(LawDecl { body, parsed });
            } else {
                self.advance();
            }
        }
        self.expect(Token::RBrace)?;

        Ok(GuaranteeDecl { name, laws })
    }

    fn parse_param_list(&mut self) -> Result<Vec<Param>, ParseError> {
        let mut params = Vec::new();
        if self.check(&Token::RParen) {
            return Ok(params);
        }
        loop {
            let name_span = self.expect_ident_or_keyword()?;
            let name = self.text_of(name_span).to_string();
            self.expect(Token::Colon)?;
            let ty = self.parse_type_expr()?;
            params.push(Param { name, ty });
            if !self.eat(Token::Comma) {
                break;
            }
        }
        Ok(params)
    }

    fn parse_type_expr(&mut self) -> Result<Spanned<TypeExpr>, ParseError> {
        let start = self.peek_span();

        if self.eat(Token::Cap) {
            let name_span = self.expect(Token::Ident)?;
            let port_name = self.text_of(name_span).to_string();
            let qualifier = if self.eat(Token::At) {
                match self.peek() {
                    Some(Token::Consume) => {
                        self.advance();
                        Some(AuthorityQualifier::Consume)
                    }
                    Some(Token::Borrow) => {
                        self.advance();
                        Some(AuthorityQualifier::Borrow)
                    }
                    Some(Token::Delegate) => {
                        self.advance();
                        Some(AuthorityQualifier::Delegate)
                    }
                    _ => {
                        let span = self.peek_span();
                        return Err(ParseError::expected(
                            "'consume', 'borrow', or 'delegate'",
                            self.peek(),
                            span,
                        ));
                    }
                }
            } else {
                None
            };
            let end = self.prev_span();
            Ok(Spanned::new(TypeExpr::Cap { port_name, qualifier }, start.merge(end)))
        } else {
            let name_span = self.expect(Token::Ident)?;
            let name = self.text_of(name_span).to_string();

            let mut type_args = Vec::new();
            if self.eat(Token::Lt) {
                loop {
                    let arg = self.parse_type_expr()?;
                    type_args.push(arg);
                    if !self.eat(Token::Comma) {
                        break;
                    }
                }
                self.expect(Token::Gt)?;
            }

            let nullable = self.eat(Token::Question);
            let end = self.prev_span();
            Ok(Spanned::new(
                TypeExpr::Named {
                    name,
                    type_args,
                    nullable,
                },
                start.merge(end),
            ))
        }
    }

    fn parse_block(&mut self) -> Result<Vec<Spanned<Stmt>>, ParseError> {
        self.expect(Token::LBrace)?;
        let mut stmts = Vec::new();
        while !self.check(&Token::RBrace) && !self.at_end() {
            let stmt = self.parse_stmt()?;
            stmts.push(stmt);
        }
        self.expect(Token::RBrace)?;
        Ok(stmts)
    }

    fn parse_stmt(&mut self) -> Result<Spanned<Stmt>, ParseError> {
        let start = self.peek_span();

        match self.peek() {
            Some(Token::Let) => {
                self.advance();
                let mutable = self.eat(Token::Mut);
                let name_span = self.expect_ident_or_keyword()?;
                let name = self.text_of(name_span).to_string();
                let ty = if self.eat(Token::Colon) {
                    Some(self.parse_type_expr()?)
                } else {
                    None
                };
                self.expect(Token::Eq)?;
                let value = self.parse_expr(0)?;
                let end = self.prev_span();
                Ok(Spanned::new(Stmt::Let { name, ty, value, mutable }, start.merge(end)))
            }
            Some(Token::Return) => {
                self.advance();
                let value = if !self.check(&Token::RBrace)
                    && !self.at_end()
                    && !self.check(&Token::Semicolon)
                {
                    Some(self.parse_expr(0)?)
                } else {
                    None
                };
                let end = self.prev_span();
                Ok(Spanned::new(Stmt::Return(value), start.merge(end)))
            }
            Some(Token::If) => self.parse_if_stmt(),
            Some(Token::Match) => self.parse_match_stmt(),
            Some(Token::For) => {
                self.advance();
                let var_span = self.expect_ident_or_keyword()?;
                let variable = self.text_of(var_span).to_string();
                self.expect(Token::In)?;
                let iterable = self.parse_expr(0)?;
                let body = self.parse_block()?;
                let end = self.prev_span();
                Ok(Spanned::new(
                    Stmt::For { variable, iterable, body },
                    start.merge(end),
                ))
            }
            Some(Token::While) => {
                self.advance();
                let condition = self.parse_expr(0)?;
                let body = self.parse_block()?;
                let end = self.prev_span();
                Ok(Spanned::new(
                    Stmt::While { condition, body },
                    start.merge(end),
                ))
            }
            Some(Token::Break) => {
                let span = self.advance().span;
                Ok(Spanned::new(Stmt::Break, span))
            }
            Some(Token::Continue) => {
                let span = self.advance().span;
                Ok(Spanned::new(Stmt::Continue, span))
            }
            Some(Token::Emit) => {
                self.advance();
                let type_span = self.expect_ident_or_keyword()?;
                let event_type = self.text_of(type_span).to_string();
                self.expect(Token::LBrace)?;
                let mut fields = Vec::new();
                while !self.check(&Token::RBrace) && !self.at_end() {
                    let f_span = self.expect_ident_or_keyword()?;
                    let f_name = self.text_of(f_span).to_string();
                    self.expect(Token::Colon)?;
                    let val = self.parse_expr(0)?;
                    fields.push((f_name, val));
                    self.eat(Token::Comma);
                }
                let end = self.expect(Token::RBrace)?;
                Ok(Spanned::new(Stmt::Emit { event_type, fields }, start.merge(end)))
            }
            Some(Token::Ident) => {
                // Check for assignment: IDENT = expr
                if self.pos + 1 < self.tokens.len() && self.tokens[self.pos + 1].node == Token::Eq {
                    let name_span = self.advance().span;
                    let name = self.text_of(name_span).to_string();
                    self.advance(); // consume =
                    let value = self.parse_expr(0)?;
                    let end = self.prev_span();
                    return Ok(Spanned::new(Stmt::Assign { name, value }, start.merge(end)));
                }
                let expr = self.parse_expr(0)?;
                let end = expr.span;
                Ok(Spanned::new(Stmt::Expr(expr), start.merge(end)))
            }
            _ => {
                let expr = self.parse_expr(0)?;
                let end = expr.span;
                Ok(Spanned::new(Stmt::Expr(expr), start.merge(end)))
            }
        }
    }

    fn parse_if_stmt(&mut self) -> Result<Spanned<Stmt>, ParseError> {
        let start = self.peek_span();
        self.expect(Token::If)?;
        let condition = self.parse_expr(0)?;
        let then_block = self.parse_block()?;
        let else_block = if self.eat(Token::Else) {
            if self.check(&Token::If) {
                let nested = self.parse_if_stmt()?;
                Some(vec![nested])
            } else {
                Some(self.parse_block()?)
            }
        } else {
            None
        };
        let end = self.prev_span();
        Ok(Spanned::new(
            Stmt::If {
                condition,
                then_block,
                else_block,
            },
            start.merge(end),
        ))
    }

    fn parse_pattern(&mut self) -> Result<Spanned<Pattern>, ParseError> {
        let pat_start = self.peek_span();
        let pattern = match self.peek() {
            Some(Token::Ident) => {
                let span = self.advance().span;
                let name = self.text_of(span).to_string();
                if name == "_" {
                    Pattern::Wildcard
                } else if name == "Some" && self.check(&Token::LParen) {
                    // Some(pattern) — option pattern
                    self.advance(); // consume (
                    let inner = self.parse_pattern()?;
                    self.expect(Token::RParen)?;
                    Pattern::Some(Box::new(inner))
                } else if name == "Ok" && self.check(&Token::LParen) {
                    self.advance(); // consume (
                    let inner = self.parse_pattern()?;
                    self.expect(Token::RParen)?;
                    Pattern::Ok(Box::new(inner))
                } else if name == "Err" && self.check(&Token::LParen) {
                    self.advance(); // consume (
                    let inner = self.parse_pattern()?;
                    self.expect(Token::RParen)?;
                    Pattern::Err(Box::new(inner))
                } else if name == "None" {
                    // None pattern (as identifier "None")
                    Pattern::None
                } else if name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
                    && self.check(&Token::LBrace)
                {
                    // Struct pattern: Point { x, y } or Point { x: inner_pat }
                    self.advance(); // consume {
                    let mut fields = Vec::new();
                    while !self.check(&Token::RBrace) && !self.at_end() {
                        let f_span = self.expect(Token::Ident)?;
                        let f_name = self.text_of(f_span).to_string();
                        let inner = if self.eat(Token::Colon) {
                            Some(self.parse_pattern()?)
                        } else {
                            None
                        };
                        fields.push((f_name, inner));
                        if !self.eat(Token::Comma) {
                            break;
                        }
                    }
                    self.expect(Token::RBrace)?;
                    Pattern::Struct { name, fields }
                } else {
                    Pattern::Ident(name)
                }
            }
            Some(Token::None) => {
                // `none` keyword as pattern
                self.advance();
                Pattern::None
            }
            Some(Token::LBracket) => {
                // List pattern: [a, b, ..rest]
                self.advance(); // consume [
                let mut elements = Vec::new();
                let mut rest = None;
                while !self.check(&Token::RBracket) && !self.at_end() {
                    if self.eat(Token::DotDot) {
                        // Rest pattern
                        let rest_pat = self.parse_pattern()?;
                        rest = Some(Box::new(rest_pat));
                        // trailing comma optional
                        self.eat(Token::Comma);
                        break;
                    }
                    let elem = self.parse_pattern()?;
                    elements.push(elem);
                    if !self.eat(Token::Comma) {
                        break;
                    }
                }
                self.expect(Token::RBracket)?;
                Pattern::List { elements, rest }
            }
            Some(Token::String) => {
                let span = self.advance().span;
                let raw = self.text_of(span);
                Pattern::Literal(Literal::String(raw[1..raw.len() - 1].to_string()))
            }
            Some(Token::Int) => {
                let span = self.advance().span;
                let val: i64 = self.text_of(span).parse().unwrap_or(0);
                Pattern::Literal(Literal::Int(val))
            }
            Some(Token::True) => {
                self.advance();
                Pattern::Literal(Literal::Bool(true))
            }
            Some(Token::False) => {
                self.advance();
                Pattern::Literal(Literal::Bool(false))
            }
            _ => {
                return Err(ParseError::new("expected pattern", self.peek_span()));
            }
        };
        let pat_end = self.prev_span();
        Ok(Spanned::new(pattern, pat_start.merge(pat_end)))
    }

    fn parse_match_stmt(&mut self) -> Result<Spanned<Stmt>, ParseError> {
        let start = self.peek_span();
        self.expect(Token::Match)?;
        let expr = self.parse_expr(0)?;
        self.expect(Token::LBrace)?;

        let mut arms = Vec::new();
        while !self.check(&Token::RBrace) && !self.at_end() {
            let pattern = self.parse_pattern()?;
            let guard = if self.eat(Token::If) {
                Some(self.parse_expr(0)?)
            } else {
                None
            };
            self.expect(Token::FatArrow)?;
            let body = if self.check(&Token::LBrace) {
                self.parse_block()?
            } else {
                let stmt = self.parse_stmt()?;
                vec![stmt]
            };
            arms.push(MatchArm {
                pattern,
                guard,
                body,
            });
            self.eat(Token::Comma);
        }
        self.expect(Token::RBrace)?;
        let end = self.prev_span();
        Ok(Spanned::new(Stmt::Match { expr, arms }, start.merge(end)))
    }

    // Pratt-style expression parser
    fn parse_expr(&mut self, min_prec: u8) -> Result<Spanned<Expr>, ParseError> {
        let mut left = self.parse_unary()?;
        left = self.parse_postfix(left)?;

        loop {
            let op = match self.peek() {
                Some(Token::PipePipe) => BinaryOp::Or,
                Some(Token::AmpAmp) => BinaryOp::And,
                Some(Token::Pipe) => BinaryOp::BitOr,
                Some(Token::Caret) => BinaryOp::BitXor,
                Some(Token::Ampersand) => BinaryOp::BitAnd,
                Some(Token::EqEq) => BinaryOp::Eq,
                Some(Token::BangEq) => BinaryOp::Neq,
                Some(Token::Lt) => BinaryOp::Lt,
                Some(Token::Gt) => BinaryOp::Gt,
                Some(Token::LtEq) => BinaryOp::LtEq,
                Some(Token::GtEq) => BinaryOp::GtEq,
                Some(Token::LtLt) => BinaryOp::Shl,
                Some(Token::GtGt) => BinaryOp::Shr,
                Some(Token::Plus) => BinaryOp::Add,
                Some(Token::Minus) => BinaryOp::Sub,
                Some(Token::Star) => BinaryOp::Mul,
                Some(Token::Slash) => BinaryOp::Div,
                Some(Token::Percent) => BinaryOp::Mod,
                _ => break,
            };

            let prec = op.precedence();
            if prec < min_prec {
                break;
            }

            self.advance();
            let right = self.parse_expr(prec + 1)?;
            let span = left.span.merge(right.span);
            left = Spanned::new(
                Expr::Binary {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
                span,
            );
        }

        Ok(left)
    }

    // Postfix: method call, field access, fn call, index
    fn parse_postfix(&mut self, mut left: Spanned<Expr>) -> Result<Spanned<Expr>, ParseError> {
        loop {
            if self.check(&Token::Dot) {
                self.advance();
                let name_span = self.expect(Token::Ident)?;
                let name = self.text_of(name_span).to_string();

                if self.eat(Token::LParen) {
                    let args = self.parse_arg_list()?;
                    let end = self.expect(Token::RParen)?;
                    let span = left.span.merge(end);
                    left = Spanned::new(
                        Expr::MethodCall {
                            receiver: Box::new(left),
                            method: name,
                            args,
                        },
                        span,
                    );
                } else {
                    let span = left.span.merge(name_span);
                    left = Spanned::new(
                        Expr::FieldAccess {
                            receiver: Box::new(left),
                            field: name,
                        },
                        span,
                    );
                }
            } else if self.check(&Token::LParen) {
                self.advance();
                let args = self.parse_arg_list()?;
                let end = self.expect(Token::RParen)?;
                let span = left.span.merge(end);
                left = Spanned::new(
                    Expr::FnCall {
                        func: Box::new(left),
                        args,
                    },
                    span,
                );
            } else if self.check(&Token::LBracket) {
                self.advance();
                let index = self.parse_expr(0)?;
                let end = self.expect(Token::RBracket)?;
                let span = left.span.merge(end);
                left = Spanned::new(
                    Expr::Index {
                        receiver: Box::new(left),
                        index: Box::new(index),
                    },
                    span,
                );
            } else if self.check(&Token::Question) {
                let end = self.advance().span;
                let span = left.span.merge(end);
                left = Spanned::new(
                    Expr::Try { expr: Box::new(left) },
                    span,
                );
            } else {
                break;
            }
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Spanned<Expr>, ParseError> {
        let start = self.peek_span();
        match self.peek() {
            Some(Token::Await) => {
                self.advance();
                // Parse at precedence 11 (above all binary ops at 1-10)
                // so postfix (method calls, field access) binds to the operand.
                let operand = self.parse_expr(11)?;
                let span = start.merge(operand.span);
                Ok(Spanned::new(
                    Expr::Await {
                        expr: Box::new(operand),
                    },
                    span,
                ))
            }
            Some(Token::Minus) => {
                self.advance();
                let operand = self.parse_unary()?;
                let span = start.merge(operand.span);
                Ok(Spanned::new(
                    Expr::Unary {
                        op: UnaryOp::Neg,
                        operand: Box::new(operand),
                    },
                    span,
                ))
            }
            Some(Token::Bang) => {
                self.advance();
                let operand = self.parse_unary()?;
                let span = start.merge(operand.span);
                Ok(Spanned::new(
                    Expr::Unary {
                        op: UnaryOp::Not,
                        operand: Box::new(operand),
                    },
                    span,
                ))
            }
            Some(Token::Tilde) => {
                self.advance();
                let operand = self.parse_unary()?;
                let span = start.merge(operand.span);
                Ok(Spanned::new(
                    Expr::Unary {
                        op: UnaryOp::BitNot,
                        operand: Box::new(operand),
                    },
                    span,
                ))
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<Spanned<Expr>, ParseError> {
        let start = self.peek_span();

        match self.peek() {
            Some(Token::String) => {
                let span = self.advance().span;
                let raw = self.text_of(span);
                let value = raw[1..raw.len() - 1].to_string();
                Ok(Spanned::new(Expr::Literal(Literal::String(value)), span))
            }
            Some(Token::Int) => {
                let span = self.advance().span;
                let val: i64 = self.text_of(span).parse().unwrap_or(0);
                Ok(Spanned::new(Expr::Literal(Literal::Int(val)), span))
            }
            Some(Token::HexInt) => {
                let span = self.advance().span;
                let text = self.text_of(span);
                let val = i64::from_str_radix(&text[2..], 16).unwrap_or(0);
                Ok(Spanned::new(Expr::Literal(Literal::Int(val)), span))
            }
            Some(Token::BytesLiteral) => {
                let span = self.advance().span;
                let raw = self.text_of(span);
                // Strip b"..." wrapper, decode hex pairs
                let hex = &raw[2..raw.len() - 1];
                let mut bytes = Vec::with_capacity(hex.len() / 2);
                let mut i = 0;
                while i + 1 < hex.len() {
                    let byte = u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
                    bytes.push(byte);
                    i += 2;
                }
                Ok(Spanned::new(Expr::Literal(Literal::Bytes(bytes)), span))
            }
            Some(Token::Float) => {
                let span = self.advance().span;
                let val: f64 = self.text_of(span).parse().unwrap_or(0.0);
                Ok(Spanned::new(Expr::Literal(Literal::Float(val)), span))
            }
            Some(Token::True) => {
                let span = self.advance().span;
                Ok(Spanned::new(Expr::Literal(Literal::Bool(true)), span))
            }
            Some(Token::False) => {
                let span = self.advance().span;
                Ok(Spanned::new(Expr::Literal(Literal::Bool(false)), span))
            }
            Some(Token::None) => {
                let span = self.advance().span;
                Ok(Spanned::new(Expr::None, span))
            }
            Some(Token::Resolve) => {
                self.advance();
                let port_span = self.expect(Token::Ident)?;
                let port = self.text_of(port_span).to_string();
                self.expect(Token::LBracket)?;
                let name = self.parse_expr(0)?;
                self.expect(Token::RBracket)?;
                let profile = if self.eat(Token::With) {
                    self.expect(Token::Profile)?;
                    let p_span = self.expect(Token::Ident)?;
                    Some(self.text_of(p_span).to_string())
                } else {
                    None
                };
                let end = self.prev_span();
                Ok(Spanned::new(
                    Expr::Resolve {
                        port,
                        name: Box::new(name),
                        profile,
                    },
                    start.merge(end),
                ))
            }
            Some(Token::LParen) => {
                self.advance();
                let expr = self.parse_expr(0)?;
                self.expect(Token::RParen)?;
                Ok(expr)
            }
            Some(Token::Concurrent) => {
                self.advance();
                self.expect(Token::LBrace)?;
                let mut exprs = Vec::new();
                while !self.check(&Token::RBrace) && !self.at_end() {
                    let expr = self.parse_expr(0)?;
                    exprs.push(expr);
                    // Allow optional semicolons between expressions
                    self.eat(Token::Semicolon);
                }
                let end = self.expect(Token::RBrace)?;
                Ok(Spanned::new(
                    Expr::ConcurrentAwait { exprs },
                    start.merge(end),
                ))
            }
            Some(Token::Select) => {
                self.advance();
                let timeout_ms = if !self.check(&Token::LBrace) {
                    Some(Box::new(self.parse_expr(0)?))
                } else {
                    None
                };
                self.expect(Token::LBrace)?;
                let mut arms = Vec::new();
                let mut else_body = None;
                while !self.check(&Token::RBrace) && !self.at_end() {
                    if self.check(&Token::Else) {
                        self.advance();
                        self.expect(Token::FatArrow)?;
                        else_body = Some(self.parse_block()?);
                        break;
                    }
                    let binding_span = self.expect(Token::Ident)?;
                    let binding = self.text_of(binding_span).to_string();
                    self.expect(Token::Eq)?;
                    let expr = self.parse_expr(0)?;
                    self.expect(Token::FatArrow)?;
                    let body = self.parse_block()?;
                    arms.push(SelectArm { binding, expr, body });
                }
                let end = self.expect(Token::RBrace)?;
                Ok(Spanned::new(
                    Expr::Select { timeout_ms, arms, else_body },
                    start.merge(end),
                ))
            }
            Some(Token::LBracket) => {
                self.advance(); // consume [
                let mut elements = Vec::new();
                if !self.check(&Token::RBracket) {
                    loop {
                        let elem = self.parse_expr(0)?;
                        elements.push(elem);
                        if !self.eat(Token::Comma) {
                            break;
                        }
                        // Allow trailing comma
                        if self.check(&Token::RBracket) {
                            break;
                        }
                    }
                }
                let end = self.expect(Token::RBracket)?;
                Ok(Spanned::new(Expr::ListLiteral { elements }, start.merge(end)))
            }
            Some(Token::LBrace) => {
                // Disambiguate: map literal vs block
                // Map literal: { String : ...
                if self.pos + 2 < self.tokens.len()
                    && self.tokens[self.pos + 1].node == Token::String
                    && self.tokens[self.pos + 2].node == Token::Colon
                {
                    self.advance(); // consume {
                    let mut entries = Vec::new();
                    while !self.check(&Token::RBrace) && !self.at_end() {
                        let key = self.parse_expr(0)?;
                        self.expect(Token::Colon)?;
                        let value = self.parse_expr(0)?;
                        entries.push((key, value));
                        if !self.eat(Token::Comma) {
                            break;
                        }
                        // Allow trailing comma
                        if self.check(&Token::RBrace) {
                            break;
                        }
                    }
                    let end = self.expect(Token::RBrace)?;
                    return Ok(Spanned::new(Expr::MapLiteral { entries }, start.merge(end)));
                }
                let stmts = self.parse_block()?;
                let end = self.prev_span();
                Ok(Spanned::new(Expr::Block(stmts), start.merge(end)))
            }
            Some(Token::Pipe) => {
                self.advance(); // consume |
                let mut params = Vec::new();
                if !self.check(&Token::Pipe) {
                    loop {
                        let name_span = self.expect_ident_or_keyword()?;
                        let name = self.text_of(name_span).to_string();
                        let ty = if self.eat(Token::Colon) {
                            self.parse_type_expr()?
                        } else {
                            // Inferred type
                            Spanned::new(
                                TypeExpr::Named {
                                    name: "Any".to_string(),
                                    type_args: Vec::new(),
                                    nullable: false,
                                },
                                name_span,
                            )
                        };
                        params.push(Param { name, ty });
                        if !self.eat(Token::Comma) {
                            break;
                        }
                    }
                }
                self.expect(Token::Pipe)?;
                let body = if self.check(&Token::LBrace) {
                    self.parse_block()?
                } else {
                    // Single expression body: wrap as return stmt
                    let expr = self.parse_expr(0)?;
                    let span = expr.span;
                    vec![Spanned::new(Stmt::Return(Some(expr)), span)]
                };
                let end = self.prev_span();
                Ok(Spanned::new(Expr::Closure { params, body }, start.merge(end)))
            }
            Some(Token::PipePipe) => {
                // || is zero-arg closure
                self.advance(); // consume ||
                let body = if self.check(&Token::LBrace) {
                    self.parse_block()?
                } else {
                    let expr = self.parse_expr(0)?;
                    let span = expr.span;
                    vec![Spanned::new(Stmt::Return(Some(expr)), span)]
                };
                let end = self.prev_span();
                Ok(Spanned::new(Expr::Closure { params: Vec::new(), body }, start.merge(end)))
            }
            Some(Token::Import) => {
                // Dynamic import: import("path")
                self.advance();
                self.expect(Token::LParen)?;
                let path = self.parse_expr(0)?;
                let end = self.expect(Token::RParen)?;
                Ok(Spanned::new(
                    Expr::DynamicImport { path: Box::new(path) },
                    start.merge(end),
                ))
            }
            Some(Token::Match) => {
                self.advance();
                let expr = self.parse_expr(0)?;
                self.expect(Token::LBrace)?;
                let mut arms = Vec::new();
                while !self.check(&Token::RBrace) && !self.at_end() {
                    let pattern = self.parse_pattern()?;
                    let guard = if self.eat(Token::If) {
                        Some(self.parse_expr(0)?)
                    } else {
                        None
                    };
                    self.expect(Token::FatArrow)?;
                    let body = if self.check(&Token::LBrace) {
                        self.parse_block()?
                    } else {
                        let stmt = self.parse_stmt()?;
                        vec![stmt]
                    };
                    arms.push(MatchArm {
                        pattern,
                        guard,
                        body,
                    });
                    self.eat(Token::Comma);
                }
                let end = self.expect(Token::RBrace)?;
                Ok(Spanned::new(
                    Expr::Match {
                        expr: Box::new(expr),
                        arms,
                    },
                    start.merge(end),
                ))
            }
            Some(Token::Ident) => {
                let span = self.advance().span;
                let name = self.text_of(span).to_string();

                // Macro call: name!(args)
                if self.check(&Token::Bang) {
                    self.advance(); // consume !
                    self.expect(Token::LParen)?;
                    let args = self.parse_arg_list()?;
                    let end = self.expect(Token::RParen)?;
                    return Ok(Spanned::new(
                        Expr::MacroCall { name, args },
                        span.merge(end),
                    ));
                }

                // Struct literal heuristic: Uppercase name followed by { IDENT :
                if name.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
                    && self.check(&Token::LBrace)
                {
                    // Lookahead: { IDENT :
                    if self.pos + 2 < self.tokens.len()
                        && self.tokens[self.pos + 1].node == Token::Ident
                        && self.tokens[self.pos + 2].node == Token::Colon
                    {
                        self.advance(); // consume {
                        let mut fields = Vec::new();
                        while !self.check(&Token::RBrace) && !self.at_end() {
                            let f_span = self.expect(Token::Ident)?;
                            let f_name = self.text_of(f_span).to_string();
                            self.expect(Token::Colon)?;
                            let val = self.parse_expr(0)?;
                            fields.push((f_name, val));
                            self.eat(Token::Comma);
                        }
                        let end = self.expect(Token::RBrace)?;
                        return Ok(Spanned::new(
                            Expr::StructLiteral { name, fields },
                            span.merge(end),
                        ));
                    }
                }

                Ok(Spanned::new(Expr::Ident(name), span))
            }
            _ => Err(ParseError::expected("expression", self.peek(), start)),
        }
    }

    fn parse_profile_decl(&mut self) -> Result<ProfileDecl, ParseError> {
        self.expect(Token::Profile)?;
        let name_span = self.expect(Token::Ident)?;
        let name = self.text_of(name_span).to_string();
        self.expect(Token::LBrace)?;

        let mut preferences = Vec::new();
        while !self.check(&Token::RBrace) && !self.at_end() {
            let key_span = self.expect_ident_or_keyword()?;
            let key = self.text_of(key_span).to_string();
            self.expect(Token::Colon)?;
            let val_span = self.expect(Token::String)?;
            let raw = self.text_of(val_span);
            let value = raw[1..raw.len() - 1].to_string();
            preferences.push((key, value));
        }
        self.expect(Token::RBrace)?;

        Ok(ProfileDecl { name, preferences })
    }

    fn parse_macro_decl(&mut self) -> Result<MacroDecl, ParseError> {
        self.expect(Token::Macro)?;
        let name_span = self.expect(Token::Ident)?;
        let name = self.text_of(name_span).to_string();
        self.expect(Token::LParen)?;

        let mut params = Vec::new();
        if !self.check(&Token::RParen) {
            loop {
                let p_span = self.expect(Token::Ident)?;
                let p_name = self.text_of(p_span).to_string();
                self.expect(Token::Colon)?;
                let kind_span = self.expect(Token::Ident)?;
                let kind_str = self.text_of(kind_span);
                let kind = match kind_str {
                    "Expr" => MacroParamKind::Expr,
                    "Stmt" => MacroParamKind::Stmt,
                    "Ident" => MacroParamKind::Ident,
                    "Block" => MacroParamKind::Block,
                    _ => {
                        return Err(ParseError::new(
                            &format!("expected macro param kind (Expr/Stmt/Ident/Block), got '{kind_str}'"),
                            kind_span,
                        ));
                    }
                };
                params.push(MacroParam { name: p_name, kind });
                if !self.eat(Token::Comma) {
                    break;
                }
            }
        }
        self.expect(Token::RParen)?;

        let body = self.parse_block()?;
        Ok(MacroDecl { name, params, body })
    }

    fn parse_arg_list(&mut self) -> Result<Vec<Spanned<Expr>>, ParseError> {
        let mut args = Vec::new();
        if self.check(&Token::RParen) {
            return Ok(args);
        }
        loop {
            let arg = self.parse_expr(0)?;
            args.push(arg);
            if !self.eat(Token::Comma) {
                break;
            }
        }
        Ok(args)
    }
}

pub fn parse(source: &str, tokens: &[Spanned<Token>]) -> (Program, Vec<ParseError>) {
    let mut parser = Parser::new(tokens, source);
    let program = parser.parse_program();
    (program, parser.errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::tokenize;

    fn parse_source(src: &str) -> (Program, Vec<ParseError>) {
        let tokens = tokenize(src).unwrap();
        parse(src, &tokens)
    }

    #[test]
    fn test_parse_hello() {
        let (prog, errors) = parse_source(r#""Hello, World!""#);
        assert!(errors.is_empty(), "errors: {:?}", errors);
        assert_eq!(prog.items.len(), 1);
    }

    #[test]
    fn test_parse_entry() {
        let (prog, errors) = parse_source(
            r#"entry(out: cap Stdout) {
                out.writeln("Hello, World!")
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
        assert_eq!(prog.items.len(), 1);
        match &prog.items[0].node {
            Item::Entry(e) => {
                assert_eq!(e.params.len(), 1);
                assert_eq!(e.params[0].name, "out");
            }
            _ => panic!("expected entry"),
        }
    }

    #[test]
    fn test_parse_port() {
        let (prog, errors) = parse_source(
            r#"port KV {
                get(key: String) -> String? [query, idempotent]
                put(key: String, value: String) -> Ack [command]
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
        assert_eq!(prog.items.len(), 1);
        match &prog.items[0].node {
            Item::Port(p) => {
                assert_eq!(p.name, "KV");
                assert_eq!(p.methods.len(), 2);
            }
            _ => panic!("expected port"),
        }
    }

    #[test]
    fn test_parse_let() {
        let (_prog, errors) = parse_source(
            r#"entry() {
                let x = 42
                let y: String = "hello"
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
    }

    #[test]
    fn test_parse_binary_expr() {
        let (_prog, errors) = parse_source(
            r#"entry() {
                let x = 1 + 2 * 3
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
    }

    #[test]
    fn test_parse_service() {
        let (prog, errors) = parse_source(
            r#"service HelloWorld provides Hello {
                publish as "hello/world"
                query greet() -> String {
                    return "Hello!"
                }
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
        match &prog.items[0].node {
            Item::Service(s) => {
                assert_eq!(s.name, "HelloWorld");
                assert_eq!(s.provides, "Hello");
            }
            _ => panic!("expected service"),
        }
    }

    #[test]
    fn test_parse_authority_qualifier_delegate() {
        let (prog, errors) = parse_source(
            r#"entry(out: cap Stdout @delegate) {
                out.writeln("Hello")
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
        match &prog.items[0].node {
            Item::Entry(e) => {
                assert_eq!(e.params[0].name, "out");
                match &e.params[0].ty.node {
                    TypeExpr::Cap { port_name, qualifier } => {
                        assert_eq!(port_name, "Stdout");
                        assert_eq!(*qualifier, Some(AuthorityQualifier::Delegate));
                    }
                    _ => panic!("expected Cap type"),
                }
            }
            _ => panic!("expected entry"),
        }
    }

    #[test]
    fn test_parse_authority_qualifier_consume() {
        let (prog, errors) = parse_source(
            r#"entry(db: cap KV @consume) {
                return none
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
        match &prog.items[0].node {
            Item::Entry(e) => {
                match &e.params[0].ty.node {
                    TypeExpr::Cap { qualifier, .. } => {
                        assert_eq!(*qualifier, Some(AuthorityQualifier::Consume));
                    }
                    _ => panic!("expected Cap type"),
                }
            }
            _ => panic!("expected entry"),
        }
    }

    #[test]
    fn test_parse_authority_qualifier_borrow() {
        let (prog, errors) = parse_source(
            r#"entry(db: cap KV @borrow) {
                return none
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
        match &prog.items[0].node {
            Item::Entry(e) => {
                match &e.params[0].ty.node {
                    TypeExpr::Cap { qualifier, .. } => {
                        assert_eq!(*qualifier, Some(AuthorityQualifier::Borrow));
                    }
                    _ => panic!("expected Cap type"),
                }
            }
            _ => panic!("expected entry"),
        }
    }

    #[test]
    fn test_parse_cap_without_qualifier() {
        let (prog, errors) = parse_source(
            r#"entry(out: cap Stdout) {
                return none
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
        match &prog.items[0].node {
            Item::Entry(e) => {
                match &e.params[0].ty.node {
                    TypeExpr::Cap { qualifier, .. } => {
                        assert_eq!(*qualifier, None);
                    }
                    _ => panic!("expected Cap type"),
                }
            }
            _ => panic!("expected entry"),
        }
    }

    #[test]
    fn test_parse_struct_pattern() {
        let (_prog, errors) = parse_source(
            r#"entry() {
                match p {
                    Point { x, y } => return x + y
                    _ => return 0
                }
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
    }

    #[test]
    fn test_parse_list_pattern() {
        let (_prog, errors) = parse_source(
            r#"entry() {
                match items {
                    [head, ..rest] => { return head }
                    [] => { return 0 }
                    _ => { return -1 }
                }
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
    }

    #[test]
    fn test_parse_guard() {
        let (_prog, errors) = parse_source(
            r#"entry() {
                match x {
                    n if n > 0 => return "positive"
                    _ => return "other"
                }
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
    }

    #[test]
    fn test_parse_match_as_expression() {
        let (_prog, errors) = parse_source(
            r#"entry() {
                let x = match 42 {
                    0 => "zero"
                    _ => "other"
                }
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
    }

    #[test]
    fn test_parse_match_expr_with_guards() {
        let (_prog, errors) = parse_source(
            r#"entry() {
                let x = match n {
                    v if v > 0 => "positive"
                    _ => "other"
                }
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
    }

    #[test]
    fn test_parse_match_expr_with_block_arms() {
        let (_prog, errors) = parse_source(
            r#"entry() {
                let result = match value {
                    1 => { return "one" }
                    2 => { return "two" }
                    _ => { return "other" }
                }
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
    }

    #[test]
    fn test_parse_on_replicated() {
        let (prog, errors) = parse_source(
            r#"service Foo provides Bar {
                on replicated(5)
                publish as "foo"
                query get() -> String {
                    return "hello"
                }
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
        match &prog.items[0].node {
            Item::Service(s) => {
                let has_replicated = s.items.iter().any(|i| matches!(&i.node, ServiceItem::Replicated(5)));
                assert!(has_replicated, "expected ServiceItem::Replicated(5)");
            }
            _ => panic!("expected service"),
        }
    }

    #[test]
    fn test_parse_service_version() {
        let (prog, errors) = parse_source(
            r#"service Foo provides Bar {
                version "1.2.3"
                publish as "foo"
                query get() -> String {
                    return "hello"
                }
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
        match &prog.items[0].node {
            Item::Service(s) => {
                let has_version = s.items.iter().any(|i| matches!(&i.node, ServiceItem::Version(v) if v == "1.2.3"));
                assert!(has_version, "expected ServiceItem::Version(\"1.2.3\")");
            }
            _ => panic!("expected service"),
        }
    }

    #[test]
    fn test_match_stmt_still_works() {
        let (_prog, errors) = parse_source(
            r#"entry() {
                match x {
                    1 => return "one"
                    _ => return "other"
                }
            }"#,
        );
        assert!(errors.is_empty(), "errors: {:?}", errors);
    }

    #[test]
    fn test_parse_aliased_selective_import() {
        let (program, errors) = parse_source("import kv.storage.{KV as KVPort, Logger}");
        assert!(errors.is_empty(), "errors: {:?}", errors);
        assert_eq!(program.imports.len(), 1);
        let imp = &program.imports[0].node;
        assert_eq!(imp.path, vec!["kv", "storage"]);
        let names = imp.names.as_ref().unwrap();
        assert_eq!(names.len(), 2);
        assert_eq!(names[0], ("KV".to_string(), Some("KVPort".to_string())));
        assert_eq!(names[1], ("Logger".to_string(), None));
    }

    #[test]
    fn test_parse_selective_import_no_alias() {
        let (program, errors) = parse_source("import helpers.math.{add, mul}");
        assert!(errors.is_empty(), "errors: {:?}", errors);
        let imp = &program.imports[0].node;
        let names = imp.names.as_ref().unwrap();
        assert_eq!(names.len(), 2);
        assert_eq!(names[0], ("add".to_string(), None));
        assert_eq!(names[1], ("mul".to_string(), None));
    }
}
