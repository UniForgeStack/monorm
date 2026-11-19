//! The text form of a Duration, one format for the admin, the Rust client and both bindings:
//! `[-][D ]HH:MM:SS[.ffffff]`. `parse_micros` also reads a time-only ISO 8601 subset (`PT1H30M`).

const SECOND: u128 = 1_000_000;
const DAY: u128 = 86_400 * SECOND;

/// The text of `micros`: days only when there is one, a fraction only when it is not zero.
pub fn format_micros(micros: i64) -> String {
    let abs = micros.unsigned_abs() as u128;
    let (days, rest) = (abs / DAY, abs % DAY);
    let (secs, frac) = (rest / SECOND, rest % SECOND);
    let mut out = String::new();
    if micros < 0 {
        out.push('-');
    }
    if days > 0 {
        out.push_str(&format!("{days} "));
    }
    out.push_str(&format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60));
    if frac > 0 {
        out.push_str(&format!(".{frac:06}"));
    }
    out
}

/// The microseconds of `text`; an error names what is wrong.
pub fn parse_micros(text: &str) -> Result<i64, String> {
    let bad = || format!("`{text}` is not a duration: use [-][D ]HH:MM:SS[.ffffff] or PT1H30M");
    let trimmed = text.trim();
    let (negative, body) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed.strip_prefix('+').unwrap_or(trimmed)),
    };
    let total = if let Some(iso) = body.strip_prefix("PT").or_else(|| body.strip_prefix("pt")) {
        parse_iso(iso).ok_or_else(bad)?
    } else {
        parse_clock(body).ok_or_else(bad)?
    };
    let signed = if negative { -(total as i128) } else { total as i128 };
    i64::try_from(signed).map_err(|_| format!("`{text}` is out of range for a Duration"))
}

fn digits(s: &str) -> Option<u128> {
    if s.is_empty() || s.len() > 30 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `.ffffff` digits (1 to 6) as microseconds.
fn fraction(s: &str) -> Option<u128> {
    if s.is_empty() || s.len() > 6 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(s.parse::<u128>().ok()? * 10u128.pow(6 - s.len() as u32))
}

/// Seconds and an optional fraction: `SS[.ffffff]`.
fn seconds(s: &str) -> Option<u128> {
    match s.split_once('.') {
        Some((whole, frac)) => Some(digits(whole)? * SECOND + fraction(frac)?),
        None => Some(digits(s)? * SECOND),
    }
}

fn parse_clock(body: &str) -> Option<u128> {
    let (days, clock) = match body.split_once(' ') {
        Some((d, rest)) => (digits(d)?, rest.trim_start()),
        None => (0, body),
    };
    let parts: Vec<&str> = clock.split(':').collect();
    let [h, m, s] = parts[..] else { return None };
    if m.len() != 2 || s.split('.').next()?.len() != 2 {
        return None;
    }
    let (minutes, secs) = (digits(m)?, seconds(s)?);
    if minutes > 59 || secs >= 60 * SECOND {
        return None;
    }
    days.checked_mul(DAY)?.checked_add(digits(h)?.checked_mul(3600 * SECOND)?)?.checked_add(minutes * 60 * SECOND)?.checked_add(secs)
}

/// `[nH][nM][n[.f]S]` after the `PT`, at least one part.
fn parse_iso(s: &str) -> Option<u128> {
    if s.is_empty() {
        return None;
    }
    let (mut total, mut rest, mut last) = (0u128, s, 0);
    while !rest.is_empty() {
        let end = rest.find(|c: char| c.is_ascii_alphabetic())?;
        let (number, unit) = (&rest[..end], rest[end..].chars().next()?.to_ascii_uppercase());
        let (rank, value) = match unit {
            'H' => (1, digits(number)?.checked_mul(3600 * SECOND)?),
            'M' => (2, digits(number)?.checked_mul(60 * SECOND)?),
            'S' => (3, seconds(number)?),
            _ => return None,
        };
        if rank <= last {
            return None;
        }
        last = rank;
        total = total.checked_add(value)?;
        rest = &rest[end + 1..];
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vectors that the Python and TypeScript parser tests read too.
    fn vectors() -> serde_json::Value {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../languages/test-vectors/duration-text.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn formats_and_parses_the_shared_vectors() {
        let v = vectors();
        for pair in v["roundtrip"].as_array().unwrap() {
            let micros: i64 = pair[0].as_str().unwrap().parse().unwrap();
            let text = pair[1].as_str().unwrap();
            assert_eq!(format_micros(micros), text);
            assert_eq!(parse_micros(text).unwrap(), micros, "{text}");
        }
        for pair in v["parse_only"].as_array().unwrap() {
            let text = pair[0].as_str().unwrap();
            assert_eq!(parse_micros(text).unwrap(), pair[1].as_str().unwrap().parse::<i64>().unwrap(), "{text}");
        }
        for text in v["refused"].as_array().unwrap() {
            assert!(parse_micros(text.as_str().unwrap()).is_err(), "{text}");
        }
    }
}
