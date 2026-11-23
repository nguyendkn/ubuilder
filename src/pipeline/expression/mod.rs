//! Sandboxed expression language: lexer, parser, AST and evaluator.
//!
//! No shell, `eval`, JavaScript or dynamic code. Limits are enforced on source
//! length, token count, AST depth and evaluation steps.

pub mod ast;
pub mod context;
pub mod evaluator;
pub mod lexer;
pub mod parser;

pub use ast::*;
pub use context::*;
pub use evaluator::*;
pub use lexer::*;
pub use parser::*;

use crate::error::{ErrorCode, Result, UbuilderError};
use serde::{Deserialize, Serialize};

pub const MAX_EXPRESSION_LENGTH: usize = 4096;

/// A compiled expression. The AST is rebuilt deterministically on evaluation;
/// the source is serialized so the IR stays debuggable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Expression {
    pub source: String,
}

impl Expression {
    pub fn parse(source: impl Into<String>) -> Result<Self> {
        let source = source.into();
        if source.len() > MAX_EXPRESSION_LENGTH {
            return Err(expression_error(format!(
                "expression exceeds {MAX_EXPRESSION_LENGTH} bytes"
            )));
        }
        parser::parse(&source)?;
        Ok(Self { source })
    }

    /// Accept both `${{ ... }}` and a bare expression for `if` fields.
    pub fn parse_condition(source: impl Into<String>) -> Result<Self> {
        let source = source.into();
        let inner = source.trim();
        if let Some(stripped) = inner
            .strip_prefix("${{")
            .and_then(|rest| rest.strip_suffix("}}"))
        {
            Self::parse(stripped.trim())
        } else {
            Self::parse(inner)
        }
    }

    pub fn evaluate(&self, context: &Context) -> Result<Value> {
        let ast = parser::parse(&self.source)?;
        evaluator::evaluate(&ast, context)
    }

    pub fn evaluate_bool(&self, context: &Context) -> Result<bool> {
        Ok(self.evaluate(context)?.truthy())
    }
}

/// A string with `${{ ... }}` interpolation segments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Template {
    pub source: String,
}

impl Template {
    pub fn parse(source: impl Into<String>) -> Result<Self> {
        let source = source.into();
        if source.len() > MAX_EXPRESSION_LENGTH * 4 {
            return Err(expression_error("interpolated string is too long"));
        }
        let segments = template_segments(&source)?;
        if segments.len() > 128 {
            return Err(expression_error("too many interpolation segments"));
        }
        for segment in segments {
            if let Segment::Expression(raw) = segment {
                parser::parse(raw)?;
            }
        }
        Ok(Self { source })
    }

    pub fn evaluate(&self, context: &Context) -> Result<String> {
        let mut output = String::new();
        for segment in template_segments(&self.source)? {
            match segment {
                Segment::Literal(text) => output.push_str(text),
                Segment::Expression(raw) => {
                    let ast = parser::parse(raw)?;
                    let value = evaluator::evaluate(&ast, context)?;
                    output.push_str(&value.as_template_string());
                }
            }
        }
        Ok(output)
    }
}

enum Segment<'a> {
    Literal(&'a str),
    Expression(&'a str),
}

fn template_segments(source: &str) -> Result<Vec<Segment<'_>>> {
    let mut segments = Vec::new();
    let mut rest = source;
    while let Some(start) = rest.find("${{") {
        if start > 0 {
            segments.push(Segment::Literal(&rest[..start]));
        }
        let after = &rest[start + 3..];
        let Some(end) = after.find("}}") else {
            return Err(expression_error("unterminated '${{' interpolation"));
        };
        let raw = after[..end].trim();
        if raw.is_empty() {
            return Err(expression_error("empty '${{ }}' interpolation"));
        }
        segments.push(Segment::Expression(raw));
        rest = &after[end + 2..];
    }
    if !rest.is_empty() {
        segments.push(Segment::Literal(rest));
    }
    if segments.is_empty() {
        segments.push(Segment::Literal(""));
    }
    Ok(segments)
}

pub fn expression_error(message: impl Into<String>) -> UbuilderError {
    UbuilderError::new(ErrorCode::ConfigInvalid, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_evaluates_conditions() {
        let expression =
            Expression::parse_condition("${{ matrix.target == 'linux-x64' }}").unwrap();
        let context = Context::new().with("matrix", serde_json::json!({ "target": "linux-x64" }));
        assert!(expression.evaluate_bool(&context).unwrap());
    }

    #[test]
    fn interpolates_templates() {
        let template = Template::parse("desktop-${{ matrix.target }}").unwrap();
        let context = Context::new().with("matrix", serde_json::json!({ "target": "macos-arm64" }));
        assert_eq!(template.evaluate(&context).unwrap(), "desktop-macos-arm64");
    }

    #[test]
    fn rejects_unterminated_interpolation() {
        assert!(Template::parse("${{ matrix.target").is_err());
    }

    #[test]
    fn rejects_oversized_expressions_and_templates() {
        let oversized = "true".repeat(MAX_EXPRESSION_LENGTH);
        assert!(Expression::parse(oversized).is_err());

        let mut template = String::new();
        for index in 0..129 {
            template.push_str(&format!("${{{{ matrix.axis{index} }}}}"));
        }
        assert!(Template::parse(template).is_err());
    }
}
