use super::ast::Value;
use super::context::{Context, ValueExt};
use super::parser::{Ast, BinaryOp};
use crate::error::{ErrorCode, Result, UbuilderError};

const MAX_EVAL_STEPS: usize = 10_000;

struct Evaluator<'a> {
    context: &'a Context,
    steps: usize,
}

pub fn evaluate(ast: &Ast, context: &Context) -> Result<Value> {
    let mut evaluator = Evaluator { context, steps: 0 };
    evaluator.eval(ast)
}

impl Evaluator<'_> {
    fn tick(&mut self) -> Result<()> {
        self.steps += 1;
        if self.steps > MAX_EVAL_STEPS {
            return Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                "expression evaluation exceeded its work limit",
            ));
        }
        Ok(())
    }

    fn eval(&mut self, ast: &Ast) -> Result<Value> {
        self.tick()?;
        match ast {
            Ast::Literal(value) => Ok(value.clone()),
            Ast::Path(path) => {
                let root = &path[0];
                let Some(mut current) = self.context.get_root(root) else {
                    if self.context.is_unavailable(root) {
                        return Err(UbuilderError::new(
                            ErrorCode::ConfigInvalid,
                            format!("context '{root}' is not available at this phase"),
                        ));
                    }
                    return Ok(Value::Null);
                };
                for part in &path[1..] {
                    current = match current {
                        Value::Object(map) => map.get(part).unwrap_or(&Value::Null),
                        Value::Array(items) => part
                            .parse::<usize>()
                            .ok()
                            .and_then(|index| items.get(index))
                            .unwrap_or(&Value::Null),
                        _ => &Value::Null,
                    };
                }
                Ok(current.clone())
            }
            Ast::UnaryNot(inner) => Ok(Value::Bool(!self.eval(inner)?.truthy())),
            Ast::Binary { op, left, right } => match op {
                BinaryOp::And => {
                    let left = self.eval(left)?;
                    if !left.truthy() {
                        return Ok(Value::Bool(false));
                    }
                    Ok(Value::Bool(self.eval(right)?.truthy()))
                }
                BinaryOp::Or => {
                    let left = self.eval(left)?;
                    if left.truthy() {
                        return Ok(Value::Bool(true));
                    }
                    Ok(Value::Bool(self.eval(right)?.truthy()))
                }
                BinaryOp::Eq => {
                    let left = self.eval(left)?;
                    let right = self.eval(right)?;
                    Ok(Value::Bool(equals(&left, &right)))
                }
                BinaryOp::NotEq => {
                    let left = self.eval(left)?;
                    let right = self.eval(right)?;
                    Ok(Value::Bool(!equals(&left, &right)))
                }
                BinaryOp::Lt | BinaryOp::Lte | BinaryOp::Gt | BinaryOp::Gte => {
                    let left = self.eval(left)?;
                    let right = self.eval(right)?;
                    Ok(Value::Bool(compare(*op, &left, &right)))
                }
            },
            Ast::Call { name, args } => self.call(name, args),
        }
    }

    fn call(&mut self, name: &str, args: &[Ast]) -> Result<Value> {
        match name {
            "success" => {
                expect_args(name, args, 0)?;
                Ok(Value::Bool(self.context.success))
            }
            "failure" => {
                expect_args(name, args, 0)?;
                Ok(Value::Bool(self.context.failure))
            }
            "cancelled" => {
                expect_args(name, args, 0)?;
                Ok(Value::Bool(self.context.cancelled))
            }
            "always" => {
                expect_args(name, args, 0)?;
                Ok(Value::Bool(true))
            }
            "contains" => {
                expect_args(name, args, 2)?;
                let haystack = self.eval(&args[0])?;
                let needle = self.eval(&args[1])?;
                Ok(Value::Bool(match &haystack {
                    Value::String(text) => text.contains(&needle.as_template_string()),
                    Value::Array(items) => items.iter().any(|item| equals(item, &needle)),
                    _ => false,
                }))
            }
            "startsWith" | "endsWith" => {
                expect_args(name, args, 2)?;
                let text = self.eval(&args[0])?.as_template_string();
                let needle = self.eval(&args[1])?.as_template_string();
                Ok(Value::Bool(if name == "startsWith" {
                    text.starts_with(&needle)
                } else {
                    text.ends_with(&needle)
                }))
            }
            other => Err(UbuilderError::new(
                ErrorCode::ConfigInvalid,
                format!("unknown expression function '{other}'"),
            )),
        }
    }
}

fn expect_args(name: &str, args: &[Ast], expected: usize) -> Result<()> {
    if args.len() != expected {
        return Err(UbuilderError::new(
            ErrorCode::ConfigInvalid,
            format!(
                "{name}() expects {expected} argument(s), got {}",
                args.len()
            ),
        ));
    }
    Ok(())
}

fn equals(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equals(a, b))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| equals(value, other)))
        }
        _ => false,
    }
}

fn compare(op: BinaryOp, left: &Value, right: &Value) -> bool {
    let ordering = match (left, right) {
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .partial_cmp(&b.as_f64())
            .unwrap_or(std::cmp::Ordering::Equal),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        _ => return false,
    };
    match op {
        BinaryOp::Lt => ordering.is_lt(),
        BinaryOp::Lte => ordering.is_le(),
        BinaryOp::Gt => ordering.is_gt(),
        BinaryOp::Gte => ordering.is_ge(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::expression::{parse, Expression};

    fn context() -> Context {
        Context::new()
            .with("matrix", serde_json::json!({ "target": "linux-x64" }))
            .with("env", serde_json::json!({ "PROFILE": "release" }))
    }

    #[test]
    fn truthiness_is_explicit() {
        assert!(!Value::Null.truthy());
        assert!(!Value::Bool(false).truthy());
        assert!(!Value::String(String::new()).truthy());
        assert!(!serde_json::json!(0).truthy());
        assert!(serde_json::json!([]).truthy());
        assert!(serde_json::json!({}).truthy());
    }

    #[test]
    fn equality_is_type_strict_for_mismatched_types() {
        assert!(!equals(&serde_json::json!("1"), &serde_json::json!(1)));
        assert!(equals(&serde_json::json!(1), &serde_json::json!(1.0)));
    }

    #[test]
    fn missing_values_are_null_not_errors() {
        let ast = parse("matrix.missing == null").unwrap();
        assert_eq!(evaluate(&ast, &context()).unwrap(), Value::Bool(true));
    }

    #[test]
    fn unavailable_contexts_are_errors() {
        let mut context = context();
        context.mark_unavailable("needs");
        let expression = Expression::parse("needs.build.result == 'success'").unwrap();
        assert!(expression.evaluate(&context).is_err());
    }

    #[test]
    fn functions_work() {
        let expression =
            Expression::parse("contains(matrix.target, 'linux') && startsWith(env.PROFILE, 'rel')")
                .unwrap();
        assert!(expression.evaluate_bool(&context()).unwrap());
    }

    #[test]
    fn identifiers_allow_hyphenated_output_keys() {
        let context = Context::new().with(
            "steps",
            serde_json::json!({ "cache": { "outputs": { "cache-hit": "true" } } }),
        );
        let expression = Expression::parse("steps.cache.outputs.cache-hit == 'true'").unwrap();
        assert!(expression.evaluate_bool(&context).unwrap());
    }
}
