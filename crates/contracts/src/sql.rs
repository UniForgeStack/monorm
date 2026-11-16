//! SQL text helpers that extensions and the host share.

/// `name` as a double-quoted SQL identifier.
pub fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A SQL string literal.
pub fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The PostgreSQL type of a column that has no `db_type`. `array` only changes `String`: `text[]` takes text parameters as they are.
pub fn column_type(ty: crate::ir::ColType, max_length: Option<u32>, array: bool) -> String {
    use crate::ir::ColType::*;
    match ty {
        BigInt => "bigint".into(),
        Int => "integer".into(),
        Float => "double precision".into(),
        Bool => "boolean".into(),
        String => match max_length {
            Some(n) => format!("varchar({n})"),
            None if array => "text".into(),
            None => "varchar".into(),
        },
        Text => "text".into(),
        DateTime => "timestamp with time zone".into(),
        Time => "time".into(),
        Date => "date".into(),
        Uuid => "uuid".into(),
        Json => "jsonb".into(),
        Decimal => "numeric".into(),
    }
}

/// The `WHEN` term that lets a block switch a trigger off on PostgreSQL: the transaction setting `monorm.skip`
/// names `name`, and the session user is a member of one of `roles` (the datasource's `bypass_roles`). Returns
/// `None` for no roles: the database then lets nobody skip the trigger. AND the term into the `WHEN` clause.
pub fn skip_guard(name: &str, roles: &[String]) -> Option<String> {
    if roles.is_empty() { return None; }
    let named = format!("{} = ANY(coalesce(nullif(current_setting('monorm.skip', true), ''), '{{}}')::text[])", literal(name));
    let member = roles.iter().map(|r| format!("pg_has_role(session_user, {}, 'member')", literal(r))).collect::<Vec<_>>().join(" OR ");
    Some(format!("NOT ({named} AND ({member}))"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_skip_guard_needs_a_role() {
        assert_eq!(super::skip_guard("x", &[]), None);
        let g = super::skip_guard("x", &["a".into(), "b'c".into()]).unwrap();
        assert_eq!(g, "NOT ('x' = ANY(coalesce(nullif(current_setting('monorm.skip', true), ''), '{}')::text[]) AND (pg_has_role(session_user, 'a', 'member') OR pg_has_role(session_user, 'b''c', 'member')))");
    }

    #[test]
    fn a_quote_in_a_name_is_doubled() {
        assert_eq!(super::ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(super::ident("t"), "\"t\"");
    }
}
