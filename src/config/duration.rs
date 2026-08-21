//! The manifest duration grammar: `<whole number><ms|s|m|h>`, with the unit
//! optional and defaulting to seconds.
//!
//! Every duration-valued manifest field routes through `parse`, so `validate`
//! and the runtime can never disagree about what a manifest says. Values are
//! rejected, never floored or truncated: a supervisor that silently substitutes
//! its own number for the operator's is the harder bug to find.

use std::{fmt, time::Duration};

/// Why a duration string could not be interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DurationError {
    /// The value was empty or entirely whitespace.
    Empty,
    /// The value did not match `<integer><ms|s|m|h>`.
    Malformed,
    /// The value parsed but does not fit in a `Duration`.
    Overflow,
    /// The value was zero where zero has no coherent meaning.
    ZeroNotAllowed,
}

impl fmt::Display for DurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("duration is empty"),
            Self::Malformed => f.write_str(
                "expected a whole number with an optional unit: ms, s, m, or h (e.g. \"100ms\", \"2s\", \"5m\")",
            ),
            Self::Overflow => f.write_str("duration is too large to represent"),
            Self::ZeroNotAllowed => f.write_str("must be greater than zero"),
        }
    }
}

/// Whether a unit counts milliseconds or seconds. Seconds are never converted
/// to milliseconds: `Duration` holds far more seconds than a `u64` of millis
/// can express, and multiplying first would reject values that fit.
enum Scale {
    Millis,
    Secs(u64),
}

/// Parses a manifest duration. A bare number is seconds, so `"15"` and `"15s"`
/// are the same value and every manifest written before `ms` existed still
/// means what it meant.
pub fn parse(raw: &str) -> Result<Duration, DurationError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(DurationError::Empty);
    }

    let (amount_str, scale) = if let Some(stripped) = value.strip_suffix("ms") {
        (stripped, Scale::Millis)
    } else if let Some(stripped) = value.strip_suffix('s') {
        (stripped, Scale::Secs(1))
    } else if let Some(stripped) = value.strip_suffix('m') {
        (stripped, Scale::Secs(60))
    } else if let Some(stripped) = value.strip_suffix('h') {
        (stripped, Scale::Secs(3_600))
    } else {
        (value, Scale::Secs(1))
    };

    // A redundant leading `+` parsed before this grammar existed, so it keeps
    // parsing: nothing is gained by breaking a manifest over a sign.
    let amount_str = amount_str.trim();
    let digits = amount_str.strip_prefix('+').unwrap_or(amount_str);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(DurationError::Malformed);
    }

    let amount: u64 = digits.parse().map_err(|_| DurationError::Overflow)?;
    Ok(match scale {
        Scale::Millis => Duration::from_millis(amount),
        Scale::Secs(per_unit) => Duration::from_secs(
            amount
                .checked_mul(per_unit)
                .ok_or(DurationError::Overflow)?,
        ),
    })
}

/// Parses a duration that must be positive. Used for fields where zero would
/// spin — a health-check interval of zero probes in a tight loop rather than
/// meaning "no wait".
pub fn parse_positive(raw: &str) -> Result<Duration, DurationError> {
    let parsed = parse(raw)?;
    if parsed.is_zero() {
        return Err(DurationError::ZeroNotAllowed);
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_scale_correctly() {
        assert_eq!(parse("100ms").unwrap(), Duration::from_millis(100));
        assert_eq!(parse("10s").unwrap(), Duration::from_secs(10));
        assert_eq!(parse("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse("2h").unwrap(), Duration::from_secs(7200));
    }

    #[test]
    fn bare_number_is_seconds() {
        assert_eq!(parse("15").unwrap(), Duration::from_secs(15));
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        assert_eq!(parse("  250 ms ").unwrap(), Duration::from_millis(250));
    }

    #[test]
    fn zero_is_allowed_by_default() {
        for raw in ["0", "0s", "0ms", "0h"] {
            assert!(parse(raw).unwrap().is_zero(), "{raw}");
        }
    }

    #[test]
    fn zero_is_refused_where_it_would_spin() {
        assert_eq!(parse_positive("0s"), Err(DurationError::ZeroNotAllowed));
        assert_eq!(parse_positive("1ms").unwrap(), Duration::from_millis(1));
    }

    #[test]
    fn malformed_values_are_rejected() {
        for raw in ["abc", "0.5s", "-1s", "1.5", "10sec", "s", "1 2s"] {
            assert!(parse(raw).is_err(), "{raw} should not parse");
        }
    }

    #[test]
    fn a_redundant_plus_still_parses() {
        assert_eq!(parse("+3s").unwrap(), Duration::from_secs(3));
    }

    #[test]
    fn empty_is_its_own_error() {
        assert_eq!(parse(""), Err(DurationError::Empty));
        assert_eq!(parse("   "), Err(DurationError::Empty));
    }

    #[test]
    fn overflow_is_rejected_not_saturated() {
        assert_eq!(
            parse(&format!("{}h", u64::MAX)),
            Err(DurationError::Overflow)
        );
        assert_eq!(
            parse(&format!("{}0", u64::MAX)),
            Err(DurationError::Overflow)
        );
    }

    #[test]
    fn the_widest_second_count_still_parses() {
        assert_eq!(
            parse(&format!("{}s", u64::MAX)).unwrap(),
            Duration::from_secs(u64::MAX)
        );
    }
}
