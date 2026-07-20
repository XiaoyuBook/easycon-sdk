#![forbid(unsafe_code)]
//! Shared deterministic helpers for the non-release hardware qualification CLI.

/// Nearest-rank distribution used by Phase 2 latency evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Distribution {
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub max: u64,
}

/// Computes a nearest-rank distribution without dropping outliers.
#[must_use]
pub fn distribution(values: &[u64]) -> Option<Distribution> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    Some(Distribution {
        p50: nearest_rank(&sorted, 50),
        p95: nearest_rank(&sorted, 95),
        p99: nearest_rank(&sorted, 99),
        max: *sorted.last().expect("non-empty distribution"),
    })
}

fn nearest_rank(sorted: &[u64], percentile: usize) -> u64 {
    let rank = sorted.len().saturating_mul(percentile).div_ceil(100);
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

/// Returns the value following an option exactly once.
pub fn option_value<'a>(arguments: &'a [String], option: &str) -> Result<Option<&'a str>, String> {
    let positions: Vec<_> = arguments
        .iter()
        .enumerate()
        .filter_map(|(index, value)| (value == option).then_some(index))
        .collect();
    match positions.as_slice() {
        [] => Ok(None),
        [index] => arguments
            .get(index + 1)
            .map(String::as_str)
            .ok_or_else(|| format!("missing value for {option}"))
            .map(Some),
        _ => Err(format!("option {option} may be supplied only once")),
    }
}

/// Parses a required typed option.
pub fn required_value<T: std::str::FromStr>(
    arguments: &[String],
    option: &str,
) -> Result<T, String> {
    option_value(arguments, option)?
        .ok_or_else(|| format!("missing required option {option}"))?
        .parse()
        .map_err(|_| format!("invalid value for {option}"))
}

/// Parses an optional typed option with a default.
pub fn value_or<T: std::str::FromStr>(
    arguments: &[String],
    option: &str,
    default: T,
) -> Result<T, String> {
    option_value(arguments, option)?.map_or(Ok(default), |value| {
        value
            .parse()
            .map_err(|_| format!("invalid value for {option}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_keeps_every_outlier() {
        assert_eq!(
            distribution(&[100, 1, 2, 3, 4]),
            Some(Distribution {
                p50: 3,
                p95: 100,
                p99: 100,
                max: 100,
            })
        );
        assert_eq!(distribution(&[]), None);
    }

    #[test]
    fn options_reject_missing_and_duplicate_values() {
        let arguments = vec!["--port".to_owned(), "COM8".to_owned()];
        assert_eq!(option_value(&arguments, "--port"), Ok(Some("COM8")));
        assert!(required_value::<usize>(&arguments, "--cycles").is_err());
        assert!(option_value(&["--port".to_owned()], "--port").is_err());
        assert!(
            option_value(
                &[
                    "--port".to_owned(),
                    "COM8".to_owned(),
                    "--port".to_owned(),
                    "COM9".to_owned(),
                ],
                "--port"
            )
            .is_err()
        );
    }
}
