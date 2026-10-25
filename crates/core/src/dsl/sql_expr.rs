//! Conservative, fully consumed SQL expressions. This is an input adapter to the
//! schema expression checker, never a SQL equivalence solver.
use super::expr::{self, Context, Node, NodeKind, Span};
use crate::{dialect::Dialect, ir::SchemaIr};

type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, PartialEq)]
enum Token { Word(String), Ident(String), String(String), Number(String), Op(String) }

fn tokens(sql: &str, dialect: Dialect) -> Result<Vec<Token>> {
    let mut chars = sql.chars().peekable();
    let mut out = vec![];
    while let Some(c) = chars.next() {
        if c.is_whitespace() { continue; }
        if c == '\'' || c == '"' || (c == '`' && dialect == Dialect::Sqlite) {
            let mut value = String::new();
            loop {
                let ch = chars.next().ok_or("unterminated SQL quote")?;
                if ch == c {
                    if chars.peek() == Some(&c) { chars.next(); value.push(c); }
                    else { break; }
                } else { value.push(ch); }
            }
            out.push(if c == '\'' { Token::String(value) } else { Token::Ident(value) });
        } else if c.is_ascii_alphabetic() || c == '_' {
            let mut word = String::from(c);
            while chars.peek().is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_') { word.push(chars.next().unwrap()); }
            out.push(Token::Word(word));
        } else if c.is_ascii_digit() {
            let mut number = String::from(c);
            while chars.peek().is_some_and(|c| c.is_ascii_digit()) { number.push(chars.next().unwrap()); }
            if chars.peek() == Some(&'.') {
                number.push(chars.next().unwrap());
                while chars.peek().is_some_and(|c| c.is_ascii_digit()) { number.push(chars.next().unwrap()); }
            }
            if chars.peek().is_some_and(|c| *c == 'e' || *c == 'E') {
                number.push(chars.next().unwrap());
                if chars.peek().is_some_and(|c| *c == '+' || *c == '-') { number.push(chars.next().unwrap()); }
                while chars.peek().is_some_and(|c| c.is_ascii_digit()) { number.push(chars.next().unwrap()); }
            }
            out.push(Token::Number(number));
        } else if (c == '-' && chars.peek() == Some(&'-')) || (c == '/' && chars.peek() == Some(&'*')) {
            return Err("SQL comments are outside the conversion subset".into());
        } else if "()=<>!+-*/,".contains(c) {
            let mut op = String::from(c);
            if matches!(c, '<' | '>' | '!') && chars.peek().is_some_and(|n| *n == '=' || c == '<' && *n == '>') { op.push(chars.next().unwrap()); }
            if op == "!" { return Err("unsupported SQL operator !".into()); }
            out.push(Token::Op(op));
        } else { return Err(format!("unsupported SQL character {c:?}")); }
    }
    Ok(out)
}

struct Parser<'a> { tokens: Vec<Token>, at: usize, ir: &'a SchemaIr, model: &'a str }
impl Parser<'_> {
    fn take(&mut self, text: &str) -> bool {
        if self.tokens.get(self.at).is_some_and(|t| match t { Token::Word(w) => w.eq_ignore_ascii_case(text), Token::Op(o) => o == text, _ => false }) {
            self.at += 1; true
        } else { false }
    }
    fn need(&mut self, text: &str) -> Result<()> { if self.take(text) { Ok(()) } else { Err(format!("expected SQL {text}")) } }
    fn list(&mut self) -> Result<Vec<String>> {
        self.need("(")?;
        let mut args = vec![];
        if !self.take(")") {
            loop {
                args.push(self.expression(0)?);
                if self.take(")") { break; }
                self.need(",")?;
            }
        }
        Ok(args)
    }
    fn atom(&mut self) -> Result<String> {
        if self.take("(") { let expr = self.expression(0)?; self.need(")")?; return Ok(format!("({expr})")); }
        if self.take("NOT") {
            let value = self.expression(3)?;
            let value = if self.ir.dialect == Dialect::Sqlite { boolean_literal(value) } else { value };
            return Ok(format!("~({value})"));
        }
        if self.take("-") { return Ok(format!("-({})", self.expression(9)?)); }
        // Unary plus is deliberately unsupported; it can affect SQLite affinity.
        let token = self.tokens.get(self.at).cloned().ok_or("expected SQL expression")?;
        self.at += 1;
        let unquoted = matches!(token, Token::Word(_));
        match token {
            Token::String(s) => Ok(serde_json::to_string(&s).unwrap()),
            Token::Number(n) => Ok(n),
            Token::Word(ref w) if w.eq_ignore_ascii_case("NULL") => Ok("null".into()),
            Token::Word(ref w) if self.ir.dialect == Dialect::Sqlite
                && (w.eq_ignore_ascii_case("TRUE") || w.eq_ignore_ascii_case("FALSE"))
                && self.ir.models.iter().find(|m| m.name == self.model).is_some_and(|m| m.fields.iter().any(|f| f.column.eq_ignore_ascii_case(w))) => {
                Err("ambiguous SQLite boolean literal/column".into())
            }
            Token::Word(ref w) if w.eq_ignore_ascii_case("TRUE") => Ok("true".into()),
            Token::Word(ref w) if w.eq_ignore_ascii_case("FALSE") => Ok("false".into()),
            Token::Word(w) | Token::Ident(w) => {
                if self.tokens.get(self.at) == Some(&Token::Op("(".into())) {
                    let function = expr::FUNCTIONS.iter().find(|f| {
                        // Only direct catalog calls: no invented reversal of complex SQL templates.
                        f.sql(self.ir.dialect).and_then(|s| s.split_once('(')).is_some_and(|(name, _)| if unquoted { name.eq_ignore_ascii_case(&w) } else { name == w })
                    }).ok_or_else(|| format!("unsupported SQL function {w}"))?;
                    let args = self.list()?;
                    return Ok(format!("{}({})", function.name, args.join(", ")));
                }
                let m = self.ir.models.iter().find(|m| m.name == self.model).ok_or("unknown model")?;
                let physical = if unquoted && self.ir.dialect == Dialect::Postgres { w.to_ascii_lowercase() } else { w };
                let fields: Vec<_> = m.fields.iter().filter(|f| {
                    if self.ir.dialect == Dialect::Sqlite { f.column.eq_ignore_ascii_case(&physical) } else { f.column == physical }
                }).collect();
                if fields.len() != 1 { return Err(format!("unresolved or ambiguous column {physical}")); }
                let f = fields[0];
                Ok(f.name.clone())
            }
            _ => Err("unsupported SQL operand".into()),
        }
    }
    fn boolean(&self, value: &str) -> bool {
        expr::check_text(self.ir, self.model, Context::Check, value).is_ok()
    }
    fn expression(&mut self, min: u8) -> Result<String> {
        let postgres = self.ir.dialect == Dialect::Postgres;
        let comparison = 5;
        let membership = if postgres { 6 } else { 4 };
        let is_null = if postgres { 4 } else { comparison };
        let mut left = self.atom()?;
        loop {
            let saved = self.at;
            if min <= is_null && self.take("IS") {
                let negated = self.take("NOT");
                self.need("NULL")?;
                left = format!("({left} {} null)", if negated { "!=" } else { "==" });
                continue;
            }
            if min <= membership {
                let negated = self.take("NOT");
                if self.take("IN") {
                    let args = self.list()?;
                    if args.is_empty() { return Err("empty SQL IN list".into()); }
                    left = format!("({left}).{}([{}])", if negated { "not_in" } else { "in_" }, args.join(", "));
                    continue;
                }
                if self.take("BETWEEN") {
                    if negated { return Err("NOT BETWEEN is not in the conversion subset".into()); }
                    let low = self.expression(membership + 1)?;
                    self.need("AND")?;
                    let high = self.expression(membership + 1)?;
                    left = format!("({left}).between({low}, {high})");
                    continue;
                }
                self.at = saved;
            }
            let Some(token) = self.tokens.get(self.at) else { break; };
            let op = match token { Token::Op(o) => o.as_str(), Token::Word(w) => w.as_str(), _ => break };
            let (precedence, native) = match op.to_ascii_uppercase().as_str() {
                "OR" => (1, "|"), "AND" => (2, "&"),
                "=" | "==" => (comparison, "=="), "!=" | "<>" => (comparison, "!="),
                "<" => (comparison, "<"), "<=" => (comparison, "<="), ">" => (comparison, ">"), ">=" => (comparison, ">="),
                "+" => (7, "+"), "-" => (7, "-"), "*" => (8, "*"), "/" => (8, "/"), _ => break,
            };
            if precedence < min { break; }
            self.at += 1;
            let mut right = self.expression(precedence + 1)?;
            // SQL `= NULL` is UNKNOWN, whereas native `== null` is IS NULL.
            if precedence == comparison && (left.trim_matches([' ', '(', ')']) == "null" || right.trim_matches([' ', '(', ')']) == "null") { return Err("SQL comparison with NULL is not a null test".into()); }
            if precedence <= 2 && self.ir.dialect == Dialect::Sqlite {
                left = boolean_literal(left);
                right = boolean_literal(right);
            }
            if precedence == comparison && self.ir.dialect == Dialect::Sqlite {
                // The existing SQLite renderer lowers boolean literals to 0/1.
                // Recognize only those exact values with a checked boolean peer.
                if self.boolean(&left) { right = boolean_literal(right); }
                if self.boolean(&right) { left = boolean_literal(left); }
            }
            left = format!("({left} {native} {right})");
        }
        Ok(left)
    }
}

fn boolean_literal(value: String) -> String {
    match value.trim_matches([' ', '(', ')']) {
        "0" => "false".into(), "1" => "true".into(), _ => value,
    }
}

fn parse_native(ir: &SchemaIr, model: &str, sql: &str) -> Result<String> {
    let mut parser = Parser { tokens: tokens(sql, ir.dialect)?, at: 0, ir, model };
    let native = parser.expression(0)?;
    if parser.at != parser.tokens.len() { return Err("unsupported trailing SQL syntax".into()); }
    Ok(native)
}

/// Converts a whole SQL expression and checks it in its destination context.
/// Unknown SQL, unresolved columns and type/context errors are ordinary fallbacks.
pub fn translate(ir: &SchemaIr, model: &str, context: Context, sql: &str) -> Result<String> {
    let native = parse_native(ir, model, sql)?;
    let native = if ir.dialect == Dialect::Sqlite && matches!(context, Context::Check | Context::IndexFilter) { boolean_literal(native) } else { native };
    let mut checked = expr::check_text(ir, model, context, &native).map_err(|e| e.message)?.root;
    // Native rendering can transform typed literals (notably SQLite dates and
    // UUIDs). Accept a conversion only when the exact supported lowering parses
    // back to the same typed structure, including every literal's contents.
    let rendered = super::render::render(ir, ir.dialect, &checked).map_err(|e| e.message)?;
    let round_trip = parse_native(ir, model, &rendered)?;
    let round_trip = if ir.dialect == Dialect::Sqlite && matches!(context, Context::Check | Context::IndexFilter) { boolean_literal(round_trip) } else { round_trip };
    let mut again = expr::check_text(ir, model, context, &round_trip).map_err(|e| e.message)?.root;
    unlocate(&mut checked);
    unlocate(&mut again);
    if checked != again { return Err("native rendering changes typed literal contents".into()); }
    Ok(native)
}

fn unlocate(node: &mut Node) {
    node.span = Span::default();
    match &mut node.kind {
        NodeKind::Field { spans, .. } => spans.clear(),
        NodeKind::Call { args, .. } => args.iter_mut().for_each(unlocate),
        NodeKind::Method { receiver, args, .. } => { unlocate(receiver); args.iter_mut().for_each(unlocate); }
        NodeKind::Binary(_, a, b) => { unlocate(a); unlocate(b); }
        NodeKind::Not(a) | NodeKind::Neg(a) | NodeKind::IsNull { operand: a, .. } | NodeKind::Key { inner: a, .. } => unlocate(a),
        _ => {}
    }
}

/// Typed structural equality, preserving operand order and literal contents.
pub(crate) fn equivalent(ir: &SchemaIr, model: &str, context: Context, a: &str, b: &str) -> bool {
    let checked = |sql| -> Option<Node> {
        let native = translate(ir, model, context, sql).ok()?;
        let mut root = expr::check_text(ir, model, context, &native).ok()?.root;
        unlocate(&mut root);
        Some(root)
    };
    match (checked(a), checked(b)) { (Some(a), Some(b)) => a == b, _ => false }
}
