use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Token {
    Literal(Value),
    Identifier(String),
    Dot,
    Comma,
    LParen,
    RParen,
    Eq,
    NotEq,
    Lt,
    Lte,
    Gt,
    Gte,
    And,
    Or,
    Not,
}

pub type Value = serde_json::Value;

impl Token {
    pub fn describe(&self) -> String {
        match self {
            Token::Literal(value) => format!("literal '{value}'"),
            Token::Identifier(name) => format!("identifier '{name}'"),
            other => format!("'{other}'"),
        }
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Token::Literal(_) => "literal",
            Token::Identifier(_) => "identifier",
            Token::Dot => ".",
            Token::Comma => ",",
            Token::LParen => "(",
            Token::RParen => ")",
            Token::Eq => "==",
            Token::NotEq => "!=",
            Token::Lt => "<",
            Token::Lte => "<=",
            Token::Gt => ">",
            Token::Gte => ">=",
            Token::And => "&&",
            Token::Or => "||",
            Token::Not => "!",
        };
        f.write_str(text)
    }
}
