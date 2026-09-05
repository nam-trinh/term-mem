//! `--since` parsing. Deliberately small: duration suffixes, a handful of
//! English forms, and an ISO date. Anything else is an error rather than a
//! guess — a `--since` that silently means "the epoch" returns the whole
//! archive and looks like it worked.

use anyhow::{bail, Result};

pub fn parse(spec: &str) -> Result<i64> {
    let now = crate::capture::now_ms();
    let s = spec.trim().to_lowercase();
    let s = s.strip_suffix(" ago").unwrap_or(&s).trim();

    if s == "today" {
        return Ok(now - now.rem_euclid(86_400_000));
    }
    if s == "yesterday" {
        return Ok(now - now.rem_euclid(86_400_000) - 86_400_000);
    }

    // `2026-03-01`
    if s.len() == 10 && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' {
        if let Some(ms) =
            crate::capture::adapters::claude_code::parse_rfc3339_ms(&format!("{s}T00:00:00Z"))
        {
            return Ok(ms);
        }
    }

    // `2h`, `30m`, `7d`, `3w`, and `2 hours`, `7 days`
    let (num, unit) = split_number(s);
    let Some(n) = num else {
        bail!("cannot understand --since '{spec}'; try `2h`, `7d`, `3w`, or `2026-03-01`")
    };
    let unit = unit.trim().trim_end_matches('s');
    let ms = match unit {
        "m" | "min" | "minute" => 60_000,
        "h" | "hr" | "hour" => 3_600_000,
        "d" | "day" | "" => 86_400_000,
        "w" | "week" => 604_800_000,
        "mo" | "month" => 2_592_000_000,
        "y" | "year" => 31_536_000_000,
        other => bail!("unknown time unit '{other}' in --since '{spec}'"),
    };
    Ok(now - n * ms)
}

fn split_number(s: &str) -> (Option<i64>, &str) {
    let idx = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    if idx == 0 {
        return (None, s);
    }
    (s[..idx].parse().ok(), &s[idx..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_are_relative_to_now() {
        let now = crate::capture::now_ms();
        let two_h = parse("2h").unwrap();
        assert!((now - two_h - 7_200_000).abs() < 2_000);
        assert!((now - parse("2 hours ago").unwrap() - 7_200_000).abs() < 2_000);
        assert!((now - parse("7d").unwrap() - 604_800_000).abs() < 2_000);
    }

    #[test]
    fn iso_dates_are_absolute() {
        assert_eq!(parse("2026-03-01").unwrap(), 1_772_323_200_000);
    }

    #[test]
    fn nonsense_is_an_error_not_the_epoch() {
        assert!(parse("last tuesday-ish").is_err());
        assert!(parse("5 fortnights").is_err());
        assert!(parse("").is_err());
    }
}
