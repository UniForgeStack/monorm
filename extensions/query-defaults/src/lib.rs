//! Optional compiler for model query policies; execution uses prepared host IR.
use orm_contracts::{extension::{OrderKey, ReadDefault}, ir::{Nulls, SchemaIr}};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

/// Compiler-local inheritance state; parent identities never enter the host contract.
#[derive(Clone, Default)]
struct ModelPolicy {
    model: String,
    filter: Option<Value>,
    /// `@@query.filter("<name>", ...)`: one entry each, inherited by name.
    named: BTreeMap<String, Value>,
    fields: Option<Vec<String>>,
    related: Vec<Vec<String>>,
    order: Option<Vec<OrderKey>>,
}

/// The entry that holds the order, fields, related paths and the `@@query.defaults(filter:)` filter.
const POLICY: &str = "query";
/// The `ReadDefault` source of every entry this extension emits.
const SOURCE: &str = "orm.query-defaults";

/// The options of a policy, in `@@query.defaults(<option>: ...)` and `@@query.<option>(...)`.
/// `@@query.filter` is not one: it adds named filters (see `named_filters`).
const OPTIONS: [&str; 5] = ["filter", "fields", "related", "order", "parent"];

/// Each option a model sets, with the declaration that sets it. One option has one source.
fn options(ir: &SchemaIr, name: &str) -> Result<BTreeMap<&'static str, Value>, String> {
    let mut out: BTreeMap<&'static str, (Value, String)> = BTreeMap::new();
    let mut attributes: Vec<&str> = vec![];
    for d in ir.behavior.declarations.iter().filter(|d| d.model == name && d.field.is_none() && d.attribute.starts_with("query.")) {
        let at = format!("{}:{}", d.location.file, d.location.line);
        if d.attribute == "query.filter" { continue; }
        if attributes.contains(&d.attribute.as_str()) { return Err(format!("{at}: {name}: @@{} is given twice", d.attribute)); }
        attributes.push(&d.attribute);
        let given: Vec<(&'static str, Value, String)> = match d.attribute.trim_start_matches("query.") {
            "defaults" => OPTIONS.iter().filter_map(|o| d.arguments.get(*o).map(|v| (*o, v.clone(), format!("@@query.defaults({o}:) at {at}")))).collect(),
            "order" => vec![("order", Value::Array(d.positional.clone()), format!("@@query.order at {at}"))],
            other => match OPTIONS.iter().find(|o| **o == other) {
                Some(o) => vec![(*o, d.positional.first().cloned().ok_or_else(|| format!("{at}: @@{} needs a value", d.attribute))?, format!("@@{} at {at}", d.attribute))],
                None => continue,
            },
        };
        for (option, value, source) in given {
            if let Some((_, first)) = out.get(option) { return Err(format!("{name}: {option} is set by both {first} and {source}; keep one")); }
            out.insert(option, (value, source));
        }
    }
    Ok(out.into_iter().map(|(k, (v, _))| (k, v)).collect())
}

/// The `@@query.filter("<name>", "<expr>")` declarations of a model, in source order.
fn named_filters(ir: &SchemaIr, model: &str) -> Result<Vec<(String, String, String)>, String> {
    let mut out: Vec<(String, String, String)> = vec![];
    for d in ir.behavior.declarations.iter().filter(|d| d.model == model && d.field.is_none() && d.attribute == "query.filter") {
        let at = format!("{}:{}", d.location.file, d.location.line);
        let [Some(name), Some(expr)] = [d.positional.first(), d.positional.get(1)].map(|v| v.and_then(Value::as_str)) else {
            return Err(format!("{at}: {model}: @@query.filter takes a name and an expression: @@query.filter(\"<name>\", \"<expr>\")"));
        };
        if d.positional.len() != 2 { return Err(format!("{at}: {model}: @@query.filter takes a name and an expression: @@query.filter(\"<name>\", \"<expr>\")")); }
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return Err(format!("{at}: {model}: filter name {name:?} must be letters, digits and underscores"));
        }
        if name == POLICY { return Err(format!("{at}: {model}: the filter name {POLICY:?} is reserved for the order, fields and related entry; pick another name")); }
        if out.iter().any(|(n, _, _)| n == name) { return Err(format!("{at}: {model}: @@query.filter {name:?} is given twice")); }
        out.push((name.to_owned(), expr.to_owned(), at));
    }
    Ok(out)
}

/// `"-created_at nulls last"`: a field, `-` for descending, and an optional nulls position.
fn order_key(model: &orm_contracts::ir::ModelIr, source: &Value) -> Result<OrderKey, String> {
    let source = source.as_str().ok_or_else(|| format!("{}: an order column is a string such as \"-created_at\"", model.name))?;
    let mut words = source.split_whitespace();
    let first = words.next().ok_or_else(|| format!("{}: empty order column", model.name))?;
    let (desc, field) = match first.strip_prefix('-') { Some(f) => (true, f), None => (false, first) };
    if !model.fields.iter().any(|f| f.name == field) {
        return Err(format!("{}: order column {field:?} is not a field of the model", model.name));
    }
    let nulls = match (words.next(), words.next(), words.next()) {
        (None, None, None) => None,
        (Some("nulls"), Some("first"), None) => Some(Nulls::First),
        (Some("nulls"), Some("last"), None) => Some(Nulls::Last),
        _ => return Err(format!("{}: order column {source:?}: write \"[-]field [nulls first|last]\"", model.name)),
    };
    Ok(OrderKey { field: field.to_owned(), desc, nulls })
}

/// A default order the model can run: its own fields, once each, an orderable type, and
/// `nulls` on a column that can hold NULL. `source` is the model whose attribute set it.
fn check_order(ir: &SchemaIr, model: &orm_contracts::ir::ModelIr, order: &[OrderKey], source: &str) -> Result<(), String> {
    let name = &model.name;
    let from = if source == name { String::new() } else { format!(" (from the default order of {source}; give {name} its own @@query.order(...), or clear it with @@query.order())") };
    let narrowed = |field: &str| ir.behavior.result_checks.iter().any(|c| c.model == *name && c.field == field && c.non_null);
    for (i, k) in order.iter().enumerate() {
        let field = &k.field;
        let Some(f) = model.fields.iter().find(|f| f.name == *field) else {
            return Err(format!("{name}: order column {field:?} is not a field of the model{from}"));
        };
        if order[..i].iter().any(|p| p.field == *field) {
            return Err(format!("{name}: order column {field:?} is given twice{from}"));
        }
        if let Some(t) = f.db_type.as_deref().filter(|t| matches!(*t, "json" | "xml")) {
            return Err(format!("{name}: order column {field:?} is {t}, which the database can't order{from}"));
        }
        if (f.nullable || narrowed(field)) && k.nulls.is_none() {
            let key = format!("{}{field}", if k.desc { "-" } else { "" });
            return Err(format!("{name}: order column {field:?} can be NULL: write \"{key} nulls first\" or \"{key} nulls last\"{from}"));
        }
    }
    Ok(())
}

pub fn lower(ir: &mut SchemaIr) -> Result<(), String> {
    let declarations = &ir.behavior.declarations;
    let mut resolved: HashMap<String, ModelPolicy> = ir.behavior.read_defaults.iter().filter(|d| d.source == SOURCE && d.name == POLICY).map(|d| (d.model.clone(), ModelPolicy { model: d.model.clone(), filter: d.filter.clone(), named: BTreeMap::new(), fields: d.fields.clone(), related: d.load.clone(), order: d.order.clone() })).collect();
    for d in ir.behavior.read_defaults.iter().filter(|d| d.source == SOURCE && d.name != POLICY) {
        if let (Some(r), Some(f)) = (resolved.get_mut(&d.model), &d.filter) { r.named.insert(d.name.clone(), f.clone()); }
    }
    let mut order_sources: HashMap<String, String> = HashMap::new();
    fn resolve(ir: &SchemaIr, name: &str, resolved: &mut HashMap<String, ModelPolicy>, order_sources: &mut HashMap<String, String>, stack: &mut Vec<String>) -> Result<ModelPolicy, String> {
        if let Some(d) = resolved.get(name) { return Ok(d.clone()); }
        if stack.iter().any(|n| n == name) { return Err(format!("query-default inheritance cycle: {} -> {name}", stack.join(" -> "))); }
        stack.push(name.into());
        let model = ir.models.iter().find(|m| m.name == name).ok_or_else(|| format!("unknown policy parent {name}"))?;
        let given = options(ir, name)?;
        // The view pass (logical phase) fills `model_views` before this behavior pass.
        let view_parent = ir.behavior.derived_parent(name);
        let composed_parent = ir.behavior.declarations.iter().find(|d| d.model == name && d.attribute == "composition.model").and_then(|d| d.arguments.get("parent")).and_then(Value::as_str);
        let parent = given.get("parent").and_then(Value::as_str).or(view_parent).or(composed_parent);
        let inherited = match parent { Some(p) => resolve(ir, p, resolved, order_sources, stack)?, None => ModelPolicy::default() };
        let mut out = inherited.clone(); out.model = name.into();
        if let Some(source) = given.get("filter") {
            let source = source.as_str().ok_or("filter must be a string")?;
            out.filter = if source == "none" { None } else { Some(Parser::parse(source, inherited.filter.as_ref())?) };
        }
        for (filter, source, at) in named_filters(ir, name)? {
            if source == "none" { out.named.remove(&filter); continue; }
            let parsed = Parser::parse(&source, inherited.named.get(&filter)).map_err(|e| format!("{at}: {name}: @@query.filter {filter:?}: {e}"))?;
            out.named.insert(filter, parsed);
        }
        if let Some(fields) = given.get("fields") {
            let fields = strings(fields)?;
            out.fields = if fields == ["*"] { None } else { Some(fields) };
        }
        if let Some(related) = given.get("related") {
            out.related = strings(related)?.into_iter().map(|s| s.split('.').map(str::to_owned).collect()).collect();
        }
        if let Some(order) = given.get("order") {
            out.order = Some(order.as_array().ok_or("order must be a list of strings")?.iter().map(|k| order_key(model, k)).collect::<Result<_, _>>()?);
        }
        let source = match parent.and_then(|p| order_sources.get(p)) {
            Some(s) if !given.contains_key("order") => s.clone(),
            _ => name.to_owned(),
        };
        check_order(ir, model, out.order.as_deref().unwrap_or_default(), &source)?;
        order_sources.insert(name.into(), source);
        let excluded: Vec<&str> = ir.behavior.declarations.iter().filter(|d| d.model == name && d.attribute == "query.selectOut").filter_map(|d| d.field.as_deref()).collect();
        if !excluded.is_empty() {
            let fields = out.fields.get_or_insert_with(|| model.fields.iter().map(|f| f.name.clone()).collect());
            fields.retain(|f| !excluded.contains(&f.as_str()));
        }
        stack.pop(); resolved.insert(name.into(), out.clone()); Ok(out)
    }
    if !declarations.iter().any(|d| d.attribute.starts_with("query.")) && resolved.is_empty() { return Ok(()); }
    for model in &ir.models { resolve(ir, &model.name, &mut resolved, &mut order_sources, &mut vec![])?; }
    ir.behavior.read_defaults.retain(|d| d.source != SOURCE);
    for m in &ir.models {
        let d = resolved.remove(&m.name).expect("resolved model");
        for (name, filter) in d.named {
            ir.behavior.read_defaults.push(ReadDefault { source: SOURCE.into(), name, model: d.model.clone(), filter: Some(filter), ..Default::default() });
        }
        ir.behavior.read_defaults.push(ReadDefault { source: SOURCE.into(), name: POLICY.into(), model: d.model,
            filter: d.filter, fields: d.fields, load: d.related,
            order: d.order, annotate: BTreeMap::new() });
    }
    Ok(())
}
fn strings(value: &Value) -> Result<Vec<String>, String> {
    value.as_array().ok_or("expected a string list")?.iter().map(|v| v.as_str().map(str::to_owned).ok_or("expected a string list".into())).collect()
}

struct Parser<'a> { tokens: Vec<String>, pos: usize, parent: Option<&'a Value> }
impl<'a> Parser<'a> {
    fn parse(source: &str, parent: Option<&'a Value>) -> Result<Value, String> {
        let mut tokens = vec![];
        let mut chars = source.chars().peekable();
        while let Some(c) = chars.next() {
            if c.is_whitespace() { continue; }
            let mut token = c.to_string();
            if c == '"' {
                let mut escaped = false; let mut closed = false;
                for c in chars.by_ref() { token.push(c); if c == '"' && !escaped { closed = true; break; } escaped = c == '\\' && !escaped; }
                if !closed { return Err("unterminated filter string".into()); }
            } else if c.is_alphanumeric() || c == '_' || c == '-' {
                while chars.peek().is_some_and(|c| c.is_alphanumeric() || *c == '_' || *c == '.') { token.push(chars.next().unwrap()); }
            } else if matches!(c, '=' | '!' | '<' | '>' | '&' | '|') && chars.peek().is_some_and(|n| *n == '=' || *n == c) { token.push(chars.next().unwrap()); }
            tokens.push(token);
        }
        if let Some(m) = tokens.iter().filter(|t| !t.starts_with('"')).flat_map(|t| t.split('.')).find(|s| matches!(*s, "or_null" | "and_not_null")) {
            return Err(format!("{m}() is not available in a default filter; write the NULL test as a condition, for example `(x > 1 or x == null)`"));
        }
        let mut p = Self { tokens, pos: 0, parent };
        let out = p.boolean(0)?;
        if p.pos != p.tokens.len() { return Err(format!("unexpected filter token {}", p.tokens[p.pos])); }
        Ok(out)
    }
    fn peek(&self) -> &str { self.tokens.get(self.pos).map(String::as_str).unwrap_or("") }
    fn take(&mut self) -> Result<String, String> { let t = self.tokens.get(self.pos).cloned().ok_or("incomplete default filter")?; self.pos += 1; Ok(t) }
    fn boolean(&mut self, min: u8) -> Result<Value, String> {
        let mut left = self.unary()?;
        loop {
            let (precedence, tag) = match self.peek() { "or" | "||" => (1, "or"), "and" | "&&" => (2, "and"), _ => break };
            if precedence < min { break; }
            self.take()?; let right = self.boolean(precedence + 1)?; left = json!({"t":tag,"items":[left,right]});
        }
        Ok(left)
    }
    /// `not` binds looser than comparisons, as in Python and TypeScript.
    fn unary(&mut self) -> Result<Value, String> {
        if matches!(self.peek(), "!" | "not") { self.take()?; return Ok(json!({"t":"not","item":self.unary()?})); }
        self.comparison()
    }
    fn comparison(&mut self) -> Result<Value, String> {
        let left = self.atom()?;
        let op = match self.peek() { "==" => "eq", "!=" => "ne", "<" => "lt", "<=" => "le", ">" => "gt", ">=" => "ge", _ => return Ok(left) };
        self.take()?; let right = self.atom()?;
        if right.is_null() { if op != "eq" && op != "ne" { return Err("NULL only supports == and !=".into()); } return Ok(json!({"t":"is_null","item":left,"neg":op == "ne"})); }
        if left.is_null() { return Err("put the column before null".into()); }
        Ok(json!({"t":"cmp","op":op,"l":left,"r":right}))
    }
    fn atom(&mut self) -> Result<Value, String> {
        let token = self.take()?;
        Ok(match token.as_str() {
            "(" => { let value = self.boolean(0)?; if self.take()? != ")" { return Err("expected ')'".into()); } value },
            "parent.default_filter" => self.parent.cloned().ok_or("parent.default_filter has no inherited filter")?,
            _ if token.starts_with("scope.") => {
                let name = &token["scope.".len()..];
                if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') { return Err(format!("invalid scope value {token}: write scope.<name>")); }
                json!({"t":"scope","name":name})
            }
            "null" => Value::Null,
            "true" | "false" => json!({"t":"const","value":token == "true"}),
            _ if token.starts_with('"') => json!({"t":"text","value":serde_json::from_str::<String>(&token).map_err(|e| e.to_string())?}),
            _ if token.parse::<i64>().is_ok() => json!({"t":"int","value":token.parse::<i64>().unwrap()}),
            _ if token.chars().all(|c| c.is_alphanumeric() || c == '_') => json!({"t":"col","path":[],"name":token}),
            _ => return Err(format!("unsupported filter token {token}")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ir() -> SchemaIr {
        serde_json::from_value(json!({"models":[
            {"name":"Parent","table":"p","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"visible","column":"visible","type":"bool"},{"name":"bio","column":"bio","type":"text"}]},
            {"name":"Child","table":"p","fields":[{"name":"id","column":"id","type":"int","primary_key":true},{"name":"visible","column":"visible","type":"bool"},{"name":"bio","column":"bio","type":"text"}]}
        ],"behavior":{"declarations":[
            {"model":"Parent","attribute":"query.defaults","field":null,"arguments":{"filter":"visible == true","fields":["id","visible"]},"positional":[],"location":{"file":"test","line":1,"column":1}},
            {"model":"Child","attribute":"query.defaults","field":null,"arguments":{"parent":"Parent","filter":"parent.default_filter and id > 0","fields":["*"]},"positional":[],"location":{"file":"test","line":2,"column":1}},
            {"model":"Child","attribute":"query.selectOut","field":"bio","arguments":{},"positional":[],"location":{"file":"test","line":3,"column":1}}
        ]}})).unwrap()
    }
    /// Declared effect `query-defaults`: the prepared policies only.
    #[test]
    fn lowering_changes_only_read_defaults_once() {
        let effects = orm_contracts::extension::pass_effects(&ir(), lower).unwrap();
        assert_eq!(effects.into_iter().collect::<Vec<_>>(), ["behavior.read_defaults"]);
    }
    #[test]
    fn emits_neutral_named_entries_and_preserves_other_sources() {
        let mut schema = ir();
        schema.behavior.read_defaults.push(ReadDefault { source: "tenant-extension".into(), name: "tenant".into(), model: "Parent".into(), ..Default::default() });
        lower(&mut schema).unwrap();
        assert_eq!(schema.behavior.read_defaults[0].source, "tenant-extension");
        for d in &schema.behavior.read_defaults[1..] {
            assert_eq!(d.source, SOURCE);
            assert_eq!(d.name, POLICY);
        }
        let before = serde_json::to_value(&schema).unwrap();
        lower(&mut schema).unwrap();
        assert_eq!(before, serde_json::to_value(&schema).unwrap());
    }
    #[test]
    fn inheritance_replacement_reset_and_select_out() {
        let mut schema = ir(); lower(&mut schema).unwrap();
        let d = &schema.behavior.read_defaults[1];
        assert_eq!(d.fields.as_ref().unwrap(), &["id", "visible"]);
        assert_eq!(d.filter.as_ref().unwrap()["t"], "and");
        let snapshot = serde_json::to_value(&schema).unwrap(); lower(&mut schema).unwrap(); assert_eq!(snapshot, serde_json::to_value(&schema).unwrap());
    }
    #[test]
    fn cycles_and_parent_reference_errors() {
        let mut schema = ir(); schema.behavior.declarations[0].arguments.insert("parent".into(), json!("Child"));
        assert!(lower(&mut schema).unwrap_err().contains("cycle"));
        assert!(Parser::parse("parent.default_filter", None).is_err());
        assert!(Parser::parse("visible == true trailing", None).is_err());
        assert!(Parser::parse("name == \"quoted value\" or id >= 4", None).is_ok());
    }
    #[test]
    fn null_methods_are_rejected_with_a_clear_error() {
        for source in ["(id > 1).or_null()", "id.in([1]).and_not_null()", "writer.or_null.name == 1", "writer.and_not_null == 1"] {
            let err = Parser::parse(source, None).unwrap_err();
            assert!(err.contains("not available in a default filter") && err.contains("x == null"), "{err}");
        }
        assert!(Parser::parse("(id > 1 or id == null)", None).is_ok());
        assert!(Parser::parse("name == \"or_null\"", None).is_ok());
        assert!(Parser::parse("name == \"a.or_null\"", None).is_ok());
    }
    #[test]
    fn inheritance_clear_and_replacement_are_distinct() {
        let mut schema = ir();
        schema.behavior.declarations[1].arguments.remove("filter");
        schema.behavior.declarations[1].arguments.remove("fields");
        lower(&mut schema).unwrap();
        assert_eq!(schema.behavior.read_defaults[0].filter, schema.behavior.read_defaults[1].filter);
        assert_eq!(schema.behavior.read_defaults[0].fields, schema.behavior.read_defaults[1].fields);
        let mut schema = ir();
        schema.behavior.declarations[1].arguments.insert("filter".into(), json!("id == 1"));
        lower(&mut schema).unwrap();
        assert_eq!(schema.behavior.read_defaults[1].filter.as_ref().unwrap()["t"], "cmp");
        let mut schema = ir();
        schema.behavior.declarations.retain(|d| d.attribute != "query.selectOut");
        schema.behavior.declarations[1].arguments.insert("filter".into(), json!("none"));
        schema.behavior.declarations[1].arguments.insert("related".into(), json!([]));
        lower(&mut schema).unwrap();
        assert!(schema.behavior.read_defaults[1].filter.is_none());
        assert!(schema.behavior.read_defaults[1].fields.is_none());
        assert!(schema.behavior.read_defaults[1].load.is_empty());
    }
    #[test]
    fn proxy_and_composed_models_inherit_through_typed_parents() {
        let mut schema = ir();
        schema.behavior.declarations.retain(|d| d.model != "Child");
        schema.behavior.model_views.push(serde_json::from_value(json!({"model":"Child","source":"Parent","root":"Parent"})).unwrap());
        lower(&mut schema).unwrap();
        assert_eq!(schema.behavior.read_defaults[1].filter, schema.behavior.read_defaults[0].filter);
        let mut schema = ir();
        schema.behavior.declarations.retain(|d| d.model != "Child");
        schema.behavior.declarations.push(serde_json::from_value(json!({"model":"Child","attribute":"composition.model","field":null,"arguments":{"parent":"Parent","parentRef":"p","childRef":"c"},"positional":[],"location":{"file":"test","line":4,"column":1}})).unwrap());
        lower(&mut schema).unwrap();
        // An untyped proxy declaration is no parent source; only the lowered contract is.
        let mut schema = ir();
        schema.behavior.declarations.retain(|d| d.model != "Child");
        schema.behavior.declarations.push(serde_json::from_value(json!({"model":"Child","attribute":"proxy.model","field":null,"arguments":{"parent":"Parent"},"positional":[],"location":{"file":"test","line":4,"column":1}})).unwrap());
        lower(&mut schema).unwrap();
        assert!(schema.behavior.read_defaults[1].filter.is_none());
    }
    fn declare(schema: &mut SchemaIr, model: &str, attribute: &str, arguments: Value, positional: Value) {
        let line = schema.behavior.declarations.len() + 1;
        schema.behavior.declarations.push(serde_json::from_value(json!({"model":model,"attribute":attribute,"field":null,"arguments":arguments,"positional":positional,"location":{"file":"test","line":line,"column":1}})).unwrap());
    }
    #[test]
    fn one_attribute_per_option_matches_the_defaults_form() {
        let mut long = ir();
        long.behavior.declarations[0].arguments.insert("related".into(), json!([]));
        long.behavior.declarations[0].arguments.insert("order".into(), json!(["-bio nulls last", "id"]));
        lower(&mut long).unwrap();
        let mut short = ir();
        short.behavior.declarations.retain(|d| d.model != "Parent");
        declare(&mut short, "Parent", "query.fields", json!({}), json!([["id", "visible"]]));
        declare(&mut short, "Parent", "query.related", json!({}), json!([[]]));
        declare(&mut short, "Parent", "query.order", json!({}), json!(["-bio nulls last", "id"]));
        let child = short.behavior.declarations.iter().position(|d| d.model == "Child").unwrap();
        short.behavior.declarations[child].arguments.remove("parent");
        declare(&mut short, "Parent", "query.defaults", json!({"filter":"visible == true"}), json!([]));
        declare(&mut short, "Child", "query.parent", json!({}), json!(["Parent"]));
        lower(&mut short).unwrap();
        assert_eq!(serde_json::to_value(&long.behavior.read_defaults).unwrap(), serde_json::to_value(&short.behavior.read_defaults).unwrap());
        let order = serde_json::to_value(&short.behavior.read_defaults[1].order).unwrap();
        assert_eq!(order, json!([{"field":"bio","desc":true,"nulls":"last"},{"field":"id"}]), "the child inherits the order");
    }
    #[test]
    fn an_option_has_one_source() {
        let mut schema = ir();
        declare(&mut schema, "Parent", "query.order", json!({}), json!(["id"]));
        declare(&mut schema, "Parent", "query.order", json!({}), json!(["-id"]));
        assert!(lower(&mut schema).unwrap_err().contains("@@query.order is given twice"));
        let mut schema = ir();
        schema.behavior.declarations[0].arguments.insert("order".into(), json!(["id"]));
        declare(&mut schema, "Parent", "query.order", json!({}), json!(["-id"]));
        assert!(lower(&mut schema).unwrap_err().contains("order is set by both @@query.defaults(order:)"));
    }
    #[test]
    fn default_order_columns_and_clearing() {
        let order = |child: Value| {
            let mut schema = ir();
            schema.behavior.declarations[0].arguments.insert("order".into(), json!(["-id"]));
            declare(&mut schema, "Child", "query.order", json!({}), child);
            lower(&mut schema).map(|_| serde_json::to_value(&schema.behavior.read_defaults[1].order).unwrap())
        };
        assert_eq!(order(json!([])).unwrap(), json!([]), "@@query.order() clears the inherited order");
        assert_eq!(order(json!(["visible nulls first"])).unwrap(), json!([{"field":"visible","nulls":"first"}]));
        assert!(order(json!(["nope"])).unwrap_err().contains("\"nope\" is not a field of the model"));
        assert!(order(json!(["id desc"])).unwrap_err().contains("write \"[-]field [nulls first|last]\""));
        assert!(order(json!(["-id nulls"])).is_err());
        let mut schema = ir();
        schema.behavior.declarations[1].arguments.insert("order".into(), json!([]));
        schema.behavior.declarations[0].arguments.insert("order".into(), json!(["id"]));
        lower(&mut schema).unwrap();
        assert!(schema.behavior.read_defaults[1].order.as_ref().is_none_or(Vec::is_empty), "order: [] clears it too");
        assert_eq!(schema.behavior.read_defaults[0].order.as_ref().unwrap().len(), 1);
    }
    #[test]
    fn a_default_order_is_checked_at_load() {
        let lowered = |nullable: bool, db_type: Option<&str>, parent: Value, child: Option<Value>| {
            let mut schema = ir();
            schema.models[0].fields[2].nullable = nullable;
            schema.models[0].fields[2].db_type = db_type.map(str::to_owned);
            schema.models[1].fields.retain(|f| f.name != "visible");
            declare(&mut schema, "Parent", "query.order", json!({}), parent);
            if let Some(child) = child { declare(&mut schema, "Child", "query.order", json!({}), child); }
            lower(&mut schema).map(|_| ())
        };
        let error = lowered(true, None, json!(["-bio"]), None).unwrap_err();
        assert!(error.contains("Parent: order column \"bio\" can be NULL: write \"-bio nulls first\" or \"-bio nulls last\""), "{error}");
        assert!(lowered(true, None, json!(["-bio nulls last"]), None).is_ok());
        let error = lowered(false, Some("json"), json!(["bio"]), None).unwrap_err();
        assert!(error.contains("order column \"bio\" is json, which the database can't order"), "{error}");
        let error = lowered(false, Some("xml"), json!(["bio"]), None).unwrap_err();
        assert!(error.contains("order column \"bio\" is xml, which the database can't order"), "{error}");
        assert!(lowered(false, None, json!(["id", "-id"]), None).unwrap_err().contains("order column \"id\" is given twice"));
        let error = lowered(false, None, json!(["visible"]), None).unwrap_err();
        assert!(error.contains("Child: order column \"visible\" is not a field of the model (from the default order of Parent; give Child its own @@query.order(...), or clear it with @@query.order())"), "{error}");
        assert!(lowered(false, None, json!(["visible"]), Some(json!([]))).is_ok());
        assert!(lowered(false, None, json!(["id nulls first x"]), None).unwrap_err().contains("write \"[-]field [nulls first|last]\""));
    }
    #[test]
    fn an_order_error_names_the_model_that_set_the_order() {
        // A proxy narrows a nullable column; the order is the child's own.
        let mut schema = ir();
        declare(&mut schema, "Child", "query.order", json!({}), json!(["bio"]));
        schema.behavior.model_views.push(serde_json::from_value(json!({"model":"Child","source":"Parent","root":"Parent"})).unwrap());
        schema.behavior.result_checks.push(serde_json::from_value(json!({"model":"Child","field":"bio","non_null":true,"code":"orm.proxy.shape"})).unwrap());
        let error = lower(&mut schema).unwrap_err();
        assert!(error.ends_with("Child: order column \"bio\" can be NULL: write \"bio nulls first\" or \"bio nulls last\""), "{error}");
        // Two levels down, the error still names the model that set the order.
        let mut schema = ir();
        let mut grandchild = serde_json::to_value(&schema.models[1]).unwrap();
        grandchild["name"] = json!("Grandchild");
        grandchild["fields"].as_array_mut().unwrap().retain(|f| f["name"] != "visible");
        schema.models.push(serde_json::from_value(grandchild).unwrap());
        declare(&mut schema, "Parent", "query.order", json!({}), json!(["visible"]));
        declare(&mut schema, "Grandchild", "query.parent", json!({}), json!(["Child"]));
        let error = lower(&mut schema).unwrap_err();
        assert!(error.contains("Grandchild: order column \"visible\" is not a field of the model (from the default order of Parent;"), "{error}");
    }
    #[test]
    fn query_parent_wins_over_the_proxy_parent() {
        let mut schema = ir();
        let mut other = serde_json::to_value(&schema.models[0]).unwrap();
        other["name"] = json!("Other");
        schema.models.push(serde_json::from_value(other).unwrap());
        declare(&mut schema, "Other", "query.order", json!({}), json!(["-id"]));
        schema.behavior.model_views.push(serde_json::from_value(json!({"model":"Child","source":"Parent","root":"Parent"})).unwrap());
        schema.behavior.declarations[1].arguments.insert("parent".into(), json!("Other"));
        schema.behavior.declarations[1].arguments.remove("filter");
        lower(&mut schema).unwrap();
        assert_eq!(schema.behavior.read_defaults[1].order, schema.behavior.read_defaults[2].order);
    }
    #[test]
    fn a_proxy_inherits_the_default_order() {
        let mut schema = ir();
        schema.behavior.declarations.retain(|d| d.model != "Child");
        declare(&mut schema, "Parent", "query.order", json!({}), json!(["-visible", "id"]));
        schema.behavior.model_views.push(serde_json::from_value(json!({"model":"Child","source":"Parent","root":"Parent"})).unwrap());
        lower(&mut schema).unwrap();
        assert_eq!(schema.behavior.read_defaults[1].order, schema.behavior.read_defaults[0].order);
        assert_eq!(schema.behavior.read_defaults[1].order.as_ref().unwrap().len(), 2);
    }
    fn named(schema: &mut SchemaIr, model: &str, name: &str, expr: &str) { declare(schema, model, "query.filter", json!({}), json!([name, expr])); }
    fn entries(schema: &SchemaIr, model: &str) -> Vec<(String, Option<Value>)> {
        schema.behavior.read_defaults.iter().filter(|d| d.model == model).map(|d| (d.name.clone(), d.filter.clone())).collect()
    }
    #[test]
    fn named_filters_are_separate_entries_beside_the_query_entry() {
        let mut schema = ir();
        named(&mut schema, "Parent", "tenant", "id > 1");
        named(&mut schema, "Parent", "published", "visible == true");
        lower(&mut schema).unwrap();
        let parent = entries(&schema, "Parent");
        assert_eq!(parent.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["published", "tenant", POLICY]);
        assert_eq!(parent[0].1.as_ref().unwrap()["t"], "cmp");
        assert!(schema.behavior.read_defaults.iter().all(|d| d.source == SOURCE));
        // The `query` entry keeps the defaults(filter:) filter, fields and order.
        assert_eq!(parent[2].1.as_ref().unwrap()["t"], "cmp");
        let before = serde_json::to_value(&schema).unwrap(); lower(&mut schema).unwrap();
        assert_eq!(before, serde_json::to_value(&schema).unwrap(), "lowering twice is stable");
    }
    #[test]
    fn a_child_inherits_each_name_replaces_one_and_removes_one() {
        let mut schema = ir();
        named(&mut schema, "Parent", "tenant", "id > 1");
        named(&mut schema, "Parent", "published", "visible == true");
        named(&mut schema, "Parent", "extra", "id > 5");
        named(&mut schema, "Child", "published", "parent.default_filter and id > 9");
        named(&mut schema, "Child", "extra", "none");
        lower(&mut schema).unwrap();
        let child = entries(&schema, "Child");
        let names: Vec<&str> = child.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["published", "tenant", POLICY], "extra is removed, tenant is inherited");
        assert_eq!(child[0].1.as_ref().unwrap()["t"], "and", "same name replaces, and can extend the parent's");
        assert_eq!(child[1].1, entries(&schema, "Parent").into_iter().find(|(n, _)| n == "tenant").unwrap().1);
    }
    #[test]
    fn named_filter_errors() {
        let error = |args: Value| { let mut s = ir(); declare(&mut s, "Parent", "query.filter", json!({}), args); lower(&mut s).unwrap_err() };
        assert!(error(json!(["id > 1"])).contains("@@query.filter takes a name and an expression: @@query.filter(\"<name>\", \"<expr>\")"));
        assert!(error(json!(["a", "b", "c"])).contains("takes a name and an expression"));
        assert!(error(json!(["bad name", "id > 1"])).contains("must be letters, digits and underscores"));
        assert!(error(json!([POLICY, "id > 1"])).contains("reserved"));
        assert!(error(json!(["a", "id >"])).contains("@@query.filter \"a\":"));
        let mut s = ir(); named(&mut s, "Parent", "a", "id > 1"); named(&mut s, "Parent", "a", "id > 2");
        assert!(lower(&mut s).unwrap_err().contains("@@query.filter \"a\" is given twice"));
    }
    #[test]
    fn scope_values() {
        let filter = Parser::parse("shop_id == scope.shop and active == true", None).unwrap();
        assert_eq!(filter["items"][0]["r"], json!({"t":"scope","name":"shop"}));
        assert!(Parser::parse("shop_id == scope.", None).is_err());
        assert!(Parser::parse("shop_id == scope.a.b", None).is_err());
    }
    #[test]
    fn operator_precedence_and_null() {
        let not = Parser::parse("not id == 1", None).unwrap();
        assert_eq!((not["t"].as_str(), not["item"]["t"].as_str()), (Some("not"), Some("cmp")));
        assert_eq!(Parser::parse("! visible", None).unwrap()["item"]["t"], "col");
        assert_eq!(Parser::parse("a or b and c", None).unwrap()["items"][1]["t"], "and");
        assert_eq!(Parser::parse("a and b or c", None).unwrap()["items"][0]["t"], "and");
        assert_eq!(Parser::parse("a != null", None).unwrap()["neg"], true);
        assert_eq!(Parser::parse("a == null", None).unwrap()["neg"], false);
        assert!(Parser::parse("a < null", None).is_err());
    }
}

// The package entry point; Rust callers keep the rlib API.
orm_contracts::extension!(("query.defaults.lower", lower));
