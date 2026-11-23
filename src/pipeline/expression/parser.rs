use super::ast::Token;
use super::lexer::tokenize;
use super::Value;
use crate::error::{ErrorCode, Result, UbuilderError};
use serde::{Deserialize, Serialize};

const MAX_DEPTH: usize = 64;
const MAX_NODES: usize = 512;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Ast {
    Literal(Value),
    Path(Vec<String>),
    UnaryNot(Box<Ast>),
    Binary {
        op: BinaryOp,
        left: Box<Ast>,
        right: Box<Ast>,
    },
    Call {
        name: String,
        args: Vec<Ast>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BinaryOp {
    Eq,
    NotEq,
    Lt,
    Lte,
    Gt,
    Gte,
    And,
    Or,
}

struct Parser {
    tokens: Vec<Token>,
    index: usize,
    nodes: usize,
    depth: usize,
}

pub fn parse(source: &str) -> Result<Ast> {
    let tokens = tokenize(source)?;
    if tokens.is_empty() {
        return Err(parse_error("expression is empty"));
    }
    let mut parser = Parser {
        tokens,
        index: 0,
        nodes: 0,
        depth: 0,
    };
    let ast = parser.parse_or()?;
    if parser.index != parser.tokens.len() {
        return Err(parse_error(format!(
            "unexpected token {} after expression",
            parser.tokens[parser.index].describe()
        )));
    }
    Ok(ast)
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.index)
    }

    fn advance(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.index).cloned();
        self.index += 1;
        token
    }

    fn node(&mut self) -> Result<()> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err(parse_error("expression is too complex"));
        }
        Ok(())
    }

    fn enter(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(parse_error("expression nesting is too deep"));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn parse_or(&mut self) -> Result<Ast> {
        self.enter()?;
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.advance();
            let right = self.parse_and()?;
            self.node()?;
            left = Ast::Binary {
                op: BinaryOp::Or,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        self.leave();
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Ast> {
        let mut left = self.parse_equality()?;
        while matches!(self.peek(), Some(Token::And)) {
            self.advance();
            let right = self.parse_equality()?;
            self.node()?;
            left = Ast::Binary {
                op: BinaryOp::And,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_equality(&mut self) -> Result<Ast> {
        let mut left = self.parse_comparison()?;
        loop {
            let op = match self.peek() {
                Some(Token::Eq) => BinaryOp::Eq,
                Some(Token::NotEq) => BinaryOp::NotEq,
                _ => break,
            };
            self.advance();
            let right = self.parse_comparison()?;
            self.node()?;
            left = Ast::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Ast> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Some(Token::Lt) => BinaryOp::Lt,
                Some(Token::Lte) => BinaryOp::Lte,
                Some(Token::Gt) => BinaryOp::Gt,
                Some(Token::Gte) => BinaryOp::Gte,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary()?;
            self.node()?;
            left = Ast::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Ast> {
        self.enter()?;
        let result = if matches!(self.peek(), Some(Token::Not)) {
            self.advance();
            self.node()?;
            Ok(Ast::UnaryNot(Box::new(self.parse_unary()?)))
        } else {
            self.parse_primary()
        };
        self.leave();
        result
    }

    fn parse_primary(&mut self) -> Result<Ast> {
        let token = self
            .advance()
            .ok_or_else(|| parse_error("unexpected end of expression"))?;
        match token {
            Token::Literal(value) => {
                self.node()?;
                Ok(Ast::Literal(value))
            }
            Token::Identifier(name) => {
                self.node()?;
                if matches!(self.peek(), Some(Token::LParen)) {
                    self.advance();
                    let mut args = Vec::new();
                    if !matches!(self.peek(), Some(Token::RParen)) {
                        loop {
                            args.push(self.parse_or()?);
                            if matches!(self.peek(), Some(Token::Comma)) {
                                self.advance();
                                continue;
                            }
                            break;
                        }
                    }
                    match self.advance() {
                        Some(Token::RParen) => {}
                        _ => return Err(parse_error("expected ')' after function arguments")),
                    }
                    if args.len() > 8 {
                        return Err(parse_error("function has too many arguments"));
                    }
                    return Ok(Ast::Call { name, args });
                }
                let mut path = vec![name];
                while matches!(self.peek(), Some(Token::Dot)) {
                    self.advance();
                    match self.advance() {
                        Some(Token::Identifier(part)) => path.push(part),
                        _ => return Err(parse_error("expected a field name after '.'")),
                    }
                }
                if path.len() > 8 {
                    return Err(parse_error("context path is too deep"));
                }
                Ok(Ast::Path(path))
            }
            Token::LParen => {
                let inner = self.parse_or()?;
                match self.advance() {
                    Some(Token::RParen) => Ok(inner),
                    _ => Err(parse_error("expected ')'")),
                }
            }
            other => Err(parse_error(format!(
                "unexpected token {}",
                other.describe()
            ))),
        }
    }
}

fn parse_error(message: impl Into<String>) -> UbuilderError {
    UbuilderError::new(ErrorCode::ConfigInvalid, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_precedence() {
        let ast = parse("!success() || matrix.target == 'linux-x64' && x != null").unwrap();
        assert!(matches!(
            ast,
            Ast::Binary {
                op: BinaryOp::Or,
                ..
            }
        ));
    }

    #[test]
    fn rejects_incomplete_expressions() {
        assert!(parse("matrix.target ==").is_err());
        assert!(parse("(a == b").is_err());
        assert!(parse("a && && b").is_err());
    }

    #[test]
    fn rejects_excessive_depth() {
        let source = format!("{}true{}", "(".repeat(100), ")".repeat(100));
        assert!(parse(&source).is_err());
    }
}
