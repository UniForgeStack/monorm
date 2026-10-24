//! Schema expressions: the shared parser output (`syntax::Value`) checked against a schema.
//!
//! `check_text` parses one expression and returns a typed tree (`Node`) or an `Error` with a
//! span. Every name keeps its span and the declaration it resolves to.

use std::fmt;

use super::lower::key_options;
use super::syntax::{self, Args, Pos, Value};
use crate::dialect::Dialect;
use crate::ir::{ColType, FieldIr, ModelIr, Nulls, RelKind, SchemaIr};

pub use super::lower::KeyOptions;
pub use super::syntax::BinOp;

/// Where an expression sits in a schema; it decides which names and functions are allowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    /// `@check` and `@@check`.
    Check,
    /// The `where:` of an index.
    IndexFilter,
    /// An index or exclusion key.
    IndexKey,
    /// A model default filter.
    DefaultFilter,
}

/// A place in the source: 1-based line and column, and the length in characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Span {
    pub line: u32,
    pub column: u32,
    pub len: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Ty {
    Scalar(ColType),
    Array(ColType),
    Enum(String),
    Null,
    /// The type of `sql("...")`: it matches every type.
    Unknown,
}

/// How a function may be used: an index takes only `Immutable`, a check takes no `Volatile`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Volatility {
    Immutable,
    Stable,
    Volatile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Param {
    Text,
    Number,
    /// Any type; every later `Same` parameter takes the type of the first.
    Same,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Params {
    Fixed(&'static [Param]),
    /// One parameter, given one or more times.
    Variadic(Param),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Returns {
    /// The type of the first argument.
    First,
    Fixed(ColType),
}

/// A function the schema language knows. `postgres` and `sqlite` are SQL templates: `{0}` is
/// the first argument, `{1}` the second, `{*}` all of them joined by `, `. A dialect without a
/// template has no such function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Function {
    pub name: &'static str,
    pub params: Params,
    pub returns: Returns,
    pub volatility: Volatility,
    pub postgres: Option<&'static str>,
    pub sqlite: Option<&'static str>,
}

impl Function {
    pub fn sql(&self, dialect: Dialect) -> Option<&'static str> {
        match dialect {
            Dialect::Postgres => self.postgres,
            Dialect::Sqlite => self.sqlite,
        }
    }
}

const fn function_of(
    name: &'static str,
    params: Params,
    returns: Returns,
    volatility: Volatility,
    postgres: Option<&'static str>,
    sqlite: Option<&'static str>,
) -> Function {
    Function { name, params, returns, volatility, postgres, sqlite }
}

const TEXT: Params = Params::Fixed(&[Param::Text]);

/// The functions a schema expression may call; anything else goes through `sql("...")`.
pub const FUNCTIONS: &[Function] = &[
    function_of("lower", TEXT, Returns::First, Volatility::Immutable, Some("lower({0})"), Some("lower({0})")),
    function_of("upper", TEXT, Returns::First, Volatility::Immutable, Some("upper({0})"), Some("upper({0})")),
    function_of("char_length", TEXT, Returns::Fixed(ColType::Int), Volatility::Immutable, Some("char_length({0})"), Some("length({0})")),
    function_of("coalesce", Params::Variadic(Param::Same), Returns::First, Volatility::Immutable, Some("coalesce({*})"), Some("coalesce({*})")),
    function_of("abs", Params::Fixed(&[Param::Number]), Returns::First, Volatility::Immutable, Some("abs({0})"), Some("abs({0})")),
    function_of("now", Params::Fixed(&[]), Returns::Fixed(ColType::DateTime), Volatility::Stable, Some("now()"), None),
];

pub fn function(name: &str) -> Option<&'static Function> {
    FUNCTIONS.iter().find(|f| f.name == name)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    In,
    NotIn,
    Between,
    Like,
    ILike,
    Contains,
    IContains,
    StartsWith,
    EndsWith,
}

impl Method {
    pub fn name(self) -> &'static str {
        match self {
            Method::In => "in_",
            Method::NotIn => "not_in",
            Method::Between => "between",
            Method::Like => "like",
            Method::ILike => "ilike",
            Method::Contains => "contains",
            Method::IContains => "icontains",
            Method::StartsWith => "startswith",
            Method::EndsWith => "endswith",
        }
    }

    fn from_name(name: &str) -> Option<Method> {
        use Method::*;
        [In, NotIn, Between, Like, ILike, Contains, IContains, StartsWith, EndsWith].into_iter().find(|m| m.name() == name)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum NodeKind {
    Null,
    Bool(bool),
    Int(i64),
    /// A number with a fraction or an exponent, as written.
    Float(String),
    Str(String),
    /// A field of `model`, reached through `via` (the relation names, outermost first).
    /// `spans` has the span of each name: the relations in `via`, then the field.
    Field { via: Vec<String>, model: String, field: String, spans: Vec<Span> },
    /// A member of an enum; `value` is what the database stores (the label, or the integer).
    EnumMember { enum_name: String, member: String, value: serde_json::Value },
    /// `name(args)`, a function of the catalog.
    Call { function: &'static Function, args: Vec<Node> },
    /// `sql("...")`: SQL text that is not checked. Its type is `Ty::Unknown`.
    Sql(String),
    /// `scope.name`: a value the caller supplies; only a default filter has it.
    Scope(String),
    /// `parent.default_filter`: the default filter of the model that holds the relation.
    ParentDefaultFilter,
    /// `receiver.method(args)`. `in_` and `not_in` hold the list items in `args`.
    Method { receiver: Box<Node>, method: Method, args: Vec<Node> },
    /// `l & r`, `l | r`, `l == r`, `l + r`, ...
    Binary(BinOp, Box<Node>, Box<Node>),
    Not(Box<Node>),
    Neg(Box<Node>),
    /// The root of an index or exclusion key: a field, `sql("...")` or an expression, with its options.
    Key { inner: Box<Node>, options: KeyOptions },
    /// `operand == null` (`negated: false`) and `operand != null`.
    IsNull { operand: Box<Node>, negated: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub ty: Ty,
    pub span: Span,
    pub kind: NodeKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub span: Span,
    pub message: String,
}

/// A parse or type error.
pub type Error = Diagnostic;

/// A diagnostic that does not stop compilation, placed in a file. `CompiledProject::warnings`
/// holds them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    pub file: String,
    pub span: Span,
    pub message: String,
}

#[derive(Debug)]
pub struct Checked {
    pub root: Node,
    pub warnings: Vec<Diagnostic>,
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}: warning: {}", self.file, self.span.line, self.span.column, self.message)
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.span.line, self.span.column, self.message)
    }
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            NodeKind::Null => f.write_str("null"),
            NodeKind::Bool(b) => write!(f, "{b}"),
            NodeKind::Int(n) => write!(f, "{n}"),
            NodeKind::Float(text) => f.write_str(text),
            NodeKind::Str(text) => write!(f, "{text:?}"),
            NodeKind::Field { via, field, .. } => {
                for name in via {
                    write!(f, "{name}.")?;
                }
                write!(f, "{field}")
            }
            NodeKind::EnumMember { enum_name, member, .. } => write!(f, "{enum_name}.{member}"),
            NodeKind::Call { function, args } => {
                write!(f, "{}(", function.name)?;
                for (i, arg) in args.iter().enumerate() {
                    write!(f, "{}{arg}", if i > 0 { ", " } else { "" })?;
                }
                f.write_str(")")
            }
            NodeKind::Sql(text) => write!(f, "sql({text:?})"),
            NodeKind::Scope(name) => write!(f, "scope.{name}"),
            NodeKind::ParentDefaultFilter => f.write_str("parent.default_filter"),
            NodeKind::Method { receiver, method, args } => {
                write!(f, "{receiver}.{}(", method.name())?;
                let list = matches!(method, Method::In | Method::NotIn);
                f.write_str(if list { "[" } else { "" })?;
                for (i, arg) in args.iter().enumerate() {
                    write!(f, "{}{arg}", if i > 0 { ", " } else { "" })?;
                }
                f.write_str(if list { "])" } else { ")" })
            }
            NodeKind::Binary(op, l, r) => write!(f, "({l} {} {r})", op.symbol()),
            NodeKind::Not(operand) => write!(f, "~{operand}"),
            NodeKind::Neg(operand) => write!(f, "-{operand}"),
            NodeKind::Key { inner, options } => {
                write!(f, "{inner}")?;
                if options.desc {
                    f.write_str(" desc")?;
                }
                match options.nulls {
                    Some(Nulls::First) => f.write_str(" nulls first")?,
                    Some(Nulls::Last) => f.write_str(" nulls last")?,
                    None => {}
                }
                if let Some(name) = &options.opclass {
                    write!(f, " ops {name}")?;
                }
                if let Some(name) = &options.collation {
                    write!(f, " collate {name}")?;
                }
                match &options.op {
                    Some(op) => write!(f, " op {op}"),
                    None => Ok(()),
                }
            }
            NodeKind::IsNull { operand, negated } => write!(f, "({operand} is {}null)", if *negated { "not " } else { "" }),
        }
    }
}

/// Parses `text` and checks it against `model` of `ir`.
pub fn check_text(ir: &SchemaIr, model: &str, context: Context, text: &str) -> Result<Checked, Error> {
    let (pos, value) = syntax::parse_expression(text).map_err(|e| Diagnostic {
        span: Span { line: e.pos.line, column: e.pos.col, len: 1 },
        message: e.msg,
    })?;
    check(ir, model, context, pos, &value)
}

/// Checks a parsed expression against `model` of `ir`.
pub(super) fn check(ir: &SchemaIr, model: &str, context: Context, pos: Pos, value: &Value) -> Result<Checked, Error> {
    let Some(model) = ir.models.iter().find(|m| m.name == model) else {
        return fail(span_at(pos, 1), format!("unknown model `{model}`"));
    };
    let mut checker = Checker { ir, model, context, warnings: vec![] };
    let root = match context {
        Context::IndexKey => checker.key(pos, value)?,
        _ => checker.node(pos, value, None)?,
    };
    let boolean_root = match context {
        Context::Check => Some("a check"),
        Context::IndexFilter => Some("an index filter"),
        Context::DefaultFilter => Some("a default filter"),
        Context::IndexKey => None,
    };
    if let Some(what) = boolean_root {
        if root.ty != BOOL && root.ty != Ty::Unknown {
            return fail(root.span, format!("{what} must be a boolean expression, found {}", root.ty));
        }
    }
    Ok(Checked { root, warnings: checker.warnings })
}

fn span_at(pos: Pos, len: u32) -> Span {
    Span { line: pos.line, column: pos.col, len }
}

/// The span of a token that the parser measured (`pos.len`): a literal drops `_` and unescapes
/// strings, so its value is shorter than its source. `written` is the length when it did not.
fn source_span(pos: Pos, written: usize) -> Span {
    span_at(pos, if pos.len > 0 { pos.len } else { written as u32 })
}

/// The span of each name of `path`, which starts at `pos`. A path the compiler wrote has no
/// positions: its names are read as `a.b.c`.
fn name_spans(pos: Pos, path: &syntax::Path) -> Vec<Span> {
    let mut column = pos.col;
    let mut spans = vec![];
    for (i, name) in path.iter().enumerate() {
        let at = path.position(i).unwrap_or(Pos { col: column, ..pos });
        let span = source_span(at, name.chars().count());
        column = span.column + span.len + 1;
        spans.push(span);
    }
    spans
}

/// From the start of `first` to the end of `last`, on one line.
fn join_spans(first: Span, last: Span) -> Span {
    Span { len: last.column + last.len - first.column, ..first }
}

/// The options of an index or exclusion key; `lead` positional arguments come before them.
fn options_of(pos: Pos, args: &Args, minus: bool, lead: usize) -> Result<KeyOptions, Error> {
    key_options(pos, args, "index", minus, lead, false).map_err(|e| Diagnostic { span: span_at(e.pos, 1), message: e.msg })
}

fn fail<T>(span: Span, message: impl Into<String>) -> Result<T, Error> {
    Err(Diagnostic { span, message: message.into() })
}

const BOOL: Ty = Ty::Scalar(ColType::Bool);

struct Checker<'a> {
    ir: &'a SchemaIr,
    model: &'a ModelIr,
    context: Context,
    warnings: Vec<Diagnostic>,
}

impl Checker<'_> {
    /// Types one expression. `hint` is the type of the other side of a comparison: it types a
    /// bare enum member name.
    fn node(&mut self, pos: Pos, value: &Value, hint: Option<&Ty>) -> Result<Node, Error> {
        match value {
            Value::Num(n) => {
                let span = source_span(pos, n.len());
                if n.contains(['.', 'e', 'E']) {
                    return Ok(Node { ty: Ty::Scalar(ColType::Float), span, kind: NodeKind::Float(n.clone()) });
                }
                match n.parse::<i64>() {
                    Ok(i) => Ok(Node { ty: Ty::Scalar(ColType::Int), span, kind: NodeKind::Int(i) }),
                    Err(_) => fail(span, format!("the number {n} does not fit in 64 bits")),
                }
            }
            Value::Str(s) => {
                let span = source_span(pos, s.chars().count() + 2);
                let ty = match hint {
                    Some(Ty::Enum(_)) => return fail(span, format!("write the member `{s}`, not a string")),
                    Some(Ty::Scalar(t @ (ColType::DateTime | ColType::Date | ColType::Uuid))) => {
                        if let Err(message) = check_literal(*t, s) {
                            return fail(span, message);
                        }
                        Ty::Scalar(*t)
                    }
                    _ => Ty::Scalar(ColType::String),
                };
                Ok(Node { ty, span, kind: NodeKind::Str(s.clone()) })
            }
            Value::Bool(b) => Ok(Node { ty: BOOL, span: span_at(pos, if *b { 4 } else { 5 }), kind: NodeKind::Bool(*b) }),
            Value::Path(path, None) if path.as_slice() == ["null"] => {
                Ok(Node { ty: Ty::Null, span: span_at(pos, 4), kind: NodeKind::Null })
            }
            Value::Path(path, None) => self.name(path, &name_spans(pos, path), hint),
            Value::Path(path, Some(args)) if path.len() == 1 => self.call(pos, &path[0], args),
            Value::Path(path, Some(args)) if path.len() >= 2 => {
                let spans = name_spans(pos, path);
                let ((method, receiver), (method_span, receiver_spans)) =
                    (path.split_last().expect("a path has a name"), spans.split_last().expect("a path has a name"));
                let receiver = self.name(receiver, receiver_spans, None)?;
                self.method(receiver, method, *method_span, args)
            }
            Value::Method(receiver, name, name_pos, args) => {
                let receiver = self.node(receiver.0, &receiver.1, None)?;
                self.method(receiver, name, span_at(*name_pos, name.chars().count() as u32), args)
            }
            Value::Desc(inner) => self.negate(pos, inner.0, &inner.1),
            Value::Binary(op, op_pos, l, r) => self.binary(*op, *op_pos, l, r),
            Value::Neg(minus, operand) => self.negate(*minus, operand.0, &operand.1),
            Value::Not(tilde, operand) => {
                let span = span_at(*tilde, 1);
                let operand = self.node(operand.0, &operand.1, None)?;
                self.expect_bool(&operand, "~")?;
                Ok(Node { ty: BOOL, span, kind: NodeKind::Not(Box::new(operand)) })
            }
            _ => fail(span_at(pos, 1), "this expression is not supported here"),
        }
    }

    /// The root of an index or exclusion key. `-` before a field or a field's options belong to
    /// the key; `sql("...")` takes options too. In any other expression `-` negates.
    fn key(&mut self, pos: Pos, value: &Value) -> Result<Node, Error> {
        let (minus, item_pos, item) = match value {
            Value::Desc(inner) => (true, inner.0, &inner.1),
            other => (false, pos, other),
        };
        let (inner, options) = match item {
            Value::Path(path, Some(args)) if path.as_slice() == ["sql"] => {
                if minus {
                    return fail(span_at(pos, 1), "an expression key takes sort: Desc, not `-`");
                }
                (self.sql(item_pos, args, true)?, options_of(item_pos, args, false, 1)?)
            }
            Value::Path(path, args) if path.len() == 1 && self.model.fields.iter().any(|f| f.name == path[0]) => {
                let options = match args {
                    Some(args) => options_of(item_pos, args, minus, 0)?,
                    None => KeyOptions { desc: minus, ..KeyOptions::default() },
                };
                (self.name(path, &name_spans(item_pos, path), None)?, options)
            }
            _ => {
                let inner = self.node(pos, value, None)?;
                return Ok(Node { ty: inner.ty.clone(), span: inner.span, kind: NodeKind::Key { inner: Box::new(inner), options: KeyOptions::default() } });
            }
        };
        let span = Span { line: pos.line, column: pos.col, len: inner.span.column + inner.span.len - pos.col };
        Ok(Node { ty: inner.ty.clone(), span, kind: NodeKind::Key { inner: Box::new(inner), options } })
    }

    fn negate(&mut self, minus: Pos, operand_pos: Pos, operand: &Value) -> Result<Node, Error> {
        let span = span_at(minus, 1);
        let operand = self.node(operand_pos, operand, None)?;
        match &operand.ty {
            Ty::Scalar(t) if is_number(*t) => {}
            Ty::Unknown => {}
            other => return fail(span, format!("`-` takes a number, found {other}")),
        }
        Ok(Node { ty: operand.ty.clone(), span, kind: NodeKind::Neg(Box::new(operand)) })
    }

    fn expect_bool(&self, node: &Node, operator: &str) -> Result<(), Error> {
        if node.ty == BOOL || node.ty == Ty::Unknown {
            return Ok(());
        }
        fail(node.span, format!("`{operator}` takes boolean operands, found {}", node.ty))
    }

    fn binary(&mut self, op: BinOp, op_pos: Pos, l: &(Pos, Value), r: &(Pos, Value)) -> Result<Node, Error> {
        let span = span_at(op_pos, op.symbol().len() as u32);
        let (l, r) = self.operands(op, l, r)?;
        let ty = match op {
            BinOp::And | BinOp::Or => {
                self.expect_bool(&l, op.symbol())?;
                self.expect_bool(&r, op.symbol())?;
                BOOL
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => match (&l.ty, &r.ty) {
                (Ty::Scalar(a), Ty::Scalar(b)) if is_number(*a) && is_number(*b) => Ty::Scalar(wider(*a, *b)),
                (Ty::Unknown, _) | (_, Ty::Unknown) => Ty::Unknown,
                (a, b) => return fail(span, format!("type mismatch: cannot apply `{}` to {a} and {b}", op.symbol())),
            },
            BinOp::Eq | BinOp::Ne if l.ty == Ty::Null || r.ty == Ty::Null => {
                return self.null_test(op, span, l, r);
            }
            _ => {
                if l.ty == Ty::Null || r.ty == Ty::Null {
                    return fail(span, "`null` only goes with `==` and `!=`; use `==` or `!=`");
                }
                require_comparable(span, &l.ty, &r.ty)?;
                if !matches!(op, BinOp::Eq | BinOp::Ne) {
                    for side in [&l, &r] {
                        if !orderable(&side.ty) {
                            return fail(span, format!("`{}` does not order {}", op.symbol(), side.ty));
                        }
                    }
                }
                BOOL
            }
        };
        Ok(Node { ty, span, kind: NodeKind::Binary(op, Box::new(l), Box::new(r)) })
    }

    /// `x == null` and `x != null`. A non-null field is a warning: the test never changes.
    fn null_test(&mut self, op: BinOp, span: Span, l: Node, r: Node) -> Result<Node, Error> {
        let operand = if l.ty == Ty::Null { r } else { l };
        if operand.ty == Ty::Null {
            return fail(span, "both sides are `null`");
        }
        let negated = op == BinOp::Ne;
        self.warn_if_never_null(&operand, &format!("{operand} {} null", op.symbol()), negated);
        Ok(Node { ty: BOOL, span, kind: NodeKind::IsNull { operand: Box::new(operand), negated } })
    }

    /// A null test on a field that is never null always gives `verdict`.
    fn warn_if_never_null(&mut self, operand: &Node, written: &str, verdict: bool) {
        // Behind a relation a join can make the field null, so only a field of the model itself warns.
        if let NodeKind::Field { via, model, field, .. } = &operand.kind {
            if via.is_empty() && !self.nullable(model, field) {
                self.warnings.push(Diagnostic {
                    span: operand.span,
                    message: format!("`{operand}` is not nullable, so `{written}` is always {verdict}"),
                });
            }
        }
    }

    fn nullable(&self, model: &str, field: &str) -> bool {
        let found = self.ir.models.iter().find(|m| m.name == model).and_then(|m| m.fields.iter().find(|f| f.name == field));
        found.is_none_or(|f| f.nullable)
    }

    /// Types both sides of a binary operator. In a comparison, a side that has no type of its own
    /// (a string literal, or a name that is no field) gets the other side's.
    fn operands(&mut self, op: BinOp, l: &(Pos, Value), r: &(Pos, Value)) -> Result<(Node, Node), Error> {
        if !matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
            let left = self.node(l.0, &l.1, None)?;
            return Ok((left, self.node(r.0, &r.1, None)?));
        }
        if !self.is_untyped(&l.1) {
            let left = self.node(l.0, &l.1, None)?;
            let right = self.node(r.0, &r.1, Some(&left.ty))?;
            return Ok((left, right));
        }
        match self.node(r.0, &r.1, None) {
            Ok(right) => Ok((self.node(l.0, &l.1, Some(&right.ty))?, right)),
            // Both sides lack a type: the left side reports first.
            Err(right_error) => match self.node(l.0, &l.1, None) {
                Err(left_error) => Err(left_error),
                Ok(_) => Err(right_error),
            },
        }
    }

    fn is_untyped(&self, value: &Value) -> bool {
        match value {
            Value::Str(_) => true,
            Value::Path(path, None) if path.len() == 2 && path[0] == "scope" => true,
            Value::Path(path, None) => {
                path.len() == 1
                    && path[0] != "null"
                    && !self.model.fields.iter().any(|f| f.name == path[0])
                    && !self.model.relations.iter().any(|r| r.name == path[0])
            }
            _ => false,
        }
    }

    /// A name: a field, a field behind to-one relations, `scope.x`, `parent.default_filter`, or
    /// an enum member when `hint` is an enum.
    fn name(&mut self, path: &[String], spans: &[Span], hint: Option<&Ty>) -> Result<Node, Error> {
        let span = join_spans(spans[0], spans[spans.len() - 1]);
        let first = &path[0];
        let own = self.model.fields.iter().any(|f| f.name == *first) || self.model.relations.iter().any(|r| r.name == *first);
        if path.len() == 2 && !own {
            match first.as_str() {
                "scope" => return self.scope(span, &path[1], hint),
                "parent" => return self.parent(span, &path[1]),
                _ => {}
            }
        }
        let ir = self.ir;
        let mut model = self.model;
        let mut via: Vec<String> = vec![];
        for (i, step) in path.iter().enumerate() {
            let last = i + 1 == path.len();
            if let Some(field) = model.fields.iter().find(|f| f.name == *step) {
                if !last {
                    return fail(span, format!("`{step}` is a field, not a relation"));
                }
                return Ok(Node {
                    ty: field_ty(field),
                    span,
                    kind: NodeKind::Field { via, model: model.name.clone(), field: field.name.clone(), spans: spans.to_vec() },
                });
            }
            if let Some(relation) = model.relations.iter().find(|r| r.name == *step) {
                let target = ir.models.iter().find(|m| m.name == relation.target);
                if via.is_empty() && self.context != Context::DefaultFilter && !last {
                    return fail(span, format!("`{}` follows a relation; only a default filter can", path.join(".")));
                }
                if last {
                    let key = target.and_then(|t| t.key_fields().first().map(|k| k.to_string())).unwrap_or_else(|| "id".into());
                    return fail(span, format!("`{step}` is a relation; name one of its fields, e.g. `{step}.{key}`"));
                }
                if relation.kind == RelKind::Many {
                    return fail(span, format!("`{step}` is a to-many relation; a path follows only to-one relations"));
                }
                let Some(target) = target else {
                    return fail(span, format!("relation `{step}` points to the unknown model `{}`", relation.target));
                };
                via.push(step.clone());
                model = target;
                continue;
            }
            if let (1, Some(Ty::Enum(enum_name))) = (path.len(), hint) {
                let value = ir.enums.iter().find(|e| e.name == *enum_name).and_then(|e| e.values.iter().find(|v| v.name == *step));
                return match value {
                    Some(v) => Ok(Node {
                        ty: Ty::Enum(enum_name.clone()),
                        span,
                        kind: NodeKind::EnumMember { enum_name: enum_name.clone(), member: step.clone(), value: v.value.clone() },
                    }),
                    None => fail(span, format!("enum `{enum_name}` has no member `{step}`")),
                };
            }
            return fail(span, format!("unknown field `{step}` on model `{}`", model.name));
        }
        unreachable!("a path has at least one name")
    }

    fn scope(&mut self, span: Span, name: &str, hint: Option<&Ty>) -> Result<Node, Error> {
        if self.context != Context::DefaultFilter {
            return fail(span, "`scope` is only available in a default filter");
        }
        match hint {
            Some(ty) if *ty != Ty::Null => Ok(Node { ty: ty.clone(), span, kind: NodeKind::Scope(name.to_string()) }),
            _ => fail(span, format!("`scope.{name}` takes the type of the other side of a comparison")),
        }
    }

    fn parent(&mut self, span: Span, name: &str) -> Result<Node, Error> {
        if name != "default_filter" {
            return fail(span, "`parent` has only `default_filter`");
        }
        if self.context != Context::DefaultFilter {
            return fail(span, "`parent.default_filter` is only available in a default filter");
        }
        Ok(Node { ty: BOOL, span, kind: NodeKind::ParentDefaultFilter })
    }

    /// `sql("...")`; a key root also takes key options after the string (`options`).
    fn sql(&self, pos: Pos, args: &Args, options: bool) -> Result<Node, Error> {
        let span = span_at(pos, "sql".len() as u32);
        match args.positional.as_slice() {
            [(_, Value::Str(text))] if options || args.named.is_empty() => {
                Ok(Node { ty: Ty::Unknown, span, kind: NodeKind::Sql(text.clone()) })
            }
            _ => fail(span, "`sql` takes one string"),
        }
    }

    /// `name(args)`: `sql("...")` or a function of the catalog.
    fn call(&mut self, pos: Pos, name: &str, args: &Args) -> Result<Node, Error> {
        let span = span_at(pos, name.chars().count() as u32);
        if name == "sql" {
            return self.sql(pos, args, false);
        }
        let Some(function) = function(name) else {
            return fail(span, format!("unknown function `{name}`; write `sql(\"...\")` for raw SQL"));
        };
        if function.sql(self.ir.dialect).is_none() {
            let dialect = match self.ir.dialect {
                Dialect::Postgres => "PostgreSQL",
                Dialect::Sqlite => "SQLite",
            };
            return fail(span, format!("`{name}` has no SQL for {dialect}; write `sql(\"...\")`"));
        }
        match (self.context, function.volatility) {
            (Context::Check, Volatility::Volatile) => {
                return fail(span, format!("`{name}` is volatile, so a check cannot use it"));
            }
            (Context::IndexFilter | Context::IndexKey, Volatility::Stable | Volatility::Volatile) => {
                return fail(span, format!("`{name}` is not immutable, so an index cannot use it"));
            }
            _ => {}
        }
        if !args.named.is_empty() {
            return fail(span, format!("`{name}` takes no named arguments"));
        }
        let given = args.positional.len();
        let plural = |n: usize| if n == 1 { "argument" } else { "arguments" };
        let kinds: Vec<Param> = match function.params {
            Params::Fixed(params) if params.len() != given => {
                return fail(span, format!("`{name}` takes {} {}, found {given}", params.len(), plural(params.len())));
            }
            Params::Fixed(params) => params.to_vec(),
            Params::Variadic(_) if given == 0 => return fail(span, format!("`{name}` takes at least 1 argument, found 0")),
            Params::Variadic(param) => vec![param; given],
        };
        let mut nodes: Vec<Node> = vec![];
        for ((arg_pos, arg), kind) in args.positional.iter().zip(kinds) {
            let node = match kind {
                Param::Text => {
                    let node = self.node(*arg_pos, arg, Some(&Ty::Scalar(ColType::String)))?;
                    require_text(node.span, name, &node.ty)?;
                    node
                }
                Param::Number => {
                    let node = self.node(*arg_pos, arg, None)?;
                    require_number(node.span, name, &node.ty)?;
                    node
                }
                Param::Same => {
                    let first = nodes.first().map(|n| n.ty.clone());
                    let node = self.node(*arg_pos, arg, first.as_ref())?;
                    if let Some(first) = &first {
                        require_comparable(node.span, first, &node.ty)?;
                    }
                    node
                }
            };
            nodes.push(node);
        }
        let ty = match function.returns {
            Returns::First => nodes[0].ty.clone(),
            Returns::Fixed(t) => Ty::Scalar(t),
        };
        Ok(Node { ty, span, kind: NodeKind::Call { function, args: nodes } })
    }

    fn method(&mut self, receiver: Node, name: &str, span: Span, args: &Args) -> Result<Node, Error> {
        let Some(method) = Method::from_name(name) else {
            return self.special_method(receiver, name, span, args);
        };
        if !args.named.is_empty() {
            return fail(span, format!("`{name}` takes no named arguments"));
        }
        let positional = &args.positional;
        let ty = receiver.ty.clone();
        let mut nodes = vec![];
        match method {
            Method::In | Method::NotIn => {
                let [(_, Value::List(items))] = positional.as_slice() else {
                    return fail(span, format!("`{name}` takes a list"));
                };
                for (item_pos, item) in items {
                    let item = self.node(*item_pos, item, Some(&ty))?;
                    require_comparable(item.span, &ty, &item.ty)?;
                    nodes.push(item);
                }
            }
            Method::Between => {
                if positional.len() != 2 {
                    return fail(span, format!("`between` takes 2 arguments, found {}", positional.len()));
                }
                if !orderable(&ty) {
                    return fail(span, format!("`between` does not order {ty}"));
                }
                for (arg_pos, arg) in positional {
                    let arg = self.node(*arg_pos, arg, Some(&ty))?;
                    require_comparable(arg.span, &ty, &arg.ty)?;
                    nodes.push(arg);
                }
            }
            _ => {
                require_text(span, name, &ty)?;
                let [(arg_pos, arg)] = positional.as_slice() else {
                    return fail(span, format!("`{name}` takes 1 argument, found {}", positional.len()));
                };
                let arg = self.node(*arg_pos, arg, Some(&Ty::Scalar(ColType::String)))?;
                require_text(arg.span, name, &arg.ty)?;
                nodes.push(arg);
            }
        }
        Ok(Node { ty: BOOL, span, kind: NodeKind::Method { receiver: Box::new(receiver), method, args: nodes } })
    }

    /// `is_null()`, and the error for any other name.
    fn special_method(&mut self, receiver: Node, name: &str, span: Span, args: &Args) -> Result<Node, Error> {
        if name != "is_null" {
            return fail(span, format!("unknown method `{name}`"));
        }
        let given = args.positional.len() + args.named.len();
        if given != 0 {
            return fail(span, format!("`is_null` takes no arguments, found {given}"));
        }
        self.warn_if_never_null(&receiver, &format!("{receiver}.is_null()"), false);
        Ok(Node { ty: BOOL, span, kind: NodeKind::IsNull { operand: Box::new(receiver), negated: false } })
    }
}

/// A string that stands for a date, a date-time or a UUID must have the form the column takes.
fn check_literal(t: ColType, text: &str) -> Result<(), String> {
    match t {
        ColType::DateTime if chrono::DateTime::parse_from_rfc3339(text).is_err() => {
            Err(format!("`{text}` is not an RFC 3339 date-time, e.g. \"2024-01-01T00:00:00Z\""))
        }
        ColType::Date if chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_err() => {
            Err(format!("`{text}` is not a date; write YYYY-MM-DD"))
        }
        ColType::Uuid if uuid::Uuid::parse_str(text).is_err() => Err(format!("`{text}` is not a UUID")),
        _ => Ok(()),
    }
}

/// `Unknown` passes the three checks below: it matches every type.
fn require_text(at: Span, name: &str, ty: &Ty) -> Result<(), Error> {
    if matches!(ty, Ty::Scalar(t) if is_text(*t)) || *ty == Ty::Unknown {
        return Ok(());
    }
    fail(at, format!("`{name}` takes a string, found {ty}"))
}

fn require_number(at: Span, name: &str, ty: &Ty) -> Result<(), Error> {
    if matches!(ty, Ty::Scalar(t) if is_number(*t)) || *ty == Ty::Unknown {
        return Ok(());
    }
    fail(at, format!("`{name}` takes a number, found {ty}"))
}

fn require_comparable(at: Span, a: &Ty, b: &Ty) -> Result<(), Error> {
    if comparable(a, b) {
        return Ok(());
    }
    fail(at, format!("type mismatch: cannot compare {a} with {b}"))
}

fn is_number(t: ColType) -> bool {
    matches!(t, ColType::BigInt | ColType::Int | ColType::Float | ColType::Decimal)
}

fn is_text(t: ColType) -> bool {
    matches!(t, ColType::String | ColType::Text)
}

/// The type of an arithmetic result: the wider of the two.
fn wider(a: ColType, b: ColType) -> ColType {
    for t in [ColType::Decimal, ColType::Float, ColType::BigInt] {
        if a == t || b == t {
            return t;
        }
    }
    ColType::Int
}

fn comparable(a: &Ty, b: &Ty) -> bool {
    match (a, b) {
        (Ty::Unknown, _) | (_, Ty::Unknown) => true,
        (Ty::Scalar(x), Ty::Scalar(y)) => x == y || (is_number(*x) && is_number(*y)) || (is_text(*x) && is_text(*y)),
        (Ty::Enum(x), Ty::Enum(y)) => x == y,
        _ => false,
    }
}

fn orderable(t: &Ty) -> bool {
    match t {
        Ty::Scalar(t) => !matches!(t, ColType::Bool | ColType::Json),
        Ty::Unknown => true,
        _ => false,
    }
}

fn field_ty(field: &FieldIr) -> Ty {
    match (&field.enum_name, field.array) {
        (Some(name), false) => Ty::Enum(name.clone()),
        (_, true) => Ty::Array(field.ty),
        _ => Ty::Scalar(field.ty),
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Scalar(t) => f.write_str(scalar_name(*t)),
            Ty::Array(t) => write!(f, "{}[]", scalar_name(*t)),
            Ty::Enum(name) => f.write_str(name),
            Ty::Null => f.write_str("null"),
            Ty::Unknown => f.write_str("unknown"),
        }
    }
}

fn scalar_name(t: ColType) -> &'static str {
    match t {
        ColType::BigInt => "BigInt",
        ColType::Int => "Int",
        ColType::Float => "Float",
        ColType::Bool => "Boolean",
        ColType::String => "String",
        ColType::Text => "Text",
        ColType::DateTime => "DateTime",
        ColType::Date => "Date",
        ColType::Uuid => "Uuid",
        ColType::Json => "Json",
        ColType::Decimal => "Decimal",
    }
}

#[cfg(test)]
mod tests;
