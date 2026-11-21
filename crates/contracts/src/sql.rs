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
        Bytes => "bytea".into(),
        Duration => "interval".into(),
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

/// The condition of a change helper over `columns`: `any_change` is `OR` of `OLD.c IS DISTINCT FROM NEW.c`, `all_change` the `AND`,
/// and the two `dont_change` kinds test `IS NOT DISTINCT FROM` the same way. Returns `None` for no column. SQLite spells the
/// tests `IS NOT` and `IS`. The text reads `OLD` and `NEW`: use it in a row trigger on update only.
pub fn change_condition(dialect: crate::dialect::Dialect, kind: crate::ir::ChangeKind, columns: &[String]) -> Option<String> {
    use crate::ir::ChangeKind::*;
    if columns.is_empty() { return None; }
    let same = matches!(kind, AnyDontChange | AllDontChange);
    let op = match (dialect, same) {
        (crate::dialect::Dialect::Postgres, false) => "IS DISTINCT FROM",
        (crate::dialect::Dialect::Postgres, true) => "IS NOT DISTINCT FROM",
        (crate::dialect::Dialect::Sqlite, false) => "IS NOT",
        (crate::dialect::Dialect::Sqlite, true) => "IS",
    };
    let join = if matches!(kind, AnyChange | AnyDontChange) { " OR " } else { " AND " };
    let tests: Vec<String> = columns.iter().map(|c| format!("OLD.{c} {op} NEW.{c}", c = ident(c))).collect();
    Some(if tests.len() == 1 { tests[0].clone() } else { format!("({})", tests.join(join)) })
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
    fn a_change_condition_joins_the_column_tests() {
        use crate::{dialect::Dialect::{Postgres, Sqlite}, ir::ChangeKind::*};
        let cols = ["a".to_owned(), "b".to_owned()];
        let one = ["a".to_owned()];
        assert_eq!(super::change_condition(Postgres, AnyChange, &cols).unwrap(), "(OLD.\"a\" IS DISTINCT FROM NEW.\"a\" OR OLD.\"b\" IS DISTINCT FROM NEW.\"b\")");
        assert_eq!(super::change_condition(Postgres, AllChange, &cols).unwrap(), "(OLD.\"a\" IS DISTINCT FROM NEW.\"a\" AND OLD.\"b\" IS DISTINCT FROM NEW.\"b\")");
        assert_eq!(super::change_condition(Postgres, AnyDontChange, &cols).unwrap(), "(OLD.\"a\" IS NOT DISTINCT FROM NEW.\"a\" OR OLD.\"b\" IS NOT DISTINCT FROM NEW.\"b\")");
        assert_eq!(super::change_condition(Postgres, AllDontChange, &one).unwrap(), "OLD.\"a\" IS NOT DISTINCT FROM NEW.\"a\"");
        assert_eq!(super::change_condition(Sqlite, AnyChange, &one).unwrap(), "OLD.\"a\" IS NOT NEW.\"a\"");
        assert_eq!(super::change_condition(Sqlite, AllDontChange, &one).unwrap(), "OLD.\"a\" IS NEW.\"a\"");
        assert_eq!(super::change_condition(Postgres, AnyChange, &[]), None);
    }

    #[test]
    fn a_quote_in_a_name_is_doubled() {
        assert_eq!(super::ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(super::ident("t"), "\"t\"");
    }
}
