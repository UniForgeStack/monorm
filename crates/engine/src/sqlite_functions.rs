//! Scalar functions of the SQLite connection that give exact DateTime arithmetic.
//! A DateTime is text `YYYY-MM-DD HH:MM:SS[.ffffff]` (the binder's form), so `strftime` (ms) and `julianday` (a double)
//! cannot add or subtract microseconds exactly. A Duration is an integer of microseconds.
//!
//! The functions run only from a statement MonOrm built: they are `SQLITE_DIRECTONLY`, so a trigger, a view or an index
//! cannot call them, and a query copied into the `sqlite3` tool fails with "no such function".

/// `monorm_dt_add(datetime, microseconds) -> datetime`, in the binder's text form.
pub const DT_ADD: &str = "monorm_dt_add";
/// `monorm_dt_diff(a, b) -> microseconds` of `a - b`.
pub const DT_DIFF: &str = "monorm_dt_diff";

/// `monorm_dur_add(a, b)`, `monorm_dur_sub(a, b)`: Duration sums that raise an error out of the 64-bit range, like PostgreSQL.
pub const DUR_ADD: &str = "monorm_dur_add";
pub const DUR_SUB: &str = "monorm_dur_sub";
/// `monorm_dur_mul(a, b)`: a Duration times an integer or a float, rounded to the nearest microsecond (a tie away from zero).
pub const DUR_MUL: &str = "monorm_dur_mul";
/// `monorm_dur_div(a, b)`: a Duration divided by a number; a zero divisor is an error, like PostgreSQL.
pub const DUR_DIV: &str = "monorm_dur_div";

/// The text form of a DateTime that the SQLite binder writes: fraction digits only when the value has them.
const FORMAT: &str = "%Y-%m-%d %H:%M:%S%.f";

fn parse(text: &str) -> Result<chrono::NaiveDateTime, String> {
    chrono::DateTime::parse_from_rfc3339(text)
        .map(|d| d.naive_utc())
        .or_else(|_| chrono::NaiveDateTime::parse_from_str(text, FORMAT))
        .map_err(|e| format!("{text:?} is not a DateTime: {e}"))
}

/// `at + microseconds`, or an error text when the result is out of range.
pub fn add(at: &str, micros: i64) -> Result<String, String> {
    let from = parse(at)?;
    from.checked_add_signed(chrono::Duration::microseconds(micros))
        .map(|d| d.format(FORMAT).to_string())
        .ok_or_else(|| format!("{at:?} plus {micros} microseconds is out of the DateTime range"))
}

/// `a - b` in microseconds, or an error text when it does not fit in 64 bits.
pub fn diff(a: &str, b: &str) -> Result<i64, String> {
    parse(a)?.signed_duration_since(parse(b)?).num_microseconds().ok_or_else(|| "the difference does not fit in 64 bits of microseconds".to_owned())
}

const RANGE: &str = "interval out of range";

/// A float result back to microseconds: rounded, and an error when it does not fit.
fn from_float(x: f64) -> Result<i64, String> {
    let r = x.round();
    // 2^63 is the first float above i64::MAX; the range check is on the float, not on a cast.
    if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&r) { Ok(r as i64) } else { Err(RANGE.to_owned()) }
}

/// A number operand of a Duration product or quotient.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Num {
    Int(i64),
    Float(f64),
}

/// `a * b` in microseconds.
pub fn mul(a: i64, b: Num) -> Result<i64, String> {
    match b {
        Num::Int(n) => a.checked_mul(n).ok_or_else(|| RANGE.to_owned()),
        Num::Float(f) => from_float(a as f64 * f),
    }
}

/// `a / b` in microseconds: an exact integer quotient rounded half away from zero, a float one for a float divisor.
pub fn div(a: i64, b: Num) -> Result<i64, String> {
    match b {
        Num::Int(0) => Err("division by zero".to_owned()),
        Num::Int(n) => {
            let (a, n) = (i128::from(a), i128::from(n));
            let (q, r) = (a / n, a % n);
            let q = if 2 * r.abs() >= n.abs() { q + if (a < 0) != (n < 0) { -1 } else { 1 } } else { q };
            i64::try_from(q).map_err(|_| RANGE.to_owned())
        }
        Num::Float(0.0) => Err("division by zero".to_owned()),
        Num::Float(f) => from_float(a as f64 / f),
    }
}

/// Registers the functions on a connection. Every connection MonOrm opens calls this one function.
#[cfg(feature = "sqlite")]
pub fn register(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    use rusqlite::functions::FunctionFlags;
    use rusqlite::types::ValueRef;
    let flags = FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC | FunctionFlags::SQLITE_DIRECTONLY;
    fn fail(e: String) -> rusqlite::Error {
        rusqlite::Error::UserFunctionError(e.into())
    }
    fn text(v: ValueRef<'_>) -> rusqlite::Result<Option<String>> {
        match v {
            ValueRef::Text(t) => std::str::from_utf8(t).map(|s| Some(s.to_owned())).map_err(|e| fail(e.to_string())),
            ValueRef::Null => Ok(None),
            _ => Err(fail("a DateTime is text".into())),
        }
    }
    conn.create_scalar_function(DT_ADD, 2, flags, move |ctx| {
        let at = text(ctx.get_raw(0))?;
        let micros = match ctx.get_raw(1) {
            ValueRef::Integer(n) => n,
            ValueRef::Null => return Ok(None),
            _ => return Err(fail("a Duration is an integer of microseconds".into())),
        };
        at.map(|at| add(&at, micros).map_err(fail)).transpose()
    })?;
    fn int(v: ValueRef<'_>) -> rusqlite::Result<Option<i64>> {
        match v {
            ValueRef::Integer(n) => Ok(Some(n)),
            ValueRef::Null => Ok(None),
            _ => Err(fail("a Duration is an integer of microseconds".into())),
        }
    }
    fn num(v: ValueRef<'_>) -> rusqlite::Result<Option<Num>> {
        match v {
            ValueRef::Integer(n) => Ok(Some(Num::Int(n))),
            ValueRef::Real(f) => Ok(Some(Num::Float(f))),
            ValueRef::Null => Ok(None),
            _ => Err(fail("a Duration is scaled by a number".into())),
        }
    }
    conn.create_scalar_function(DUR_ADD, 2, flags, move |ctx| {
        let (Some(a), Some(b)) = (int(ctx.get_raw(0))?, int(ctx.get_raw(1))?) else { return Ok(None) };
        a.checked_add(b).map(Some).ok_or_else(|| fail(RANGE.into()))
    })?;
    conn.create_scalar_function(DUR_SUB, 2, flags, move |ctx| {
        let (Some(a), Some(b)) = (int(ctx.get_raw(0))?, int(ctx.get_raw(1))?) else { return Ok(None) };
        a.checked_sub(b).map(Some).ok_or_else(|| fail(RANGE.into()))
    })?;
    // `n * Duration` reaches these with the number first, so the integer side is found by type.
    conn.create_scalar_function(DUR_MUL, 2, flags, move |ctx| {
        let (Some(a), Some(b)) = (num(ctx.get_raw(0))?, num(ctx.get_raw(1))?) else { return Ok(None) };
        let (d, n) = match (a, b) {
            (Num::Int(d), n) => (d, n),
            (n, Num::Int(d)) => (d, n),
            _ => return Err(fail("a Duration is an integer of microseconds".into())),
        };
        mul(d, n).map(Some).map_err(fail)
    })?;
    conn.create_scalar_function(DUR_DIV, 2, flags, move |ctx| {
        let (Some(a), Some(b)) = (int(ctx.get_raw(0))?, num(ctx.get_raw(1))?) else { return Ok(None) };
        div(a, b).map(Some).map_err(fail)
    })?;
    conn.create_scalar_function(DT_DIFF, 2, flags, move |ctx| {
        let (Some(a), Some(b)) = (text(ctx.get_raw(0))?, text(ctx.get_raw(1))?) else { return Ok(None) };
        diff(&a, &b).map(Some).map_err(fail)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_text_form_matches_the_binder() {
        assert_eq!(add("2024-01-01 00:00:00", 500_000).unwrap(), "2024-01-01 00:00:00.500");
        assert_eq!(add("2024-01-01 00:00:00", 1).unwrap(), "2024-01-01 00:00:00.000001");
        assert_eq!(add("2024-01-01 00:00:00.000001", -1).unwrap(), "2024-01-01 00:00:00");
        assert_eq!(add("2024-01-01 00:00:00", -86_400_000_000).unwrap(), "2023-12-31 00:00:00");
    }

    #[test]
    fn a_difference_is_exact_and_signed() {
        assert_eq!(diff("2024-01-01 00:00:00.000001", "2024-01-01 00:00:00").unwrap(), 1);
        assert_eq!(diff("2024-01-01 00:00:00", "2024-01-02 00:00:00").unwrap(), -86_400_000_000);
    }

    #[test]
    fn a_duration_product_and_quotient_round_and_stay_in_range() {
        assert_eq!(mul(10, Num::Int(3)).unwrap(), 30);
        assert_eq!(mul(10, Num::Float(0.25)).unwrap(), 3);
        assert_eq!(mul(-10, Num::Float(0.25)).unwrap(), -3);
        assert!(mul(i64::MAX, Num::Int(2)).is_err());
        assert!(mul(i64::MAX, Num::Float(2.0)).is_err());
        assert_eq!(div(10, Num::Int(3)).unwrap(), 3);
        assert_eq!(div(11, Num::Int(3)).unwrap(), 4);
        assert_eq!(div(-11, Num::Int(3)).unwrap(), -4);
        assert_eq!(div(5, Num::Int(2)).unwrap(), 3);
        assert_eq!(div(i64::MIN, Num::Int(-1)).unwrap_err(), RANGE);
        assert_eq!(div(1, Num::Int(0)).unwrap_err(), "division by zero");
        assert_eq!(div(1, Num::Float(0.0)).unwrap_err(), "division by zero");
    }

    #[test]
    fn a_bad_input_is_an_error() {
        assert!(add("not a time", 1).is_err());
        assert!(add("2024-01-01 00:00:00", i64::MAX).is_err());
        assert!(diff("not a time", "2024-01-01 00:00:00").is_err());
    }
}
