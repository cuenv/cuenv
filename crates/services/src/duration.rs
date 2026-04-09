//! Duration string parser for CUE configuration values.
//!
//! Parses human-readable duration strings like "500ms", "10s", "1m", "1h"
//! into `std::time::Duration`.

use std::time::Duration;

use crate::Error;

/// Parse a CUE duration string into a `Duration`.
///
/// Supported formats:
/// - `"500ms"` — milliseconds
/// - `"10s"` — seconds
/// - `"1m"` — minutes
/// - `"1h"` — hours
/// - `"1m30s"` — compound (minutes + seconds)
///
/// # Errors
///
/// Returns `Error::InvalidDuration` if the input cannot be parsed.
pub fn parse_duration(input: &str) -> crate::Result<Duration> {
    if input.is_empty() {
        return Err(Error::InvalidDuration {
            input: input.to_string(),
            message: "empty string".to_string(),
        });
    }

    let mut total_ms: u64 = 0;
    let mut num_buf = String::new();
    let mut chars = input.chars().peekable();

    while chars.peek().is_some() {
        // Collect digits (and optional decimal point)
        num_buf.clear();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() || c == '.' {
                num_buf.push(c);
                chars.next();
            } else {
                break;
            }
        }

        if num_buf.is_empty() {
            return Err(Error::InvalidDuration {
                input: input.to_string(),
                message: "expected a number".to_string(),
            });
        }

        let value: f64 = num_buf.parse().map_err(|_| Error::InvalidDuration {
            input: input.to_string(),
            message: format!("invalid number: {num_buf}"),
        })?;

        // Collect unit suffix
        let mut unit = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_ascii_alphabetic() {
                unit.push(c);
                chars.next();
            } else {
                break;
            }
        }

        let ms = match unit.as_str() {
            "ms" => value,
            "s" => value * 1000.0,
            "m" | "min" => value * 60_000.0,
            "h" | "hr" => value * 3_600_000.0,
            "" => {
                return Err(Error::InvalidDuration {
                    input: input.to_string(),
                    message: "missing unit (expected ms, s, m, or h)".to_string(),
                });
            }
            _ => {
                return Err(Error::InvalidDuration {
                    input: input.to_string(),
                    message: format!("unknown unit: {unit}"),
                });
            }
        };

        total_ms += ms as u64;
    }

    Ok(Duration::from_millis(total_ms))
}

/// Parse a duration string with a default value if the input is `None`.
pub fn parse_duration_or(input: Option<&str>, default: Duration) -> crate::Result<Duration> {
    match input {
        Some(s) => parse_duration(s),
        None => Ok(default),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_milliseconds() {
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("0ms").unwrap(), Duration::from_millis(0));
        assert_eq!(
            parse_duration("1500ms").unwrap(),
            Duration::from_millis(1500)
        );
    }

    #[test]
    fn test_seconds() {
        assert_eq!(parse_duration("1s").unwrap(), Duration::from_secs(1));
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("60s").unwrap(), Duration::from_secs(60));
    }

    #[test]
    fn test_minutes() {
        assert_eq!(parse_duration("1m").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
    }

    #[test]
    fn test_hours() {
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
    }

    #[test]
    fn test_compound() {
        assert_eq!(
            parse_duration("1m30s").unwrap(),
            Duration::from_secs(90)
        );
        assert_eq!(
            parse_duration("1h30m").unwrap(),
            Duration::from_secs(5400)
        );
    }

    #[test]
    fn test_errors() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("10").is_err());
        assert!(parse_duration("10xyz").is_err());
    }

    #[test]
    fn test_parse_duration_or() {
        let default = Duration::from_secs(10);
        assert_eq!(parse_duration_or(None, default).unwrap(), default);
        assert_eq!(
            parse_duration_or(Some("5s"), default).unwrap(),
            Duration::from_secs(5)
        );
    }
}
