//! Recursive-descent parser with a documented precedence table and hard
//! nesting limits.
//!
//! # Operator precedence
//!
//! From lowest to highest binding power:
//!
//! 1. assignment `= += -= *= /=` (right associative; assignment targets must be
//!    an identifier, a dot access, or an index access)
//! 2. logical OR `||`
//! 3. logical AND `&&`
//! 4. equality `== !=`
//! 5. relational `< <= > >=`
//! 6. additive `+ -`
//! 7. multiplicative `* / %`
//! 8. unary `! - +`
//! 9. postfix call `()`, dot `.`, and index `[]`
//!
//! `&&` and `||` short-circuit and return an operand, as in JavaScript. Every
//! binary operator is left associative. The parser rejects any token that is
//! not part of the subset with a typed [`JsError`] that names the feature.

use crate::js::ast::{
    AssignOp, BinaryOp, Expr, ExprKind, Program, Stmt, StmtKind, UnaryOp, VarKind,
};
use crate::js::error::{JsError, Pos};
use crate::js::lexer::{Op, Spanned, Token};
use crate::js::{is_dangerous_name, is_reserved_name, is_valid_identifier};

/// Parses a token stream into a [`Program`].
pub(crate) struct Parser<'a> {
    tokens: &'a [Spanned],
    eof: Spanned,
    index: usize,
    depth: usize,
    max_depth: usize,
    loop_depth: usize,
    function_depth: usize,
}

impl<'a> Parser<'a> {
    /// Creates a parser over `tokens`, enforcing `max_depth` nesting levels.
    pub(crate) fn new(tokens: &'a [Spanned], max_depth: usize) -> Self {
        let eof = tokens.last().cloned().unwrap_or(Spanned {
            token: Token::Eof,
            pos: Pos::default(),
        });
        Self {
            tokens,
            eof,
            index: 0,
            depth: 0,
            max_depth,
            loop_depth: 0,
            function_depth: 0,
        }
    }

    /// Parses the whole token stream, which must end with [`Token::Eof`].
    pub(crate) fn parse_program(&mut self) -> Result<Program, JsError> {
        let mut body = Vec::new();
        while !matches!(self.peek_token(), Token::Eof) {
            body.push(self.parse_stmt()?);
        }
        Ok(Program { body })
    }

    fn peek(&self) -> &Spanned {
        // The lexer always appends Eof and `advance` never steps past it, so
        // the fallback is only a defensive guard against a malformed stream.
        self.tokens.get(self.index).unwrap_or(&self.eof)
    }

    fn peek_token(&self) -> &Token {
        &self.peek().token
    }

    fn pos(&self) -> Pos {
        self.peek().pos
    }

    fn advance(&mut self) -> Spanned {
        let token = self.peek().clone();
        if !matches!(token.token, Token::Eof) {
            self.index += 1;
        }
        token
    }

    fn at_op(&self, op: Op) -> bool {
        matches!(self.peek_token(), Token::Op(found) if *found == op)
    }

    fn at_ident(&self, name: &str) -> bool {
        matches!(self.peek_token(), Token::Ident(found) if found == name)
    }

    fn expect_op(&mut self, op: Op, label: &str) -> Result<Spanned, JsError> {
        if self.at_op(op) {
            Ok(self.advance())
        } else {
            Err(JsError::Parse {
                pos: self.pos(),
                message: format!("expected '{label}'"),
            })
        }
    }

    fn enter(&mut self) -> Result<(), JsError> {
        self.depth += 1;
        if self.depth > self.max_depth {
            self.depth -= 1;
            return Err(JsError::AstTooDeep {
                pos: self.pos(),
                limit: self.max_depth,
            });
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn parse_stmt(&mut self) -> Result<Stmt, JsError> {
        self.enter()?;
        let result = self.parse_stmt_inner();
        self.leave();
        result
    }

    fn parse_stmt_inner(&mut self) -> Result<Stmt, JsError> {
        let pos = self.pos();
        if let Token::Ident(keyword) = self.peek_token().clone() {
            match keyword.as_str() {
                "true" | "false" | "null" => {}
                "let" => return self.parse_var_decl(pos, VarKind::Let),
                "var" => return self.parse_var_decl(pos, VarKind::Var),
                "const" => return self.parse_var_decl(pos, VarKind::Const),
                "if" => return self.parse_if(pos),
                "while" => return self.parse_while(pos),
                "for" => return self.parse_for(pos),
                "function" => return self.parse_function_decl(pos),
                "return" => return self.parse_return(pos),
                "break" => {
                    self.advance();
                    self.expect_op(Op::Semicolon, ";")?;
                    if self.loop_depth == 0 {
                        return Err(JsError::Parse {
                            pos,
                            message: "'break' is only valid inside a loop".to_owned(),
                        });
                    }
                    return Ok(Stmt {
                        kind: StmtKind::Break,
                        pos,
                    });
                }
                "continue" => {
                    self.advance();
                    self.expect_op(Op::Semicolon, ";")?;
                    if self.loop_depth == 0 {
                        return Err(JsError::Parse {
                            pos,
                            message: "'continue' is only valid inside a loop".to_owned(),
                        });
                    }
                    return Ok(Stmt {
                        kind: StmtKind::Continue,
                        pos,
                    });
                }
                // `this` is parsed as an expression so that Acrobat-style
                // `this.getField(...)` works when a context is injected. When
                // no context is present, evaluation rejects it as a dangerous
                // global (see the `ExprKind::This` arm in the interpreter).
                "this" => {}
                name if is_dangerous_name(name) => {
                    return Err(JsError::DangerousGlobal {
                        pos,
                        name: name.to_owned(),
                    });
                }
                name if is_reserved_name(name) => {
                    return Err(JsError::UnsupportedSyntax {
                        pos,
                        feature: name.to_owned(),
                    });
                }
                _ => {}
            }
        }
        if self.at_op(Op::LBrace) {
            return self.parse_block();
        }
        if self.at_op(Op::Semicolon) {
            self.advance();
            return Ok(Stmt {
                kind: StmtKind::Empty,
                pos,
            });
        }
        if let Token::Unsupported(feature) = self.peek_token().clone() {
            self.advance();
            return Err(JsError::UnsupportedSyntax { pos, feature });
        }
        let expr = self.parse_expr(0)?;
        self.expect_op(Op::Semicolon, ";")?;
        Ok(Stmt {
            kind: StmtKind::Expr(expr),
            pos,
        })
    }

    fn parse_var_decl(&mut self, pos: Pos, kind: VarKind) -> Result<Stmt, JsError> {
        self.advance();
        let name = self.parse_decl_name()?;
        let init = if self.at_op(Op::Assign) {
            self.advance();
            Some(self.parse_expr(0)?)
        } else {
            None
        };
        self.expect_op(Op::Semicolon, ";")?;
        Ok(Stmt {
            kind: StmtKind::VarDecl { kind, name, init },
            pos,
        })
    }

    fn parse_decl_name(&mut self) -> Result<String, JsError> {
        let pos = self.pos();
        let Token::Ident(name) = self.peek_token().clone() else {
            return Err(JsError::Parse {
                pos,
                message: "expected an identifier".to_owned(),
            });
        };
        if !is_valid_identifier(&name) {
            return Err(JsError::Parse {
                pos,
                message: format!("'{name}' cannot be used as a declaration name"),
            });
        }
        if is_dangerous_name(&name) {
            return Err(JsError::DangerousGlobal { pos, name });
        }
        if is_reserved_name(&name) {
            return Err(JsError::UnsupportedSyntax { pos, feature: name });
        }
        self.advance();
        Ok(name)
    }

    fn parse_block(&mut self) -> Result<Stmt, JsError> {
        let pos = self.pos();
        self.expect_op(Op::LBrace, "{")?;
        let mut body = Vec::new();
        while !self.at_op(Op::RBrace) {
            if matches!(self.peek_token(), Token::Eof) {
                return Err(JsError::Parse {
                    pos: self.pos(),
                    message: "unterminated block; expected '}'".to_owned(),
                });
            }
            body.push(self.parse_stmt()?);
        }
        self.advance();
        Ok(Stmt {
            kind: StmtKind::Block(body),
            pos,
        })
    }

    fn parse_if(&mut self, pos: Pos) -> Result<Stmt, JsError> {
        self.advance();
        self.expect_op(Op::LParen, "(")?;
        let cond = self.parse_expr(0)?;
        self.expect_op(Op::RParen, ")")?;
        let then_branch = Box::new(self.parse_stmt()?);
        let else_branch = if self.at_ident("else") {
            self.advance();
            Some(Box::new(self.parse_stmt()?))
        } else {
            None
        };
        Ok(Stmt {
            kind: StmtKind::If {
                cond,
                then_branch,
                else_branch,
            },
            pos,
        })
    }

    fn parse_while(&mut self, pos: Pos) -> Result<Stmt, JsError> {
        self.advance();
        self.expect_op(Op::LParen, "(")?;
        let cond = self.parse_expr(0)?;
        self.expect_op(Op::RParen, ")")?;
        self.loop_depth += 1;
        let body = self.parse_stmt();
        self.loop_depth -= 1;
        Ok(Stmt {
            kind: StmtKind::While {
                cond,
                body: Box::new(body?),
            },
            pos,
        })
    }

    fn parse_for(&mut self, pos: Pos) -> Result<Stmt, JsError> {
        self.advance();
        self.expect_op(Op::LParen, "(")?;
        let init = if self.at_op(Op::Semicolon) {
            None
        } else if self.at_ident("let") || self.at_ident("var") || self.at_ident("const") {
            let decl_pos = self.pos();
            let kind = match self.peek_token().clone() {
                Token::Ident(name) if name == "let" => VarKind::Let,
                Token::Ident(name) if name == "var" => VarKind::Var,
                _ => VarKind::Const,
            };
            Some(Box::new(self.parse_var_decl(decl_pos, kind)?))
        } else {
            let expr = self.parse_expr(0)?;
            self.expect_op(Op::Semicolon, ";")?;
            Some(Box::new(Stmt {
                kind: StmtKind::Expr(expr),
                pos: self.pos(),
            }))
        };
        let cond = if self.at_op(Op::Semicolon) {
            None
        } else {
            Some(self.parse_expr(0)?)
        };
        self.expect_op(Op::Semicolon, ";")?;
        let update = if self.at_op(Op::RParen) {
            None
        } else {
            Some(self.parse_expr(0)?)
        };
        self.expect_op(Op::RParen, ")")?;
        self.loop_depth += 1;
        let body = self.parse_stmt();
        self.loop_depth -= 1;
        Ok(Stmt {
            kind: StmtKind::For {
                init,
                cond,
                update,
                body: Box::new(body?),
            },
            pos,
        })
    }

    fn parse_function_decl(&mut self, pos: Pos) -> Result<Stmt, JsError> {
        self.advance();
        let name = self.parse_decl_name()?;
        let params = self.parse_params()?;
        let saved_loops = self.loop_depth;
        self.loop_depth = 0;
        self.function_depth += 1;
        let body = self.parse_block();
        self.function_depth -= 1;
        self.loop_depth = saved_loops;
        let block = body?;
        let StmtKind::Block(statements) = block.kind else {
            return Err(JsError::Internal {
                pos,
                message: "function body was not a block".to_owned(),
            });
        };
        Ok(Stmt {
            kind: StmtKind::FunctionDecl {
                name,
                params,
                body: statements,
            },
            pos,
        })
    }

    fn parse_params(&mut self) -> Result<Vec<String>, JsError> {
        self.expect_op(Op::LParen, "(")?;
        let mut params = Vec::new();
        while !self.at_op(Op::RParen) {
            if !params.is_empty() {
                self.expect_op(Op::Comma, ",")?;
            }
            params.push(self.parse_decl_name()?);
        }
        self.expect_op(Op::RParen, ")")?;
        Ok(params)
    }

    fn parse_return(&mut self, pos: Pos) -> Result<Stmt, JsError> {
        self.advance();
        let value = if self.at_op(Op::Semicolon) {
            None
        } else {
            Some(self.parse_expr(0)?)
        };
        self.expect_op(Op::Semicolon, ";")?;
        if self.function_depth == 0 {
            return Err(JsError::Parse {
                pos,
                message: "'return' is only valid inside a function".to_owned(),
            });
        }
        Ok(Stmt {
            kind: StmtKind::Return(value),
            pos,
        })
    }

    fn parse_expr(&mut self, min_prec: u8) -> Result<Expr, JsError> {
        self.enter()?;
        let result = self.parse_expr_inner(min_prec);
        self.leave();
        result
    }

    fn parse_expr_inner(&mut self, min_prec: u8) -> Result<Expr, JsError> {
        let mut left = self.parse_unary()?;
        loop {
            if let Token::Unsupported(feature) = self.peek_token().clone() {
                let unsupported_pos = self.advance().pos;
                return Err(JsError::UnsupportedSyntax {
                    pos: unsupported_pos,
                    feature,
                });
            }
            if let Some(op) = self.peek_op().and_then(binary_op) {
                let prec = binary_precedence(op);
                if prec < min_prec {
                    break;
                }
                let op_pos = self.advance().pos;
                let right = self.parse_expr(prec + 1)?;
                left = Expr {
                    pos: op_pos,
                    kind: ExprKind::Binary {
                        op,
                        left: Box::new(left),
                        right: Box::new(right),
                    },
                };
                continue;
            }
            if min_prec == 0
                && let Some(op) = self.peek_op().and_then(assign_op)
            {
                if !matches!(
                    left.kind,
                    ExprKind::Ident(_) | ExprKind::Member { .. } | ExprKind::Index { .. }
                ) {
                    return Err(JsError::Parse {
                        pos: left.pos,
                        message: "invalid assignment target".to_owned(),
                    });
                }
                let op_pos = self.advance().pos;
                let value = self.parse_expr(0)?;
                left = Expr {
                    pos: op_pos,
                    kind: ExprKind::Assign {
                        op,
                        target: Box::new(left),
                        value: Box::new(value),
                    },
                };
                continue;
            }
            break;
        }
        Ok(left)
    }

    /// Parses a unary expression, charging every recursive unary step to the
    /// shared depth budget.
    ///
    /// Unary operators are right-recursive (`!!-x` nests three operands). Without
    /// the `enter()` guard, a hostile input such as 50,000 `!` characters would
    /// recurse 50,000 native frames and overflow the stack before any step or
    /// depth limit could apply. With the guard the parser unwinds with a typed
    /// [`JsError::AstTooDeep`] at the configured limit.
    fn parse_unary(&mut self) -> Result<Expr, JsError> {
        self.enter()?;
        let result = self.parse_unary_inner();
        self.leave();
        result
    }

    fn parse_unary_inner(&mut self) -> Result<Expr, JsError> {
        let pos = self.pos();
        let op = match self.peek_op() {
            Some(Op::Bang) => Some(UnaryOp::Not),
            Some(Op::Minus) => Some(UnaryOp::Neg),
            Some(Op::Plus) => Some(UnaryOp::Pos),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let operand = self.parse_unary()?;
            return Ok(Expr {
                pos,
                kind: ExprKind::Unary {
                    op,
                    operand: Box::new(operand),
                },
            });
        }
        self.parse_postfix()
    }

    /// Parses a postfix chain (`.`, `[]`, and `()`).
    ///
    /// Unlike unary operators these links are consumed by a loop, so the native
    /// call stack is not directly at risk. The resulting AST is nevertheless a
    /// deeply nested chain of `Box` nodes; 50,000 links would overflow the stack
    /// when the tree is later dropped or evaluated. Each link is therefore
    /// charged to the same depth budget so a hostile chain fails with a typed
    /// [`JsError::AstTooDeep`] during parsing.
    fn parse_postfix(&mut self) -> Result<Expr, JsError> {
        let mut expr = self.parse_primary()?;
        let start = expr.pos;
        // `enter()` must stay held for the whole chain: balancing it after each
        // link would keep `self.depth` constant and let a 50k-link chain through.
        let mut links = 0usize;
        loop {
            let is_link = matches!(
                self.peek_op(),
                Some(Op::Dot) | Some(Op::LBracket) | Some(Op::LParen)
            );
            if !is_link {
                break;
            }
            if let Err(error) = self.enter() {
                Self::unwind(&mut self.depth, links);
                return Err(error);
            }
            links += 1;
            expr = match self.parse_postfix_link(start, expr) {
                Ok(next) => next,
                Err(error) => {
                    Self::unwind(&mut self.depth, links);
                    return Err(error);
                }
            };
        }
        Self::unwind(&mut self.depth, links);
        Ok(expr)
    }

    /// Releases `count` depth levels held by a postfix chain.
    fn unwind(depth: &mut usize, count: usize) {
        for _ in 0..count {
            *depth -= 1;
        }
    }

    /// Parses exactly one postfix link. Called only by [`Parser::parse_postfix`],
    /// which owns the depth accounting and has already confirmed a link follows.
    fn parse_postfix_link(&mut self, start: Pos, expr: Expr) -> Result<Expr, JsError> {
        if self.at_op(Op::Dot) {
            self.advance();
            let property = self.parse_property_name()?;
            Ok(Expr {
                pos: start,
                kind: ExprKind::Member {
                    object: Box::new(expr),
                    property,
                },
            })
        } else if self.at_op(Op::LBracket) {
            self.advance();
            let index = self.parse_expr(0)?;
            self.expect_op(Op::RBracket, "]")?;
            Ok(Expr {
                pos: start,
                kind: ExprKind::Index {
                    object: Box::new(expr),
                    index: Box::new(index),
                },
            })
        } else if self.at_op(Op::LParen) {
            self.advance();
            let args = self.parse_args()?;
            Ok(Expr {
                pos: start,
                kind: ExprKind::Call {
                    callee: Box::new(expr),
                    args,
                },
            })
        } else {
            Err(JsError::Internal {
                pos: start,
                message: "postfix link was not a chain operator".to_owned(),
            })
        }
    }

    fn parse_property_name(&mut self) -> Result<String, JsError> {
        let pos = self.pos();
        match self.peek_token().clone() {
            Token::Ident(name) | Token::Str(name) => {
                self.advance();
                Ok(name)
            }
            Token::Op(Op::LBracket) => Err(JsError::UnsupportedSyntax {
                pos,
                feature: "computed property after '.'".to_owned(),
            }),
            _ => Err(JsError::Parse {
                pos,
                message: "expected a property name after '.'".to_owned(),
            }),
        }
    }

    fn parse_args(&mut self) -> Result<Vec<Expr>, JsError> {
        let mut args = Vec::new();
        while !self.at_op(Op::RParen) {
            if !args.is_empty() {
                self.expect_op(Op::Comma, ",")?;
            }
            args.push(self.parse_expr(0)?);
        }
        self.expect_op(Op::RParen, ")")?;
        Ok(args)
    }

    fn parse_primary(&mut self) -> Result<Expr, JsError> {
        self.enter()?;
        let result = self.parse_primary_inner();
        self.leave();
        result
    }

    fn parse_primary_inner(&mut self) -> Result<Expr, JsError> {
        let pos = self.pos();
        match self.peek_token().clone() {
            Token::Number(value) => {
                self.advance();
                Ok(Expr {
                    pos,
                    kind: ExprKind::Number(value),
                })
            }
            Token::Str(value) => {
                self.advance();
                Ok(Expr {
                    pos,
                    kind: ExprKind::Str(value),
                })
            }
            Token::Ident(name) => {
                self.advance();
                match name.as_str() {
                    "true" => Ok(Expr {
                        pos,
                        kind: ExprKind::Bool(true),
                    }),
                    "false" => Ok(Expr {
                        pos,
                        kind: ExprKind::Bool(false),
                    }),
                    "null" => Ok(Expr {
                        pos,
                        kind: ExprKind::Null,
                    }),
                    "this" => Ok(Expr {
                        pos,
                        kind: ExprKind::This,
                    }),
                    _ if is_dangerous_name(&name) => Err(JsError::DangerousGlobal { pos, name }),
                    _ if is_reserved_name(&name) => {
                        Err(JsError::UnsupportedSyntax { pos, feature: name })
                    }
                    _ => Ok(Expr {
                        pos,
                        kind: ExprKind::Ident(name),
                    }),
                }
            }
            Token::Op(Op::LParen) => {
                self.advance();
                let expr = self.parse_expr(0)?;
                self.expect_op(Op::RParen, ")")?;
                Ok(expr)
            }
            Token::Op(Op::LBracket) => {
                self.advance();
                let mut items = Vec::new();
                while !self.at_op(Op::RBracket) {
                    if !items.is_empty() {
                        self.expect_op(Op::Comma, ",")?;
                    }
                    if self.at_op(Op::RBracket) {
                        break;
                    }
                    items.push(self.parse_expr(0)?);
                }
                self.expect_op(Op::RBracket, "]")?;
                Ok(Expr {
                    pos,
                    kind: ExprKind::Array(items),
                })
            }
            Token::Op(Op::LBrace) => {
                self.advance();
                let mut entries = Vec::new();
                while !self.at_op(Op::RBrace) {
                    if !entries.is_empty() {
                        self.expect_op(Op::Comma, ",")?;
                    }
                    if self.at_op(Op::RBrace) {
                        break;
                    }
                    let key = match self.peek_token().clone() {
                        Token::Ident(name) => {
                            self.advance();
                            name
                        }
                        Token::Str(name) => {
                            self.advance();
                            name
                        }
                        _ => {
                            return Err(JsError::Parse {
                                pos: self.pos(),
                                message: "expected an object key".to_owned(),
                            });
                        }
                    };
                    self.expect_op(Op::Colon, ":")?;
                    let value = self.parse_expr(0)?;
                    entries.push((key, value));
                }
                self.expect_op(Op::RBrace, "}")?;
                Ok(Expr {
                    pos,
                    kind: ExprKind::Object(entries),
                })
            }
            Token::Unsupported(feature) => {
                self.advance();
                Err(JsError::UnsupportedSyntax { pos, feature })
            }
            Token::Eof => Err(JsError::Parse {
                pos,
                message: "unexpected end of input".to_owned(),
            }),
            other => Err(JsError::Parse {
                pos,
                message: format!("unexpected token {other:?}"),
            }),
        }
    }

    fn peek_op(&self) -> Option<Op> {
        match self.peek_token() {
            Token::Op(op) => Some(*op),
            _ => None,
        }
    }
}

fn binary_op(op: Op) -> Option<BinaryOp> {
    match op {
        Op::Plus => Some(BinaryOp::Add),
        Op::Minus => Some(BinaryOp::Sub),
        Op::Star => Some(BinaryOp::Mul),
        Op::Slash => Some(BinaryOp::Div),
        Op::Percent => Some(BinaryOp::Mod),
        Op::Eq => Some(BinaryOp::Eq),
        Op::Ne => Some(BinaryOp::Ne),
        Op::Lt => Some(BinaryOp::Lt),
        Op::Le => Some(BinaryOp::Le),
        Op::Gt => Some(BinaryOp::Gt),
        Op::Ge => Some(BinaryOp::Ge),
        Op::AndAnd => Some(BinaryOp::And),
        Op::OrOr => Some(BinaryOp::Or),
        _ => None,
    }
}

fn assign_op(op: Op) -> Option<AssignOp> {
    match op {
        Op::Assign => Some(AssignOp::Assign),
        Op::PlusAssign => Some(AssignOp::Add),
        Op::MinusAssign => Some(AssignOp::Sub),
        Op::StarAssign => Some(AssignOp::Mul),
        Op::SlashAssign => Some(AssignOp::Div),
        Op::PercentAssign => Some(AssignOp::Mod),
        _ => None,
    }
}

fn binary_precedence(op: BinaryOp) -> u8 {
    match op {
        BinaryOp::Or => 1,
        BinaryOp::And => 2,
        BinaryOp::Eq | BinaryOp::Ne => 3,
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => 4,
        BinaryOp::Add | BinaryOp::Sub => 5,
        BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => 6,
    }
}
