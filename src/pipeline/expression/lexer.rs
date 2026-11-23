use super::ast::{Token, Value};
use super::{expression_error, MAX_EXPRESSION_LENGTH};
use crate::error::{ErrorCode, Result, UbuilderError};

const MAX_TOKENS: usize = 1024;

pub fn tokenize(source: &str) -> Result<Vec<Token>> {
    if source.len() > MAX_EXPRESSION_LENGTH {
        return Err(expression_error("expression is too long"));
    }
    let mut tokens = Vec::new();
    let chars: Vec<char> = source.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_whitespace() {
            index += 1;
            continue;
        }
        if tokens.len() >= MAX_TOKENS {
            return Err(expression_error("expression has too many tokens"));
        }
        match ch {
            '(' => {
                tokens.push(Token::LParen);
                index += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                index += 1;
            }
            ',' => {
                tokens.push(Token::Comma);
                index += 1;
            }
            '.' => {
                tokens.push(Token::Dot);
                index += 1;
            }
            '!' => {
                if chars.get(index + 1) == Some(&'=') {
                    tokens.push(Token::NotEq);
                    index += 2;
                } else {
                    tokens.push(Token::Not);
                    index += 1;
                }
            }
            '=' => {
                if chars.get(index + 1) == Some(&'=') {
                    tokens.push(Token::Eq);
                    index += 2;
                } else {
                    return Err(expression_error("expected '==' but found '='"));
                }
            }
            '<' => {
                if chars.get(index + 1) == Some(&'=') {
                    tokens.push(Token::Lte);
                    index += 2;
                } else {
                    tokens.push(Token::Lt);
                    index += 1;
                }
            }
            '>' => {
                if chars.get(index + 1) == Some(&'=') {
                    tokens.push(Token::Gte);
                    index += 2;
                } else {
                    tokens.push(Token::Gt);
                    index += 1;
                }
            }
            '&' => {
                if chars.get(index + 1) == Some(&'&') {
                    tokens.push(Token::And);
                    index += 2;
                } else {
                    return Err(expression_error("expected '&&' but found '&'"));
                }
            }
            '|' => {
                if chars.get(index + 1) == Some(&'|') {
                    tokens.push(Token::Or);
                    index += 2;
                } else {
                    return Err(expression_error("expected '||' but found '|'"));
                }
            }
            '\'' | '"' => {
                let quote = ch;
                index += 1;
                let mut text = String::new();
                let mut closed = false;
                while index < chars.len() {
                    let current = chars[index];
                    if current == '\\' {
                        let Some(next) = chars.get(index + 1) else {
                            break;
                        };
                        text.push(match next {
                            'n' => '\n',
                            'r' => '\r',
                            't' => '\t',
                            other => *other,
                        });
                        index += 2;
                    } else if current == quote {
                        closed = true;
                        index += 1;
                        break;
                    } else {
                        text.push(current);
                        index += 1;
                    }
                }
                if !closed {
                    return Err(expression_error("unterminated string literal"));
                }
                tokens.push(Token::Literal(Value::String(text)));
            }
            ch if ch.is_ascii_digit() => {
                let start = index;
                while index < chars.len() && (chars[index].is_ascii_digit() || chars[index] == '.')
                {
                    index += 1;
                }
                let raw: String = chars[start..index].iter().collect();
                let number: f64 = raw
                    .parse()
                    .map_err(|_| expression_error(format!("invalid number '{raw}'")))?;
                let value = serde_json::Number::from_f64(number)
                    .ok_or_else(|| expression_error(format!("invalid number '{raw}'")))?;
                tokens.push(Token::Literal(Value::Number(value)));
            }
            ch if ch.is_ascii_alphabetic() || ch == '_' => {
                let start = index;
                while index < chars.len()
                    && (chars[index].is_ascii_alphanumeric()
                        || chars[index] == '_'
                        || chars[index] == '-')
                {
                    index += 1;
                }
                let word: String = chars[start..index].iter().collect();
                match word.as_str() {
                    "true" => tokens.push(Token::Literal(Value::Bool(true))),
                    "false" => tokens.push(Token::Literal(Value::Bool(false))),
                    "null" => tokens.push(Token::Literal(Value::Null)),
                    _ => tokens.push(Token::Identifier(word)),
                }
            }
            other => {
                return Err(UbuilderError::new(
                    ErrorCode::ConfigInvalid,
                    format!("unexpected character '{other}' in expression"),
                ))
            }
        }
    }
    Ok(tokens)
}
