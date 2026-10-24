//! Lowers a checked expression tree (`expr::Node`) to canonical SQL text for one dialect.
//!
//! One tree gives one text: the parentheses are the fewest that keep the meaning, keywords are
//! upper case, and a column name is quoted only where it must be.

use super::expr::{BinOp, Diagnostic, Method, Node, NodeKind, Ty};
use crate::dialect::Dialect;
use crate::ir::{ColType, SchemaIr};
use crate::migrate::model::quote_literal;
use crate::migrate::pg::ident;

type Rendered = (String, u8);

// Binding strength, from the loosest to the tightest.
const OR: u8 = 1;
const AND: u8 = 2;
const NOT: u8 = 3;
const COMPARE: u8 = 4;
const SUM: u8 = 5;
const PRODUCT: u8 = 6;
const UNARY: u8 = 7;
const ATOM: u8 = 8;

/// The SQL text of `node`.
pub(super) fn render(ir: &SchemaIr, dialect: Dialect, node: &Node) -> Result<String, Diagnostic> {
    let root = match &node.kind {
        NodeKind::Key { inner, .. } => inner,
        _ => node,
    };
    // The migration SQL puts its own parentheses around a whole check or key.
    if let NodeKind::Sql(text) = &root.kind {
        return Ok(text.clone());
    }
    Ok(Renderer { ir, dialect }.node(root)?.0)
}

/// A column name, quoted when it is not a plain lower-case word or it is a reserved word.
pub(super) fn column_name(name: &str) -> String {
    let plain = name.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if plain && !RESERVED.contains(&name) {
        name.to_string()
    } else {
        ident(name)
    }
}

/// The characters that `LIKE` reads as syntax. `\` comes first, so an inserted escape is never escaped again.
const LIKE_SYNTAX: [&str; 3] = ["\\", "%", "_"];

/// `text` with the `LIKE` syntax escaped, so it matches itself.
fn like_escape(text: &str) -> String {
    LIKE_SYNTAX.iter().fold(text.to_string(), |text, c| text.replace(*c, &format!("\\{c}")))
}

/// The SQL that escapes the `LIKE` syntax in the value of `sql`.
fn like_escape_sql(sql: String) -> String {
    LIKE_SYNTAX.iter().fold(sql, |sql, c| format!("replace({sql}, {}, {})", quote_literal(c), quote_literal(&format!("\\{c}"))))
}

/// `template` with `{*}` as all of `args` and `{i}` as `args[i]`. One pass, so the text of an argument is never read again.
fn expand(template: &str, args: &[String]) -> String {
    let mut text = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        text.push_str(&rest[..open]);
        rest = &rest[open..];
        let Some(close) = rest.find('}') else { break };
        match &rest[1..close] {
            "*" => text.push_str(&args.join(", ")),
            index => match index.parse::<usize>().ok().and_then(|i| args.get(i)) {
                Some(arg) => text.push_str(arg),
                None => text.push_str(&rest[..=close]),
            },
        }
        rest = &rest[close + 1..];
    }
    text.push_str(rest);
    text
}

/// A `DateTime`, `Date` or `Uuid` literal in the text form that the SQLite engine stores (`bind` in `engine/src/db/sqlite.rs`).
/// SQLite compares those columns as text, so the form the author wrote (RFC 3339, `2024-1-5`, upper case) would not match.
fn sqlite_stored(ty: ColType, text: &str) -> Option<String> {
    match ty {
        ColType::DateTime => {
            let at = chrono::DateTime::parse_from_rfc3339(text).ok()?;
            Some(at.with_timezone(&chrono::Utc).format("%Y-%m-%d %H:%M:%S%.f").to_string())
        }
        ColType::Date => Some(chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?.to_string()),
        ColType::Uuid => Some(uuid::Uuid::parse_str(text).ok()?.to_string()),
        _ => None,
    }
}

struct Renderer<'a> {
    ir: &'a SchemaIr,
    dialect: Dialect,
}

impl Renderer<'_> {
    fn sqlite(&self) -> bool {
        self.dialect == Dialect::Sqlite
    }

    fn node(&self, node: &Node) -> Result<Rendered, Diagnostic> {
        let fail = |message: String| Diagnostic { span: node.span, message };
        Ok(match &node.kind {
            NodeKind::Null => ("NULL".into(), ATOM),
            NodeKind::Bool(b) => {
                let text = match (self.sqlite(), *b) {
                    (true, true) => "1",
                    (true, false) => "0",
                    (false, true) => "TRUE",
                    (false, false) => "FALSE",
                };
                (text.into(), ATOM)
            }
            NodeKind::Int(n) => (n.to_string(), ATOM),
            NodeKind::Float(text) => (text.clone(), ATOM),
            NodeKind::Str(text) => {
                let stored = match node.ty {
                    Ty::Scalar(ty) if self.sqlite() => sqlite_stored(ty, text),
                    _ => None,
                };
                (quote_literal(stored.as_deref().unwrap_or(text)), ATOM)
            }
            NodeKind::Field { via, model, field, .. } => {
                if !via.is_empty() {
                    return Err(fail("a relation path has no SQL form here".into()));
                }
                let column = self
                    .ir
                    .models
                    .iter()
                    .find(|m| m.name == *model)
                    .and_then(|m| m.fields.iter().find(|f| f.name == *field))
                    .map_or(field.as_str(), |f| f.column.as_str());
                (column_name(column), ATOM)
            }
            NodeKind::EnumMember { value, .. } => match value {
                serde_json::Value::String(label) => (quote_literal(label), ATOM),
                other => (other.to_string(), ATOM),
            },
            NodeKind::Call { function, args } => {
                let Some(template) = function.sql(self.dialect) else {
                    let dialect = if self.sqlite() { "SQLite" } else { "Postgres" };
                    return Err(fail(format!("`{}` has no {dialect} form; use sql(\"...\")", function.name)));
                };
                let args = args.iter().map(|a| Ok(self.node(a)?.0)).collect::<Result<Vec<_>, Diagnostic>>()?;
                (expand(template, &args), ATOM)
            }
            NodeKind::Sql(text) => (format!("({text})"), ATOM),
            NodeKind::Scope(_) | NodeKind::ParentDefaultFilter => {
                return Err(fail("this name only exists in a default filter".into()));
            }
            NodeKind::Method { receiver, method, args } => self.method(node, receiver, *method, args)?,
            NodeKind::Binary(op, l, r) => {
                let (symbol, prec) = match op {
                    BinOp::Or => ("OR", OR),
                    BinOp::And => ("AND", AND),
                    BinOp::Eq => ("=", COMPARE),
                    BinOp::Ne => ("<>", COMPARE),
                    BinOp::Lt => ("<", COMPARE),
                    BinOp::Le => ("<=", COMPARE),
                    BinOp::Gt => (">", COMPARE),
                    BinOp::Ge => (">=", COMPARE),
                    BinOp::Add => ("+", SUM),
                    BinOp::Sub => ("-", SUM),
                    BinOp::Mul => ("*", PRODUCT),
                    BinOp::Div => ("/", PRODUCT),
                };
                // A comparison does not chain, so both of its sides must bind tighter.
                let left_min = if prec == COMPARE { COMPARE + 1 } else { prec };
                (format!("{} {symbol} {}", self.at_least(l, left_min)?, self.at_least(r, prec + 1)?), prec)
            }
            NodeKind::Not(operand) => (format!("NOT {}", self.at_least(operand, ATOM)?), NOT),
            NodeKind::Neg(operand) => {
                let inner = self.at_least(operand, UNARY)?;
                // `--` starts a comment.
                (if inner.starts_with('-') { format!("-({inner})") } else { format!("-{inner}") }, UNARY)
            }
            NodeKind::IsNull { operand, negated } => {
                (format!("{} IS {}NULL", self.at_least(operand, COMPARE + 1)?, if *negated { "NOT " } else { "" }), COMPARE)
            }
            NodeKind::Key { inner, .. } => self.node(inner)?,
        })
    }

    /// The text of `node`, in parentheses when it binds looser than `min`.
    fn at_least(&self, node: &Node, min: u8) -> Result<String, Diagnostic> {
        let (text, prec) = self.node(node)?;
        Ok(if prec < min { format!("({text})") } else { text })
    }

    fn method(&self, node: &Node, receiver: &Node, method: Method, args: &[Node]) -> Result<Rendered, Diagnostic> {
        let receiver_sql = self.at_least(receiver, COMPARE + 1)?;
        let sqlite = self.sqlite();
        Ok(match method {
            Method::In | Method::NotIn => {
                let items = args.iter().map(|a| Ok(self.node(a)?.0)).collect::<Result<Vec<_>, Diagnostic>>()?;
                if items.is_empty() {
                    return Err(Diagnostic { span: node.span, message: "`in_` needs at least one value".into() });
                }
                (format!("{receiver_sql} {}IN ({})", if method == Method::NotIn { "NOT " } else { "" }, items.join(", ")), COMPARE)
            }
            Method::Between => {
                (format!("{receiver_sql} BETWEEN {} AND {}", self.at_least(&args[0], SUM)?, self.at_least(&args[1], SUM)?), COMPARE)
            }
            Method::Like if sqlite => {
                return Err(Diagnostic {
                    span: node.span,
                    message: "`like` depends on `PRAGMA case_sensitive_like` in SQLite; use sql(\"...\") or `contains`, `startswith`, `endswith`".into(),
                });
            }
            Method::Like => (format!("{receiver_sql} LIKE {}", self.at_least(&args[0], COMPARE + 1)?), COMPARE),
            // Postgres reads `\` as the escape character; SQLite has none unless it is named.
            Method::ILike if sqlite => (format!("LOWER({receiver_sql}) LIKE LOWER({}) ESCAPE '\\'", self.node(&args[0])?.0), COMPARE),
            Method::ILike => (format!("{receiver_sql} ILIKE {}", self.at_least(&args[0], COMPARE + 1)?), COMPARE),
            Method::Contains => {
                let find = if sqlite { "instr" } else { "strpos" };
                (format!("{find}({receiver_sql}, {}) > 0", self.node(&args[0])?.0), COMPARE)
            }
            Method::StartsWith => {
                let pattern = self.node(&args[0])?.0;
                (format!("substr({receiver_sql}, 1, length({pattern})) = {pattern}"), COMPARE)
            }
            Method::EndsWith => {
                let pattern = self.node(&args[0])?.0;
                // SQLite `substr(x, -0)` is the whole text, so an empty suffix needs a computed start.
                let tail = if sqlite {
                    format!("substr({receiver_sql}, length({receiver_sql}) - length({pattern}) + 1)")
                } else {
                    format!("right({receiver_sql}, length({pattern}))")
                };
                (format!("{tail} = {pattern}"), COMPARE)
            }
            Method::IContains => {
                let pattern = match &args[0].kind {
                    NodeKind::Str(text) => quote_literal(&format!("%{}%", like_escape(text))),
                    _ => format!("'%' || {} || '%'", like_escape_sql(self.node(&args[0])?.0)),
                };
                if sqlite {
                    (format!("LOWER({receiver_sql}) LIKE LOWER({pattern}) ESCAPE '\\'"), COMPARE)
                } else {
                    (format!("{receiver_sql} ILIKE {pattern}"), COMPARE)
                }
            }
        })
    }
}

/// Words a column name must not use unquoted: the SQL keywords of Postgres and SQLite.
const RESERVED: &[&str] = &[
    "abort", "all", "analyse", "analyze", "and", "any", "array", "as", "asc", "asymmetric", "attach", "authorization", "begin", "between", "binary",
    "both", "by", "case", "cast", "check", "collate", "collation", "column", "commit", "concurrently", "conflict", "constraint", "create", "cross",
    "current_catalog", "current_date", "current_role", "current_schema", "current_time", "current_timestamp", "current_user", "database", "default",
    "deferrable", "deferred", "delete", "desc", "detach", "distinct", "do", "drop", "each", "else", "end", "escape", "except", "exclusive", "exists",
    "explain", "fail", "false", "fetch", "for", "foreign", "freeze", "from", "full", "glob", "grant", "group", "having", "if", "ignore", "ilike",
    "immediate", "in", "index", "indexed", "initially", "inner", "insert", "instead", "intersect", "into", "is", "isnull", "join", "key", "lateral",
    "leading", "left", "like", "limit", "localtime", "localtimestamp", "match", "natural", "no", "not", "notnull", "null", "of", "offset", "on", "only",
    "or", "order", "outer", "overlaps", "placing", "plan", "pragma", "primary", "query", "raise", "recursive", "references", "regexp", "reindex",
    "release", "rename", "replace", "restrict", "returning", "right", "rollback", "row", "rows", "savepoint", "select", "session_user", "set", "similar",
    "some", "symmetric", "system_user", "table", "tablesample", "temp", "temporary", "then", "to", "trailing", "transaction", "trigger", "true", "union",
    "unique", "update", "user", "using", "vacuum", "values", "variadic", "verbose", "view", "virtual", "when", "where", "window", "with",
];

#[cfg(test)]
mod tests;
