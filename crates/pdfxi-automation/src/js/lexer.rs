//! Deterministic tokenizer for the JavaScript sandbox subset.
//!
//! The lexer is intentionally small: it recognizes numbers, quoted strings,
//! identifiers, and a fixed set of operators. Everything else is either a
//! [`Token::Unsupported`] marker (a construct the subset deliberately omits) or
//! a [`JsError::Lex`] for a character that cannot start any token. Keywords are
//! represented as identifiers and interpreted by the parser; this keeps the
//! dangerous-name check in a single place.

use crate::js::error::{JsError, Pos};

/// A lexical token with no attached source position.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Token {
    /// A numeric literal.
    Number(f64),
    /// A string literal (quotes removed, escapes decoded).
    Str(String),
    /// An identifier or keyword.
    Ident(String),
    /// An operator or punctuation mark supported by the subset.
    Op(Op),
    /// A construct that is recognized but intentionally unsupported.
    Unsupported(String),
    /// End of input.
    Eof,
}

/// Operators and punctuation supported by the subset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Assign,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Bang,
    AndAnd,
    OrOr,
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,
    PercentAssign,
    Dot,
    Comma,
    Semicolon,
    Colon,
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
}

/// A token paired with its source position.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Spanned {
    pub(crate) token: Token,
    pub(crate) pos: Pos,
}

/// Tokenizes `source` into a position-annotated vector terminated by
/// [`Token::Eof`].
///
/// The function is deterministic and performs no I/O. It never panics: every
/// malformed input produces a [`JsError::Lex`] with the exact character
/// position.
pub(crate) fn lex(source: &str) -> Result<Vec<Spanned>, JsError> {
    let chars: Vec<char> = source.chars().collect();
    let mut lexer = Lexer {
        chars: &chars,
        index: 0,
        line: 1,
        column: 1,
        tokens: Vec::new(),
    };
    lexer.run()?;
    Ok(lexer.tokens)
}

struct Lexer<'a> {
    chars: &'a [char],
    index: usize,
    line: usize,
    column: usize,
    tokens: Vec<Spanned>,
}

impl<'a> Lexer<'a> {
    fn pos(&self) -> Pos {
        Pos::new(self.line, self.column, self.index)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }

    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.index + ahead).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.index += 1;
        if ch == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(ch)
    }

    fn push(&mut self, token: Token, pos: Pos) {
        self.tokens.push(Spanned { token, pos });
    }

    fn run(&mut self) -> Result<(), JsError> {
        loop {
            self.skip_trivia()?;
            let pos = self.pos();
            let Some(ch) = self.peek() else {
                self.push(Token::Eof, pos);
                return Ok(());
            };
            if ch.is_ascii_digit() {
                self.lex_number(pos)?;
            } else if is_ident_start(ch) {
                self.lex_identifier(pos);
            } else if ch == '\'' || ch == '"' {
                self.lex_string(pos)?;
            } else {
                self.lex_operator(pos)?;
            }
        }
    }

    fn skip_trivia(&mut self) -> Result<(), JsError> {
        loop {
            match self.peek() {
                Some(' ') | Some('\t') | Some('\r') | Some('\n') => {
                    self.bump();
                }
                Some('/') if self.peek_at(1) == Some('/') => {
                    self.bump();
                    self.bump();
                    while let Some(ch) = self.peek() {
                        if ch == '\n' {
                            break;
                        }
                        self.bump();
                    }
                }
                Some('/') if self.peek_at(1) == Some('*') => {
                    let start = self.pos();
                    self.bump();
                    self.bump();
                    let mut closed = false;
                    while self.peek().is_some() {
                        if self.peek() == Some('*') && self.peek_at(1) == Some('/') {
                            self.bump();
                            self.bump();
                            closed = true;
                            break;
                        }
                        self.bump();
                    }
                    if !closed {
                        return Err(JsError::Lex {
                            pos: start,
                            message: "unterminated block comment".to_owned(),
                        });
                    }
                }
                _ => return Ok(()),
            }
        }
    }

    fn lex_number(&mut self, pos: Pos) -> Result<(), JsError> {
        let mut text = String::new();
        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() {
                text.push(ch);
                self.bump();
            } else {
                break;
            }
        }
        if self.peek() == Some('.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            text.push('.');
            self.bump();
            while let Some(ch) = self.peek() {
                if ch.is_ascii_digit() {
                    text.push(ch);
                    self.bump();
                } else {
                    break;
                }
            }
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            let mut exponent = String::new();
            exponent.push(self.bump().unwrap_or('e'));
            if matches!(self.peek(), Some('+') | Some('-')) {
                exponent.push(self.bump().unwrap_or('+'));
            }
            let mut has_digit = false;
            while let Some(ch) = self.peek() {
                if ch.is_ascii_digit() {
                    exponent.push(ch);
                    has_digit = true;
                    self.bump();
                } else {
                    break;
                }
            }
            if has_digit {
                text.push_str(&exponent);
            } else {
                // Roll back the exponent characters so they can be lexed as an
                // identifier (`1efoo` becomes number 1 followed by ident foo).
                for _ in 0..exponent.len() {
                    self.index = self.index.saturating_sub(1);
                    self.column = self.column.saturating_sub(1);
                }
            }
        }
        let number = text.parse::<f64>().map_err(|_| JsError::Lex {
            pos,
            message: format!("invalid numeric literal '{text}'"),
        })?;
        self.push(Token::Number(number), pos);
        Ok(())
    }

    fn lex_identifier(&mut self, pos: Pos) {
        let mut name = String::new();
        while let Some(ch) = self.peek() {
            if is_ident_continue(ch) {
                name.push(ch);
                self.bump();
            } else {
                break;
            }
        }
        self.push(Token::Ident(name), pos);
    }

    fn lex_string(&mut self, pos: Pos) -> Result<(), JsError> {
        let quote = self.bump().unwrap_or('\'');
        let mut value = String::new();
        loop {
            let Some(ch) = self.bump() else {
                return Err(JsError::Lex {
                    pos,
                    message: "unterminated string literal".to_owned(),
                });
            };
            if ch == quote {
                break;
            }
            if ch == '\n' {
                return Err(JsError::Lex {
                    pos,
                    message: "newline inside string literal".to_owned(),
                });
            }
            if ch == '\\' {
                let escaped = self.bump().ok_or_else(|| JsError::Lex {
                    pos,
                    message: "unterminated escape sequence".to_owned(),
                })?;
                match escaped {
                    'n' => value.push('\n'),
                    'r' => value.push('\r'),
                    't' => value.push('\t'),
                    '0' => value.push('\0'),
                    '\\' => value.push('\\'),
                    '\'' => value.push('\''),
                    '"' => value.push('"'),
                    '/' => value.push('/'),
                    other => {
                        return Err(JsError::Lex {
                            pos,
                            message: format!("unsupported escape '\\{other}'"),
                        });
                    }
                }
            } else {
                value.push(ch);
            }
        }
        self.push(Token::Str(value), pos);
        Ok(())
    }

    fn lex_operator(&mut self, pos: Pos) -> Result<(), JsError> {
        // Longest unsupported sequences are recognized first so they produce a
        // typed "unsupported syntax" error instead of a confusing parse error.
        let three: String = (0..3).filter_map(|i| self.peek_at(i)).collect();
        if matches!(
            three.as_str(),
            "===" | "!==" | "**=" | "<<=" | ">>=" | "..."
        ) {
            return self.unsupported(&three, pos);
        }
        let two: String = (0..2).filter_map(|i| self.peek_at(i)).collect();
        if matches!(
            two.as_str(),
            "==" | "!="
                | "<="
                | ">="
                | "&&"
                | "||"
                | "+="
                | "-="
                | "*="
                | "/="
                | "%="
                | "**"
                | "??"
                | "?."
                | "=>"
                | "<<"
                | ">>"
                | "&&="
                | "||="
        ) {
            if matches!(two.as_str(), "&&=" | "||=") {
                return self.unsupported(&two, pos);
            }
            let op = match two.as_str() {
                "==" => Op::Eq,
                "!=" => Op::Ne,
                "<=" => Op::Le,
                ">=" => Op::Ge,
                "&&" => Op::AndAnd,
                "||" => Op::OrOr,
                "+=" => Op::PlusAssign,
                "-=" => Op::MinusAssign,
                "*=" => Op::StarAssign,
                "/=" => Op::SlashAssign,
                "%=" => Op::PercentAssign,
                other => return self.unsupported(other, pos),
            };
            self.bump();
            self.bump();
            self.push(Token::Op(op), pos);
            return Ok(());
        }
        let op = match self.peek().unwrap_or('\0') {
            '=' => Op::Assign,
            '<' => Op::Lt,
            '>' => Op::Gt,
            '+' => Op::Plus,
            '-' => Op::Minus,
            '*' => Op::Star,
            '/' => Op::Slash,
            '%' => Op::Percent,
            '!' => Op::Bang,
            '.' => Op::Dot,
            ',' => Op::Comma,
            ';' => Op::Semicolon,
            ':' => Op::Colon,
            '(' => Op::LParen,
            ')' => Op::RParen,
            '{' => Op::LBrace,
            '}' => Op::RBrace,
            '[' => Op::LBracket,
            ']' => Op::RBracket,
            '`' => return self.unsupported("template literal", pos),
            '?' => return self.unsupported("'?' conditional", pos),
            '#' => return self.unsupported("'#' private name", pos),
            other => {
                return Err(JsError::Lex {
                    pos,
                    message: format!("unexpected character '{other}'"),
                });
            }
        };
        self.bump();
        self.push(Token::Op(op), pos);
        Ok(())
    }

    fn unsupported(&mut self, feature: &str, pos: Pos) -> Result<(), JsError> {
        // Consume the whole unsupported marker so later tokens keep their
        // positions and a follow-up parse does not cascade from the same spot.
        let unknown = Token::Unsupported(feature.to_owned());
        self.push(unknown, pos);
        for _ in 0..feature.chars().count().min(3) {
            self.bump();
        }
        Ok(())
    }
}

fn is_ident_start(ch: char) -> bool {
    ch.is_ascii_alphabetic() || ch == '_' || ch == '$'
}

fn is_ident_continue(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '$'
}
