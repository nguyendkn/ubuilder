//! Workflow schema types. These are the parser-facing models; the compiler
//! lowers them into [`crate::pipeline::ir`] before the runtime sees them.

pub mod job;
pub mod matrix;
pub mod step;
pub mod trigger;
pub mod workflow;

pub use job::*;
pub use matrix::*;
pub use step::*;
pub use trigger::*;

use crate::error::{ErrorCode, Result, UbuilderError};
use serde::{Deserialize, Deserializer};
use std::time::Duration;

/// Parse a human duration (`500ms`, `2s`, `10m`, `1h`, `1h30m`).
pub fn parse_duration(value: &str) -> Result<Duration> {
    let text = value.trim();
    if text.is_empty() {
        return Err(invalid("empty duration"));
    }
    let mut total = Duration::ZERO;
    let mut number = String::new();
    let mut unit = String::new();
    let mut parts = 0usize;
    let flush = |number: &mut String, unit: &mut String, total: &mut Duration| -> Result<()> {
        if number.is_empty() {
            return Err(invalid(format!("duration '{value}' is missing a number")));
        }
        let amount: u64 = number
            .parse()
            .map_err(|_| invalid(format!("invalid duration number '{number}' in '{value}'")))?;
        let millis = match unit.as_str() {
            "ms" => amount,
            "s" => amount.saturating_mul(1_000),
            "m" => amount.saturating_mul(60_000),
            "h" => amount.saturating_mul(3_600_000),
            other => {
                return Err(invalid(format!(
                    "unknown duration unit '{other}' in '{value}' (use ms, s, m or h)"
                )))
            }
        };
        *total += Duration::from_millis(millis);
        number.clear();
        unit.clear();
        Ok(())
    };
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            if !unit.is_empty() {
                flush(&mut number, &mut unit, &mut total)?;
                parts += 1;
            }
            number.push(ch);
        } else if ch.is_whitespace() {
            continue;
        } else {
            unit.push(ch);
        }
    }
    if !number.is_empty() || !unit.is_empty() {
        flush(&mut number, &mut unit, &mut total)?;
        parts += 1;
    }
    if parts == 0 || total.is_zero() {
        return Err(invalid(format!(
            "duration '{value}' must be greater than zero"
        )));
    }
    Ok(total)
}

fn invalid(message: impl Into<String>) -> UbuilderError {
    UbuilderError::new(ErrorCode::ConfigInvalid, message)
}

/// Deserialize a duration from a YAML string.
pub fn duration_from_str<'de, D>(deserializer: D) -> std::result::Result<Option<Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    raw.map(|value| parse_duration(&value).map_err(serde::de::Error::custom))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duration_suffixes() {
        assert_eq!(parse_duration("250ms").unwrap().as_millis(), 250);
        assert_eq!(parse_duration("2s").unwrap().as_secs(), 2);
        assert_eq!(parse_duration("1h30m").unwrap().as_secs(), 5400);
        assert!(parse_duration("10").is_err());
        assert!(parse_duration("5d").is_err());
    }
}
