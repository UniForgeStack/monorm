//! Arguments of kind `expression`: a schema expression in an extension attribute.
//!
//! The parser keeps the syntax of such an argument as a marker (`marker`). `resolve` runs once
//! the model shape is final: it checks every marker against its model in the default-filter
//! context and writes the engine filter form (JSON) back, so an extension reads a checked tree.
//! A string is the old filter language: it is read into the same tree, with a warning.

use serde_json::{json, Value as Json};

use super::expr::{self, Context, Diagnostic, Method, Node, NodeKind, Span, Warning};
use super::syntax::{BinOp, Path, Pos, Value};
use crate::behavior::Manifest;
use crate::ir::SchemaIr;
use monorm::extension::{ArgumentKind, AttributeTarget, Declaration};

const KEY: &str = "$expression";

/// The engine form of `parent.default_filter` until an extension puts the inherited filter in its place.
pub const PARENT_PLACEHOLDER: &str = "parent_default_filter";

/// The syntax of an expression argument, for `resolve`.
pub fn marker(pos: Pos, value: &Value) -> Json {
    match value {
        Value::Str(text) => json!({ KEY: { "pos": pos, "text": text } }),
        other => json!({ KEY: { "pos": pos, "value": other } }),
    }
}

/// Records the file of a declaration and the first line of that file in the combined source.
pub fn stamp(declaration: &mut Declaration, file: &str, base: u32) {
    let values = declaration.arguments.values_mut().chain(declaration.positional.iter_mut());
    for value in values {
        if let Some(inner) = value.get_mut(KEY).and_then(Json::as_object_mut) {
            inner.insert("file".into(), json!(file));
            inner.insert("base".into(), json!(base));
        }
    }
}

enum Slot {
    Named(String),
    Positional(usize),
}

/// Checks each expression argument of the declarations that no pass has lowered yet.
pub fn resolve(ir: &mut SchemaIr, manifests: &[Manifest], warnings: &mut Vec<Warning>) -> Result<(), String> {
    let mut resolved: Vec<(usize, Slot, Json)> = vec![];
    for (i, d) in ir.behavior.declarations.iter().enumerate().filter(|(_, d)| !d.lowered) {
        let named = || manifests.iter().flat_map(|m| m.attributes.iter()).filter(|a| a.name == d.attribute);
        let found = named().find(|a| (a.target == AttributeTarget::Field) == d.field.is_some()).or_else(|| named().next());
        let Some(attribute) = found else { continue };
        let is_expression = |kind: Option<ArgumentKind>| kind == Some(ArgumentKind::Expression);
        for (name, value) in &d.arguments {
            if is_expression(attribute.arguments.get(name).map(|a| a.kind)) {
                resolved.push((i, Slot::Named(name.clone()), one(ir, d, value, warnings)?));
            }
        }
        for (at, value) in d.positional.iter().enumerate() {
            if is_expression(attribute.positional_at(at).map(|a| a.kind)) {
                resolved.push((i, Slot::Positional(at), one(ir, d, value, warnings)?));
            }
        }
    }
    for (i, slot, value) in resolved {
        let d = &mut ir.behavior.declarations[i];
        match slot {
            Slot::Named(name) => { d.arguments.insert(name, value); }
            Slot::Positional(at) => d.positional[at] = value,
        }
    }
    Ok(())
}

/// A `parent.default_filter` that no extension has replaced is an error.
pub fn reject_placeholders(ir: &SchemaIr) -> Result<(), String> {
    fn has(value: &Json) -> bool {
        match value {
            Json::Object(map) => map.get("t").and_then(Json::as_str) == Some(PARENT_PLACEHOLDER) || map.values().any(has),
            Json::Array(items) => items.iter().any(has),
            _ => false,
        }
    }
    for d in &ir.behavior.read_defaults {
        if d.filter.as_ref().is_some_and(has) {
            return Err(format!("{}: parent.default_filter has no inherited filter", d.model));
        }
    }
    Ok(())
}

/// Where an expression is: the file, the first line of the file in the combined source, and the position of the value.
struct Origin {
    file: String,
    base: u32,
    pos: Pos,
}

impl Origin {
    fn span(&self, span: Span) -> Span {
        Span { line: (span.line + 1).saturating_sub(self.base).max(1), ..span }
    }

    fn error(&self, d: &Declaration, diagnostic: &Diagnostic) -> String {
        let prefix = if d.field.is_some() { "@" } else { "@@" };
        let span = self.span(diagnostic.span);
        format!("{}:{}:{}: {prefix}{}: {}", self.file, span.line, span.column, d.attribute, diagnostic.message)
    }
}

fn one(ir: &SchemaIr, d: &Declaration, value: &Json, warnings: &mut Vec<Warning>) -> Result<Json, String> {
    let own = |pos: Pos| Origin { file: d.location.file.clone(), base: 1, pos };
    let (origin, text, syntax) = match value {
        Json::Null => return Ok(Json::Null),
        // The tree of an earlier resolution, or IR that another tool wrote.
        Json::Object(map) if !map.contains_key(KEY) => return Ok(value.clone()),
        Json::Object(map) => {
            let inner = map[KEY].as_object().ok_or_else(|| d.error_with("@", "an expression argument is malformed"))?;
            let pos: Pos = serde_json::from_value(inner["pos"].clone()).map_err(|e| d.error_with("@", e))?;
            let file = inner.get("file").and_then(Json::as_str).map_or_else(|| d.location.file.clone(), str::to_owned);
            let base = inner.get("base").and_then(Json::as_u64).map_or(1, |b| b as u32);
            let origin = Origin { file, base, pos };
            match inner.get("text").and_then(Json::as_str) {
                Some(text) => (origin, Some(text.to_owned()), None),
                None => {
                    let value: Value = serde_json::from_value(inner["value"].clone()).map_err(|e| d.error_with("@", e))?;
                    (origin, None, Some(value))
                }
            }
        }
        Json::String(text) => (own(Pos { line: d.location.line, col: d.location.column, len: 0 }), Some(text.clone()), None),
        _ => return Err(d.error_with("@", "expects an expression: write a condition, or \"none\" to clear it")),
    };
    let model = &d.model;
    let (pos, syntax, old) = match (text, syntax) {
        (Some(text), _) => {
            if text == "none" { return Ok(Json::Null); }
            match old_form(&text, origin.pos) {
                Ok((pos, value)) => (pos, value, true),
                Err((pos, message)) => return Err(origin.error(d, &Diagnostic { span: Span { line: pos.line, column: pos.col, len: pos.len }, message })),
            }
        }
        (None, Some(value)) => (origin.pos, value, false),
        (None, None) => unreachable!("a marker has text or a value"),
    };
    let checked = expr::check(ir, model, Context::DefaultFilter, pos, &syntax).map_err(|e| origin.error(d, &e))?;
    let tree = engine(&checked.root).map_err(|e| origin.error(d, &e))?;
    for w in checked.warnings {
        warnings.push(Warning { file: origin.file.clone(), span: origin.span(w.span), message: w.message });
    }
    if old {
        let span = origin.span(Span { line: origin.pos.line, column: origin.pos.col, len: origin.pos.len });
        warnings.push(Warning {
            file: origin.file.clone(),
            span,
            message: format!("this filter is a string; write the expression: {}", native(&checked.root)),
        });
    }
    Ok(tree)
}

// ---- The engine form -------------------------------------------------------

fn fail<T>(node: &Node, message: impl Into<String>) -> Result<T, Diagnostic> {
    Err(Diagnostic { span: node.span, message: message.into() })
}

fn col(via: &[String], name: &str) -> Json {
    json!({"t": "col", "path": via, "name": name})
}

/// The engine `Expr` of a checked default filter. `parent.default_filter` stays a placeholder.
fn engine(node: &Node) -> Result<Json, Diagnostic> {
    let e = |n: &Node| engine(n);
    Ok(match &node.kind {
        NodeKind::Null => json!({"t": "null"}),
        NodeKind::Bool(b) => json!({"t": "const", "value": b}),
        NodeKind::Int(n) => json!({"t": "int", "value": n}),
        NodeKind::Str(s) => json!({"t": "text", "value": s}),
        NodeKind::Float(_) => return fail(node, "a number with a fraction is not available in a default filter"),
        NodeKind::Field { via, field, .. } => col(via, field),
        NodeKind::EnumMember { value, .. } => match value {
            Json::String(s) => json!({"t": "text", "value": s}),
            Json::Number(n) => json!({"t": "int", "value": n}),
            _ => return fail(node, "this enum member has no stored value"),
        },
        NodeKind::Call { function, args } => {
            let name = if function.name == "char_length" { "length" } else { function.name };
            let args = args.iter().map(e).collect::<Result<Vec<_>, _>>()?;
            json!({"t": "func", "name": name, "args": args})
        }
        NodeKind::Sql(_) => return fail(node, "sql(...) is not available in a default filter"),
        NodeKind::Scope(name) => json!({"t": "scope", "name": name}),
        NodeKind::ParentDefaultFilter => json!({"t": PARENT_PLACEHOLDER}),
        NodeKind::Method { receiver, method, args } => {
            let item = e(receiver)?;
            let literal = |at: usize| -> Result<String, Diagnostic> {
                match args.get(at).map(|a| &a.kind) {
                    Some(NodeKind::Str(text)) => Ok(text.clone()),
                    _ => fail(node, format!("{}(...) in a default filter takes a text literal", method.name())),
                }
            };
            match method {
                Method::In | Method::NotIn => {
                    let values = args.iter().map(e).collect::<Result<Vec<_>, _>>()?;
                    json!({"t": "in", "item": item, "values": values, "neg": *method == Method::NotIn})
                }
                Method::Between => {
                    let (low, high) = (e(&args[0])?, e(&args[1])?);
                    json!({"t": "and", "items": [
                        {"t": "cmp", "op": "ge", "l": item.clone(), "r": low},
                        {"t": "cmp", "op": "le", "l": item, "r": high},
                    ]})
                }
                Method::Like | Method::ILike => {
                    let pattern = json!({"t": "text", "value": literal(0)?});
                    json!({"t": "like", "item": item, "pattern": pattern, "ci": *method == Method::ILike, "neg": false})
                }
                Method::Contains | Method::IContains | Method::StartsWith | Method::EndsWith => {
                    let text = like_escape(&literal(0)?);
                    let pattern = match method {
                        Method::StartsWith => format!("{text}%"),
                        Method::EndsWith => format!("%{text}"),
                        _ => format!("%{text}%"),
                    };
                    json!({"t": "like", "item": item, "pattern": {"t": "text", "value": pattern}, "ci": *method == Method::IContains, "neg": false})
                }
            }
        }
        NodeKind::Binary(op, l, r) => {
            let (l, r) = (e(l)?, e(r)?);
            let cmp = |op: &str| json!({"t": "cmp", "op": op, "l": l, "r": r});
            let arith = |op: &str| json!({"t": "arith", "op": op, "l": l, "r": r});
            match op {
                BinOp::Or => json!({"t": "or", "items": [l, r]}),
                BinOp::And => json!({"t": "and", "items": [l, r]}),
                BinOp::Eq => cmp("eq"),
                BinOp::Ne => cmp("ne"),
                BinOp::Lt => cmp("lt"),
                BinOp::Le => cmp("le"),
                BinOp::Gt => cmp("gt"),
                BinOp::Ge => cmp("ge"),
                BinOp::Add => arith("add"),
                BinOp::Sub => arith("sub"),
                BinOp::Mul => arith("mul"),
                BinOp::Div => arith("div"),
            }
        }
        NodeKind::Not(item) => json!({"t": "not", "item": e(item)?}),
        NodeKind::Neg(item) => match &item.kind {
            NodeKind::Int(n) => json!({"t": "int", "value": -n}),
            _ => json!({"t": "arith", "op": "sub", "l": {"t": "int", "value": 0}, "r": e(item)?}),
        },
        NodeKind::Key { .. } => return fail(node, "an index key is not a filter"),
        NodeKind::IsNull { operand, negated } => json!({"t": "is_null", "item": e(operand)?, "neg": negated}),
    })
}

/// A text that the `LIKE` of the engine reads as itself: `\` is its escape character.
fn like_escape(text: &str) -> String {
    ['\\', '%', '_'].iter().fold(text.to_owned(), |text, c| text.replace(*c, &format!("\\{c}")))
}

/// The native form of a checked filter, for a warning: every binary operator in parentheses, the outer pair dropped.
fn native(node: &Node) -> String {
    let text = render(node);
    match (&node.kind, text.strip_prefix('(').and_then(|t| t.strip_suffix(')'))) {
        (NodeKind::Binary(..) | NodeKind::IsNull { .. }, Some(inner)) => inner.to_owned(),
        _ => text,
    }
}

fn render(node: &Node) -> String {
    match &node.kind {
        NodeKind::Str(text) => serde_json::to_string(text).unwrap_or_default(),
        NodeKind::Binary(op, l, r) => format!("({} {} {})", render(l), op.symbol(), render(r)),
        NodeKind::Not(item) => format!("~{}", render(item)),
        NodeKind::IsNull { operand, negated } => format!("({} {} null)", render(operand), if *negated { "!=" } else { "==" }),
        NodeKind::Call { function, args } => {
            let args: Vec<_> = args.iter().map(render).collect();
            format!("{}({})", function.name, args.join(", "))
        }
        NodeKind::Method { receiver, method, args } => {
            let args: Vec<_> = args.iter().map(render).collect();
            let list = matches!(method, Method::In | Method::NotIn);
            let args = if list { format!("[{}]", args.join(", ")) } else { args.join(", ") };
            format!("{}.{}({args})", render(receiver), method.name())
        }
        _ => node.to_string(),
    }
}

// ---- The old filter language ------------------------------------------------

struct Token {
    text: String,
    pos: Pos,
}

type OldError = (Pos, String);

/// `and`, `or`, `not`, `&&`, `||`, `!`, comparisons, names, integers, quoted strings,
/// `scope.<name>`, `parent.default_filter`, `null`, `true` and `false`, read into the syntax of the native language.
fn old_form(source: &str, at: Pos) -> Result<(Pos, Value), OldError> {
    let chars: Vec<char> = source.chars().collect();
    let place = |from: usize, to: usize| Pos { line: at.line, col: at.col + 1 + from as u32, len: (to - from) as u32 };
    let mut tokens: Vec<Token> = vec![];
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() { i += 1; continue; }
        let start = i;
        i += 1;
        if c == '"' {
            let (mut escaped, mut closed) = (false, false);
            while i < chars.len() {
                let d = chars[i];
                i += 1;
                if d == '"' && !escaped { closed = true; break; }
                escaped = d == '\\' && !escaped;
            }
            if !closed { return Err((place(start, chars.len()), "unterminated filter string".into())); }
        } else if c.is_alphanumeric() || c == '_' || c == '-' {
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.') { i += 1; }
        } else if matches!(c, '=' | '!' | '<' | '>' | '&' | '|') && i < chars.len() && (chars[i] == '=' || chars[i] == c) {
            i += 1;
        }
        tokens.push(Token { text: chars[start..i].iter().collect(), pos: place(start, i) });
    }
    if let Some(t) = tokens.iter().filter(|t| !t.text.starts_with('"')).find(|t| t.text.split('.').any(|s| matches!(s, "or_null" | "and_not_null"))) {
        let name = t.text.split('.').find(|s| matches!(*s, "or_null" | "and_not_null")).unwrap_or_default();
        return Err((t.pos, format!("{name}() is not available in a default filter; write the NULL test as a condition, for example `(x > 1 or x == null)`")));
    }
    let mut parser = Old { tokens, at: 0, end: place(chars.len(), chars.len()) };
    let value = parser.boolean(0)?;
    if let Some(t) = parser.tokens.get(parser.at) {
        return Err((t.pos, format!("unexpected filter token {}", t.text)));
    }
    Ok(value)
}

struct Old {
    tokens: Vec<Token>,
    at: usize,
    end: Pos,
}

type Operand = (Pos, Value);

impl Old {
    fn peek(&self) -> &str {
        self.tokens.get(self.at).map_or("", |t| t.text.as_str())
    }

    fn take(&mut self) -> Result<(String, Pos), OldError> {
        let token = self.tokens.get(self.at).ok_or((self.end, "incomplete default filter".to_owned()))?;
        self.at += 1;
        Ok((token.text.clone(), token.pos))
    }

    fn boolean(&mut self, min: u8) -> Result<Operand, OldError> {
        let mut left = self.unary()?;
        loop {
            let (precedence, op) = match self.peek() { "or" | "||" => (1, BinOp::Or), "and" | "&&" => (2, BinOp::And), _ => break };
            if precedence < min { break; }
            let (_, op_pos) = self.take()?;
            let right = self.boolean(precedence + 1)?;
            left = (left.0, Value::Binary(op, op_pos, Box::new(left), Box::new(right)));
        }
        Ok(left)
    }

    /// `not` binds looser than comparisons, as in Python and TypeScript.
    fn unary(&mut self) -> Result<Operand, OldError> {
        if matches!(self.peek(), "!" | "not") {
            let (_, pos) = self.take()?;
            let operand = self.unary()?;
            return Ok((pos, Value::Not(pos, Box::new(operand))));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Operand, OldError> {
        let left = self.atom()?;
        let op = match self.peek() {
            "==" => BinOp::Eq, "!=" => BinOp::Ne, "<" => BinOp::Lt, "<=" => BinOp::Le, ">" => BinOp::Gt, ">=" => BinOp::Ge,
            _ => return Ok(left),
        };
        let (_, op_pos) = self.take()?;
        let right = self.atom()?;
        Ok((left.0, Value::Binary(op, op_pos, Box::new(left), Box::new(right))))
    }

    fn atom(&mut self) -> Result<Operand, OldError> {
        let (token, pos) = self.take()?;
        let name = |names: &[&str]| Value::Path(Path::new(names.iter().map(|s| (*s).to_owned()).collect()), None);
        Ok(match token.as_str() {
            "(" => {
                let value = self.boolean(0)?;
                if self.take()?.0 != ")" { return Err((pos, "expected ')'".into())); }
                value
            }
            "parent.default_filter" => (pos, name(&["parent", "default_filter"])),
            _ if token.starts_with("scope.") => {
                let scope = &token["scope.".len()..];
                if scope.is_empty() || !scope.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    return Err((pos, format!("invalid scope value {token}: write scope.<name>")));
                }
                (pos, name(&["scope", scope]))
            }
            "null" => (pos, name(&["null"])),
            "true" | "false" => (pos, Value::Bool(token == "true")),
            _ if token.starts_with('"') => (pos, Value::Str(serde_json::from_str::<String>(&token).map_err(|e| (pos, e.to_string()))?)),
            _ if token.parse::<i64>().is_ok() => (pos, Value::Num(token)),
            _ if token.chars().all(|c| c.is_alphanumeric() || c == '_') => (pos, name(&[&token])),
            _ => return Err((pos, format!("unsupported filter token {token}"))),
        })
    }
}
