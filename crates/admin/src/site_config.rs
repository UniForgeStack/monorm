//! The tagged values of the config wire, lowered to the plain config that `site.rs` reads.
//!
//! A host sends a value that holds a host object as a tagged object: `{"$qs": {...}}` is a prepared query set,
//! `{"$fn": "<local id>"}` is a host function, `{"$ref": {"model", "path"}}` is a column or a relation path, `{"$ir": ir}`
//! is a condition or an expression of the engine IR (it holds `{"t": "slot", "name", "transform"}` nodes where the host
//! wrote `param("user")`), and `{"$host": "<type>"}` is a value of a host type that the config does not take.
//! Rust derives the host name of each function (`queryset.<M>`,
//! `search.<M>`, `column.<M>.<name>`, with the inline tag `@<Parent>.<Child>[.<via>]` after `<M>` for a page that an
//! inline wrapper overrides) and returns the table of local id to host name. The binding registers each function
//! under the name of its id. A value without a tag is the plain config and passes through unchanged.
//!
//! The public structs below the functions are the plain config that `Site::build` parses: one place for every key.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{code, hint, Fault, Hint, PathPart, Spelling};
use monorm_core::schema::Schema;

/// The host name of each local function id.
pub type Handles = BTreeMap<String, String>;

/// The body of a `{"$qs": ...}` value: a prepared query set as the admin takes it. The flags are the calls that the
/// admin refuses; the host sets each one that its query set holds.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Qs {
    /// The model of the query set. It must be the model of the resource.
    pub model: Option<String>,
    /// The condition of the query set as a `{"$ir": ...}` value. Absent: the query set has no filter, which the admin refuses.
    pub filter: Option<Value>,
    /// The order: each item is a plain name, or a `$ref` with `desc` beside it.
    #[serde(default)]
    pub order: Vec<Value>,
    /// The query set loads related rows (`load()`, a field list, `related`, `prefetch`).
    #[serde(default)]
    pub load: bool,
    /// The query set has a limit or an offset.
    #[serde(default)]
    pub slice: bool,
    /// The query set has its own `from` or joins.
    #[serde(default)]
    pub join: bool,
    /// The query set locks rows.
    #[serde(default)]
    pub lock: bool,
    /// The query set drops the model defaults.
    #[serde(default)]
    pub without_defaults: bool,
}

impl Qs {
    /// The call that the query set holds and the admin refuses.
    fn refused(&self) -> Option<String> {
        let flags = [
            (self.load, "load()".to_owned()),
            (self.slice, "a slice".to_owned()),
            (self.join, hint(Hint::FromJoin)),
            (self.lock, "lock()".to_owned()),
            (self.without_defaults, hint(Hint::WithoutDefaults)),
        ];
        flags.into_iter().find_map(|(set, what)| set.then_some(what))
    }
}

/// Lowers the tagged values of `config` in place and returns the function table.
pub(crate) fn lower(config: &mut Value, schema: &Schema) -> Result<Handles, Fault> {
    let mut handles = Handles::new();
    let Some(resources) = config.get_mut("resources").and_then(Value::as_array_mut) else { return Ok(handles) };
    for resource in resources {
        let Some(model) = resource.get("model").and_then(Value::as_str).map(str::to_owned) else { continue };
        lower_page(resource, &model, "", schema, &mut handles).map_err(|f| f.under(PathPart::Model(model.clone())))?;
        let Some(inlines) = resource.get_mut("inlines").and_then(Value::as_array_mut) else { continue };
        for inline in inlines {
            let Some(child) = inline.get("model").and_then(Value::as_str).map(str::to_owned) else { continue };
            let via = inline.get("via").and_then(Value::as_str).map(|v| format!(".{v}")).unwrap_or_default();
            let tag = format!("@{model}.{child}{via}");
            if let Some(overrides) = inline.get_mut("page").filter(|p| p.is_object()) {
                lower_page(overrides, &child, &tag, schema, &mut handles).map_err(|f| f.under(PathPart::Model(child.clone())))?;
            }
        }
    }
    Ok(handles)
}

/// The tag of a value: `Some((tag, body))` for an object with a key that starts with `$`. The other keys are siblings.
fn tagged(value: &Value) -> Option<(&str, &Value)> {
    let (key, body) = value.as_object()?.iter().find(|(key, _)| key.starts_with('$'))?;
    Some((key.as_str(), body))
}

fn lower_page(page: &mut Value, model: &str, tag: &str, schema: &Schema, handles: &mut Handles) -> Result<(), Fault> {
    let Some(map) = page.as_object_mut() else { return Ok(()) };
    if let Some(value) = map.remove("queryset") {
        let (queryset, order) = queryset(model, tag, value, handles)?;
        map.insert("queryset".into(), queryset);
        if let Some(order) = order {
            let list = map.entry("list").or_insert_with(|| json!({}));
            let Some(list) = list.as_object_mut() else { return Ok(()) };
            if list.contains_key("order") {
                return Err(Fault::new(code::INVALID_VALUE, "the order is in the queryset or in the list, not in both"));
            }
            list.insert("order".into(), order);
        }
    }
    if let Some(list) = map.get_mut("list").and_then(Value::as_object_mut) {
        lower_list(list, model, tag, handles)?;
    }
    if let Some(rules) = map.get_mut("can").and_then(Value::as_object_mut) {
        for (name, rule) in rules.iter_mut() {
            *rule = ir_of(rule, &format!("can '{name}'"), Slots::User)?.0;
        }
    }
    let mut listed = std::collections::BTreeSet::new();
    for action in map.get_mut("actions").and_then(Value::as_array_mut).into_iter().flatten() {
        let Some(action) = action.as_object_mut() else { continue };
        let name = action.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
        if !listed.insert(name.clone()) {
            return Err(Fault::new(code::DUPLICATE, format!("action '{name}' is listed twice")));
        }
        if let Some(rule) = action.get_mut("applies_to") {
            *rule = ir_of(rule, &format!("action '{name}' applies_to"), Slots::None)?.0;
        }
        if let Some(rule) = action.get_mut("can") {
            *rule = ir_of(rule, &format!("action '{name}' can"), Slots::User)?.0;
        }
    }
    for (op, entry) in map.get_mut("service").and_then(Value::as_object_mut).into_iter().flatten() {
        lower_callable(entry, format!("service.{model}.{op}"), format!("{op} is a service function (ctx, ...)"), handles)?;
    }
    for action in map.get_mut("actions").and_then(Value::as_array_mut).into_iter().flatten() {
        let Some(action) = action.as_object_mut() else { continue };
        let Some(mut function) = action.remove("function") else { continue };
        let name = action.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
        if tagged(&function).is_none() {
            return Err(Fault::new(code::INVALID_VALUE, format!("action '{name}' needs a function")));
        }
        lower_callable(&mut function, format!("action.{model}.{name}"), format!("action '{name}' needs a function"), handles)?;
        if let Some(flags) = function.as_object() {
            action.extend(flags.clone());
        }
    }
    if let Some(relations) = map.get_mut("relations").and_then(Value::as_object_mut) {
        for (name, options) in relations.iter_mut() {
            let Some(choices) = options.get_mut("choices") else { continue };
            let at = || Fault::new(code::INVALID_VALUE, format!("relation {name:?}: choices takes a prepared query set of the target"));
            if tagged(choices).is_none() {
                // An object without a tag is the plain config; any other value is not a query set.
                if choices.is_object() { continue } else { return Err(at()) }
            }
            let Some(("$qs", body)) = tagged(choices) else { return Err(at()) };
            let target = schema.model_idx(model).ok().and_then(|idx| schema.model(idx).relation(name).ok()).map(|(_, t)| schema.model(t).ir.name.clone());
            if let (Some(target), Some(of)) = (&target, body.get("model").and_then(Value::as_str)) {
                if target != of {
                    return Err(Fault::new(code::INVALID_VALUE, format!("relation {name:?}: choices is a query set of {of}, not of {target}")));
                }
            }
            let target = target.unwrap_or_else(|| model.to_owned());
            let (lowered, _) = queryset(&target, "", choices.clone(), handles)?;
            *choices = lowered;
        }
    }
    Ok(())
}

/// The host name of a `{"$fn": id}` value, with the id in the table. `None` for a plain name.
fn host_function(value: &Value, name: String, handles: &mut Handles) -> Result<Option<String>, Fault> {
    match tagged(value) {
        None => Ok(None),
        Some(("$fn", Value::String(id))) => {
            if handles.iter().any(|(other, taken)| *taken == name && other != id) {
                return Err(Fault::new(code::INVALID_VALUE, format!("two host functions get the name {name:?}: one inline model twice under one parent needs a different via")));
            }
            handles.insert(id.clone(), name.clone());
            Ok(Some(name))
        }
        Some((other, _)) => Err(Fault::new(code::INVALID_VALUE, format!("{other:?} is not a host function: a function is {{\"$fn\": id}}"))),
    }
}

/// The `sync` and `threaded` flags that sit beside a `{"$fn": id}`, and the host name of the function.
/// A value with no tag is a plain config and stays. A `$host` value is not a function: `refusal` is its text.
fn lower_callable(value: &mut Value, name: String, refusal: String, handles: &mut Handles) -> Result<(), Fault> {
    match tagged(value) {
        None => Ok(()),
        Some(("$fn", _)) => {
            let flag = |key: &str| value.get(key).and_then(Value::as_bool).unwrap_or(false);
            let flags = json!({"sync": flag("sync"), "threaded": flag("threaded")});
            host_function(value, name, handles)?;
            *value = flags;
            Ok(())
        }
        Some(_) => Err(Fault::new(code::INVALID_VALUE, refusal)),
    }
}

/// The plain `queryset` config of a tagged queryset, and the order that a prepared query set sets.
fn queryset(model: &str, tag: &str, value: Value, handles: &mut Handles) -> Result<(Value, Option<Value>), Fault> {
    let wrong = || Fault::new(code::INVALID_VALUE, format!("queryset takes a prepared query set ({}) or an async function", hint(Hint::PreparedQuerySet)));
    let Some((key, body)) = tagged(&value) else {
        // The plain config of a filter or of a function name.
        return if value.is_object() { Ok((value, None)) } else { Err(wrong()) };
    };
    match key {
        "$fn" => {
            let name = host_function(&value, format!("queryset.{model}{tag}"), handles)?.ok_or_else(wrong)?;
            Ok((json!({"function": name}), None))
        }
        "$qs" => {
            let qs: Qs = serde_json::from_value(body.clone()).map_err(|e| Fault::new(code::INVALID_VALUE, format!("queryset: invalid prepared query set: {e}")))?;
            if let Some(of) = qs.model.as_deref().filter(|of| *of != model) {
                return Err(Fault::new(code::INVALID_VALUE, format!("queryset is a query set of {of}")));
            }
            if let Some(what) = qs.refused() {
                return Err(Fault::new(code::UNSUPPORTED, format!("queryset takes a filter and an order; it does not take {what}")));
            }
            let filter = qs.filter.as_ref().filter(|f| !f.is_null()).ok_or_else(|| Fault::new(code::INVALID_VALUE, "queryset needs a filter"))?;
            let (filter, used) = ir_of(filter, "queryset", Slots::User)?;
            let slots: Vec<&str> = if used { vec!["user"] } else { vec![] };
            let order = lower_order(&qs.order, model, "queryset order")?;
            Ok((json!({"filter": filter, "slots": slots}), (!order.is_empty()).then_some(Value::Array(order))))
        }
        "$host" => Err(Fault::new(code::INVALID_VALUE, format!("queryset takes a prepared query set ({}) or an async function, not a value of type {}", hint(Hint::PreparedQuerySet), type_of(body)))),
        _ => Err(wrong()),
    }
}

/// The type name that a `{"$host": "<type>"}` value carries.
fn type_of(body: &Value) -> &str {
    body.as_str().unwrap_or("unknown")
}

/// Which request slots a condition takes: none, or `param("user")`, which is parameter 0.
#[derive(Clone, Copy, PartialEq)]
enum Slots {
    None,
    User,
}

/// The plain IR of an `{"$ir": ir}` value, and whether it holds the user slot. An untagged value is plain IR already.
fn ir_of(value: &Value, what: &str, slots: Slots) -> Result<(Value, bool), Fault> {
    let node = match tagged(value) {
        Some(("$ir", body)) => body,
        Some(("$host", t)) => return Err(Fault::new(code::INVALID_VALUE, format!("{what} takes a condition or an expression, not a value of type {}", type_of(t)))),
        Some((other, _)) => return Err(Fault::new(code::INVALID_VALUE, format!("{what} takes a condition or an expression: {other:?} is not one"))),
        None => value,
    };
    let mut used = false;
    let ir = walk_ir(node, what, slots, &mut used)?;
    Ok((ir, used))
}

fn walk_ir(node: &Value, what: &str, slots: Slots, used: &mut bool) -> Result<Value, Fault> {
    match node {
        Value::Array(items) => items.iter().map(|n| walk_ir(n, what, slots, used)).collect::<Result<Vec<_>, _>>().map(Value::Array),
        Value::Object(map) => {
            if let Some(t) = map.get("$host") {
                return Err(Fault::new(code::INVALID_VALUE, format!("{what}: a value of type {} is not a literal of the admin; use an integer, a string or a boolean", type_of(t))));
            }
            if map.get("t").and_then(Value::as_str) == Some("slot") {
                let name = map.get("name").and_then(Value::as_str).unwrap_or_default();
                if slots == Slots::None {
                    return Err(Fault::new(code::INVALID_VALUE, format!("{what}: param('{name}') is a placeholder of a prepared query; it takes no request values here")));
                }
                if name != "user" || map.get("transform").and_then(Value::as_bool) == Some(true) {
                    return Err(Fault::new(code::INVALID_VALUE, format!("{what}: takes the params 'user', not param('{name}')")));
                }
                *used = true;
                return Ok(json!({"t": "param", "i": 0}));
            }
            map.iter().map(|(k, v)| Ok((k.clone(), walk_ir(v, what, slots, used)?))).collect::<Result<serde_json::Map<_, _>, Fault>>().map(Value::Object)
        }
        other => Ok(other.clone()),
    }
}

/// The dotted path of a `{"model", "path"}` reference to a column or a relation of `model`.
fn ref_path(body: &Value, model: &str) -> Result<String, Fault> {
    let (of, path) = (body.get("model").and_then(Value::as_str), body.get("path").and_then(Value::as_str));
    let (Some(of), Some(path)) = (of, path) else { return Err(Fault::new(code::INVALID_VALUE, "a reference is {\"model\", \"path\"}")) };
    if of != model {
        return Err(Fault::new(code::INVALID_VALUE, format!("{of}.{path} belongs to {of}, not to {model}")));
    }
    Ok(path.to_owned())
}

/// The name that a column or relation position holds. `None` for a plain name, which stays as it is.
fn name_of(value: &Value, model: &str, what: &str) -> Result<Option<String>, Fault> {
    match tagged(value) {
        None => Ok(None),
        Some(("$ref", _)) if value.as_object().is_some_and(|m| m.len() > 1) => {
            Err(Fault::new(code::INVALID_VALUE, format!("{what}: a reference of {model} takes no key beside \"$ref\"")))
        }
        Some(("$ref", body)) => ref_path(body, model).map(Some),
        Some(("$host", t)) => Err(Fault::new(code::INVALID_VALUE, format!("{what}: a value of type {} is not a column or a relation of {model}", type_of(t)))),
        Some((other, _)) => Err(Fault::new(code::INVALID_VALUE, format!("{what}: {other:?} is not a column or a relation of {model}"))),
    }
}

/// Replaces each tagged item of the list at `key` by its dotted path.
fn lower_names(map: &mut serde_json::Map<String, Value>, key: &str, model: &str) -> Result<(), Fault> {
    for item in map.get_mut(key).and_then(Value::as_array_mut).into_iter().flatten() {
        if let Some(name) = name_of(item, model, key)? {
            *item = Value::String(name);
        }
    }
    Ok(())
}

/// Turns the list of `[key, value]` pairs at `key` into an object. A key is a name, or a `$ref` to a column or a relation of `model`.
/// A plain object stays as it is.
fn lower_pairs(map: &mut serde_json::Map<String, Value>, key: &str, model: &str) -> Result<(), Fault> {
    let Some(pairs) = map.get(key).and_then(Value::as_array) else { return Ok(()) };
    let mut out = serde_json::Map::new();
    for pair in pairs {
        let Some([k, v]) = pair.as_array().map(Vec::as_slice) else {
            return Err(Fault::new(code::INVALID_VALUE, format!("{key}: a pair is [column, value]")));
        };
        let name = match name_of(k, model, key)? {
            Some(name) => name,
            None => k.as_str().map(str::to_owned).ok_or_else(|| Fault::new(code::INVALID_VALUE, format!("{key}: {k} is not a column or a relation of {model}")))?,
        };
        out.insert(name, v.clone());
    }
    map.insert(key.into(), Value::Object(out));
    Ok(())
}

/// An order as `"-name"` strings. An item is a plain string, or a `$ref` with `desc` and `nulls` beside it.
fn lower_order(items: &[Value], model: &str, what: &str) -> Result<Vec<Value>, Fault> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some((tag, body)) = tagged(item) else {
            out.push(item.clone());
            continue;
        };
        match tag {
            "$ref" => {
                if let Some(key) = item.as_object().and_then(|m| m.keys().find(|k| !matches!(k.as_str(), "$ref" | "desc" | "nulls"))) {
                    return Err(Fault::new(code::INVALID_VALUE, format!("{what} takes no key {key:?} beside \"$ref\"")));
                }
                if item.get("nulls").is_some_and(|n| !n.is_null()) {
                    return Err(Fault::new(code::INVALID_VALUE, "order does not take a NULLs placement"));
                }
                let desc = item.get("desc").and_then(Value::as_bool) == Some(true);
                out.push(Value::String(format!("{}{}", if desc { "-" } else { "" }, ref_path(body, model)?)));
            }
            "$host" => return Err(Fault::new(code::INVALID_VALUE, format!("{what} takes columns, not a value of type {}", type_of(body)))),
            _ => return Err(Fault::new(code::INVALID_VALUE, format!("{what} takes columns, not an expression"))),
        }
    }
    Ok(out)
}

fn lower_list(list: &mut serde_json::Map<String, Value>, model: &str, tag: &str, handles: &mut Handles) -> Result<(), Fault> {
    for key in ["columns", "filters", "tabs", "editable", "sortable", "search"] {
        lower_names(list, key, model)?;
    }
    for key in ["labels", "empty_fields", "badges"] {
        lower_pairs(list, key, model)?;
    }
    if let Some(items) = list.get("order").and_then(Value::as_array) {
        let order = lower_order(items, model, "order")?;
        list.insert("order".into(), Value::Array(order));
    }
    if let Some(value) = list.get("date_hierarchy") {
        match value.as_object().filter(|_| tagged(value).is_none()) {
            Some(full) => {
                if let Some(name) = full.get("field").map(|f| name_of(f, model, "date_hierarchy")).transpose()?.flatten() {
                    if let Some(map) = list.get_mut("date_hierarchy").and_then(Value::as_object_mut) {
                        map.insert("field".into(), Value::String(name));
                    }
                }
            }
            None => {
                if let Some(name) = name_of(value, model, "date_hierarchy")? {
                    list.insert("date_hierarchy".into(), Value::String(name));
                }
            }
        }
    }
    if let Some(value) = list.get("search_with") {
        if let Some(name) = host_function(value, format!("search.{model}{tag}"), handles)? {
            list.insert("search_with".into(), Value::String(name));
        }
    }
    for filter in list.get_mut("named_filters").and_then(Value::as_array_mut).into_iter().flatten() {
        let name = filter.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
        for choice in filter.get_mut("choices").and_then(Value::as_array_mut).into_iter().flatten() {
            if let Some(expr) = choice.get_mut("expr") {
                *expr = ir_of(expr, &format!("named filter '{name}'"), Slots::None)?.0;
            }
        }
    }
    for item in list.get_mut("computed").and_then(Value::as_array_mut).into_iter().flatten() {
        let Some(item) = item.as_object_mut() else { continue };
        let name = item.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
        if let Some(value) = item.get("function") {
            if let Some(host) = host_function(value, format!("column.{model}{tag}.{name}"), handles)? {
                item.insert("function".into(), Value::String(host));
            }
        }
        if let Some(expr) = item.get_mut("expr") {
            *expr = ir_of(expr, &format!("computed '{name}'"), Slots::None)?.0;
        }
        if let Some(by) = item.get_mut("order_by") {
            let at = || Fault::new(code::INVALID_VALUE, format!("computed '{name}': order_by takes a field of {model} or an expression"));
            match tagged(by) {
                Some(("$ref", body)) => *by = Value::String(ref_path(body, model)?),
                Some(("$ir", _)) => *by = ir_of(by, &format!("computed '{name}' order_by"), Slots::None)?.0,
                Some(_) => return Err(at()),
                None => {}
            }
        }
    }
    for item in list.get_mut("aggregates").and_then(Value::as_array_mut).into_iter().flatten() {
        let Some(item) = item.as_object_mut() else { continue };
        let name = item.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
        if let Some(relation) = item.get("relation") {
            let at = || Fault::new(code::INVALID_VALUE, format!("aggregate '{name}': relation takes a relation such as {model}.<relation>"));
            match tagged(relation) {
                Some(("$ref", body)) => {
                    let path = ref_path(body, model)?;
                    item.insert("relation".into(), Value::String(path));
                }
                Some(_) => return Err(at()),
                None => {}
            }
        }
        let relation = item.get("relation").and_then(Value::as_str).unwrap_or_default().to_owned();
        if let Some(field) = item.get("field").filter(|f| tagged(f).is_some()) {
            let at = || Fault::new(code::INVALID_VALUE, format!("aggregate '{name}': field takes a field of the relation '{relation}'"));
            let Some(("$ref", body)) = tagged(field) else { return Err(at()) };
            let path = ref_path(body, model)?;
            let Some(field) = path.strip_prefix(&format!("{relation}.")).filter(|f| !f.contains('.')) else { return Err(at()) };
            item.insert("field".into(), Value::String(field.to_owned()));
        }
    }
    Ok(())
}

// ---- The config structs: what `site.rs` and the modules read after `lower`. ----

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// How the host spells an option key in an error text: `snake` (default) or `camel`. `Site::build` reads it first.
    #[allow(dead_code)] // `Site::build` reads it before this parse; the field only lets the key through `deny_unknown_fields`.
    pub spelling: Option<Spelling>,
    /// The table of the active auth user model, supplied by the binding.
    pub user_table: Option<String>,
    /// The path under which the UI and the API live, `/admin` by default.
    pub base_path: Option<String>,
    /// The path prefix of every API route: `<base_path>/api` by default, and always under `base_path`.
    pub prefix: Option<String>,
    /// `page` (default), `model` or `access`.
    pub permissions: Option<String>,
    /// The label of the permission names, `admin` by default.
    pub label: Option<String>,
    /// The name, logo, colors, header links and environment label of the UI.
    pub brand: Option<BrandConfig>,
    /// The title, logo and brand colour of the UI: the short form of `brand`.
    pub theme: Option<ThemeConfig>,
    /// The navigation groups in order. A resource with a `group` must name one of them.
    #[serde(default)]
    pub groups: Vec<String>,
    /// A lucide icon name for each navigation group, by group name.
    #[serde(default)]
    pub group_icons: BTreeMap<String, String>,
    /// The service switcher of the code: the current service, the services and the build identity.
    pub switcher: Option<SwitcherConfig>,
    /// The `[admin.switcher]` table of `monorm.toml`, as the serve mode hands it to the code. A key set here and in `switcher` is an error.
    pub switcher_file: Option<SwitcherConfig>,
    /// The registered resources. None: every concrete model is a read-only resource.
    #[serde(default)]
    pub resources: Vec<ResourceConfig>,
    /// The time limit of each statement of a request, in seconds (PostgreSQL). 0: none. Default 15.
    pub statement_timeout: Option<f64>,
    /// True when the host gave the admin a file storage registry: a resource may then list `files`.
    #[serde(default)]
    pub storage: bool,
    /// How long an export waits for the client to take the next chunk, in ms. 0: no limit. Default 30000.
    pub export_idle_timeout_ms: Option<u64>,
    /// Widgets for every resource: `Model.field`, `Type.subfield`, or a type name, to a widget name.
    #[serde(default)]
    pub widgets: BTreeMap<String, String>,
    /// Custom routes under `<prefix>/routes/`.
    #[serde(default)]
    pub routes: Vec<RouteConfig>,
    /// Extension packages: static widgets, form modules and styles under `<prefix>/packages/`.
    #[serde(default)]
    pub packages: Vec<PackageConfig>,
    #[serde(default)]
    pub frame_origins: Vec<String>,
    #[serde(default)]
    pub frame_cards: Vec<FrameConfig>,
    #[serde(default)]
    pub embeds: BTreeMap<String, EmbedConfig>,
}

/// The rows a resource shows to staff.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuerysetConfig {
    /// A prepared filter. A `param` with index `i` reads `slots[i]`.
    pub filter: Option<Value>,
    /// What each parameter holds at a request: `user` (the key of the staff user).
    #[serde(default)]
    pub slots: Vec<String>,
    /// A host function that returns `{"filter": expr}` at each request, instead of `filter`.
    pub function: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceConfig {
    pub model: String,
    #[serde(default)]
    pub list: ListConfig,
    /// The operations that the resource offers, for every user: `read`, `create`, `update`, `delete`.
    pub allow: Option<Vec<String>>,
    /// Page levels: `read`, `manage` and, to split delete from manage, `delete`.
    pub perms: Option<Vec<String>>,
    /// Overrides the site label in the names of this resource.
    pub label: Option<String>,
    /// The name in the navigation and the page heading. The model name, spaced, by default.
    pub title: Option<String>,
    /// The navigation group of the resource.
    pub group: Option<String>,
    /// A lucide icon name (`book-open`) for the navigation item.
    pub icon: Option<String>,
    /// Names of field permissions (`see_cost`): `<label>.<name>`.
    #[serde(default)]
    pub field_permissions: Vec<String>,
    /// Field to permission name: only a user with that name sees the field. A bound name is declared as a field permission.
    #[serde(default)]
    pub see: BTreeMap<String, String>,
    /// Field to permission name: only a user with that name changes the field, the others see it read-only.
    #[serde(default)]
    pub change: BTreeMap<String, String>,
    /// Actions: a permission name (`<label>.<action>_<resource>`), a placement, a confirmation and inputs.
    #[serde(default)]
    pub actions: Vec<ActionConfig>,
    /// Bulk delete is on by default: `false` removes it from the list.
    #[serde(default = "yes")]
    pub bulk_delete: bool,
    #[serde(default)]
    pub set_password: bool,
    /// The rows that staff see and touch.
    pub queryset: Option<QuerysetConfig>,
    /// Per-row rules, `change` and `delete`: prepared expressions that the list and the detail select as flags.
    #[serde(default)]
    pub can: BTreeMap<String, Value>,
    /// The text of a form message for a named `@@check`: the name of the constraint, then the text.
    #[serde(default)]
    pub check_messages: BTreeMap<String, String>,
    /// Host functions that run the writes: `create`, `update`, `delete`, each with `sync` for a plain Python function.
    #[serde(default)]
    pub service: ServiceConfig,
    /// Controls that a form shows next to the fields. Only a service reads them.
    #[serde(default)]
    pub inputs: Vec<InputConfig>,
    /// The resource opts in to writes of a protected model.
    #[serde(default)]
    pub allow_protected_writes: bool,
    /// The model has host behavior that the host knows (a field adapter): its writes run through the host.
    #[serde(default)]
    pub host_behavior: bool,
    /// Sections: a list, or `{create, edit}` with a list for each form. A field that no section names goes last.
    pub form: Option<Value>,
    /// Fields that the form shows and a request cannot write.
    #[serde(default)]
    pub readonly: Vec<String>,
    /// Bytes fields that the host shows as another value: the field to `json` (the host converts both ways) or `text` (read-only).
    #[serde(default)]
    pub views: BTreeMap<String, String>,
    /// Widgets of this resource: a field, `Type.subfield`, or a type name, to a widget name.
    #[serde(default)]
    pub widgets: BTreeMap<String, String>,
    /// The path of a JavaScript module that replaces the form.
    pub form_component: Option<String>,
    /// Package detail tabs (`<package>.<tab>`) that the detail page shows after the relation tabs.
    #[serde(default)]
    pub detail_tabs: Vec<String>,
    #[serde(default)]
    pub frames: Vec<FrameConfig>,
    #[serde(default)]
    pub fieldsets: Vec<Fieldset>,
    /// Relation fields with a picker (to-one) or chips (many-to-many), by relation name.
    #[serde(default)]
    pub relations: BTreeMap<String, RelationConfig>,
    /// Json fields that hold an uploaded file: the form uploads and downloads them.
    #[serde(default)]
    pub files: Vec<String>,
    /// To-many relations whose rows the detail page edits in place. `Site::build` merges the page of the child into each one.
    #[serde(default)]
    pub inlines: Vec<InlineConfig>,
}

/// The wrapper of an inline: what a page cannot say. `page` holds the page keys that the wrapper overrides, merged over the registered page of the child model.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InlineConfig {
    pub model: String,
    /// The to-many relation of the parent that leads to the child model. Needed when more than one does.
    pub via: Option<String>,
    /// `with_parent` (default): the rows save with the parent in one change set. `row`: each row saves alone.
    pub save: Option<String>,
    /// The name of a permission (`<label>.<name>`) that replaces the write permission of the parent.
    pub permission: Option<String>,
    /// `table` (default) or `stacked`.
    pub style: Option<String>,
    /// The most rows an inline shows and holds. Default 20, at most 200.
    pub max: Option<usize>,
    pub label: Option<String>,
    #[serde(default)]
    pub collapsed: bool,
    /// Whether a row can be deleted. Default true.
    pub delete: Option<bool>,
    pub page: Option<Box<ResourceConfig>>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ListConfig {
    /// Field names; `author.name` reaches a field through to-one relations. A computed or aggregate column is named here too.
    pub columns: Option<Vec<String>>,
    /// Field names of the model, `-created_at` for descending.
    pub order: Option<Vec<String>>,
    pub page_size: Option<usize>,
    /// Field names of the model that the search box reads.
    pub search: Option<Vec<String>>,
    /// The name of a host function that replaces the search: it takes the text and returns keys.
    pub search_with: Option<String>,
    /// Field names whose cells the list edits in place: list columns that are scalar fields of the model.
    #[serde(default)]
    pub editable: Vec<String>,
    /// Field names that the sort headers may use. Default: the list columns that can sort.
    pub sortable: Option<Vec<String>>,
    /// The title of a column, by column name.
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// What an empty value shows in every column of the list.
    pub empty: Option<String>,
    /// What an empty value shows in one column, by column name.
    #[serde(default)]
    pub empty_fields: BTreeMap<String, String>,
    /// Fields that the filter bar offers: a field, `author.name` (a field of a to-one relation) or a to-one relation.
    #[serde(default)]
    pub filters: Vec<String>,
    /// Filters made of prepared expressions: one choice at a time.
    #[serde(default)]
    pub named_filters: Vec<NamedFilterConfig>,
    /// Columns that a SQL expression or a host function computes.
    #[serde(default)]
    pub computed: Vec<ComputedConfig>,
    /// Columns that count or sum the rows of a to-many relation.
    #[serde(default)]
    pub aggregates: Vec<AggregateConfig>,
    /// The to-many relations that the detail page shows as tabs. Default: every one that leads to a resource.
    pub tabs: Option<Vec<String>>,
    /// A column name to a map of value (as text) to a colour of the palette: the list and the detail page show the value as a badge.
    #[serde(default)]
    pub badges: BTreeMap<String, BTreeMap<String, String>>,
    /// A date or datetime field: the list shows a bar of the years, months and days with their row counts.
    pub date_hierarchy: Option<DateHierarchyConfig>,
}

/// `date_hierarchy: "created"` or `date_hierarchy: {field: "created", counts: false}`.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum DateHierarchyConfig {
    Field(String),
    Full(DateHierarchyFull),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DateHierarchyFull {
    pub field: String,
    /// False: the bar shows the periods without counts and the route runs no counting statement.
    pub counts: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedFilterConfig {
    pub name: String,
    pub label: Option<String>,
    pub choices: Vec<NamedChoiceConfig>,
    /// The choice that applies when the URL names none. None: all rows.
    pub default: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedChoiceConfig {
    pub name: String,
    pub label: Option<String>,
    /// The condition, as an expression of the engine IR.
    pub expr: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComputedConfig {
    pub name: String,
    pub label: Option<String>,
    /// The declared type: `int`, `bigint`, `float`, `bool`, `string`, `text`, `datetime`, `date`, `uuid`, `json` or `decimal`.
    #[serde(rename = "type")]
    pub ty: String,
    /// Whether the value may be null. Default: true.
    pub nullable: Option<bool>,
    /// A SQL expression, as an expression of the engine IR.
    pub expr: Option<Value>,
    /// The name of a host function: it takes `{"keys": [...]}` for one page and returns one value per key.
    pub function: Option<String>,
    /// Whether the column has a sort header. It needs `order_by`, or `expr` and `nullable: false`.
    pub sortable: Option<bool>,
    /// What the sort header orders by: a field path (a string) or an expression of the engine IR. It makes the column sortable.
    pub order_by: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AggregateConfig {
    pub name: String,
    pub label: Option<String>,
    /// A to-many relation of the model.
    pub relation: String,
    /// `count`, `sum`, `min`, `max` or `avg`.
    #[serde(rename = "fn")]
    pub function: String,
    /// A field of the related model: every function but `count` needs one.
    pub field: Option<String>,
}

/// The config of one action as a host sends it. An entry with no placement only declares a permission.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ActionConfig {
    pub name: String,
    pub label: Option<String>,
    pub placement: Option<String>,
    /// The permission name `<label>.<permission>_<resource>`. None: the action needs `manage`.
    pub permission: Option<String>,
    /// The text of the confirmation. None: no confirmation.
    pub confirm: Option<String>,
    #[serde(default)]
    pub inputs: Vec<InputConfig>,
    /// A prepared filter: the rows that the action applies to.
    pub applies_to: Option<Value>,
    /// A row permission condition with optional user slot 0.
    pub can: Option<Value>,
    #[serde(default)]
    pub replace_change: bool,
    #[serde(default)]
    pub sync: bool,
    #[serde(default)]
    pub threaded: bool,
}

#[derive(Deserialize, Default, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    pub create: Option<ServiceOp>,
    pub update: Option<ServiceOp>,
    pub delete: Option<ServiceOp>,
}

/// A control that the form shows next to the fields. Only a service reads its value.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct InputConfig {
    pub name: String,
    #[serde(rename = "type", default = "text")]
    pub kind: String,
    pub label: Option<String>,
    #[serde(default)]
    pub required: bool,
    pub help: Option<String>,
    #[serde(default)]
    pub choices: Vec<String>,
    /// The audit row stores a mask, not the value.
    #[serde(default)]
    pub secret: bool,
}

/// The config of one relation field of a resource.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RelationConfig {
    /// The rows of the target that the field accepts, on top of the `queryset` of the target.
    pub choices: Option<QuerysetConfig>,
    /// The form offers a modal that creates a record of the target.
    #[serde(default)]
    pub create: bool,
    /// The text column of the target that names a row in the picker. By default the first text column.
    pub label: Option<String>,
}

/// One route as the host sends it: `permission` is a full name, `<label>.<name>`.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct RouteConfig {
    pub method: String,
    pub path: String,
    pub permission: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BrandConfig {
    /// The name in the header and the page title. `Admin` by default.
    pub name: Option<String>,
    /// The URL of the logo image.
    pub logo: Option<String>,
    /// `primary` and `accent`, as `#rgb` or `#rrggbb`.
    #[serde(default)]
    pub colors: BTreeMap<String, String>,
    /// Links in the header.
    #[serde(default)]
    pub links: Vec<LinkConfig>,
    /// A short label such as `staging`, shown in the header.
    pub environment: Option<String>,
    /// `info`, `warning` or `danger`: the colour of the environment label. `production` gives danger, `staging` warning, else info.
    pub environment_level: Option<String>,
}

/// The service switcher: which service this admin is, its siblings and the build that runs. Code and `monorm.toml` may each set some keys.
#[derive(Deserialize, serde::Serialize, Default, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SwitcherConfig {
    /// The `key` of the service that this admin is.
    pub current: Option<String>,
    pub services: Option<Vec<SwitcherService>>,
    #[serde(default)]
    pub build: SwitcherBuild,
}

#[derive(Deserialize, serde::Serialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SwitcherService {
    pub key: String,
    pub title: String,
    /// A path that starts with `/` or an `https:` URL. Needed for every service but the current one.
    pub url: Option<String>,
    /// A lucide icon name.
    pub icon: Option<String>,
    /// `#rgb` or `#rrggbb`.
    pub color: Option<String>,
}

/// Plain strings that each project fills from its own environment. An empty string is no value.
#[derive(Deserialize, serde::Serialize, Default, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SwitcherBuild {
    pub commit: Option<String>,
    pub built_at: Option<String>,
    pub deployed_at: Option<String>,
    pub tree: Option<String>,
}

impl SwitcherConfig {
    /// The checks of the switcher that need no brand: the service keys, titles, icons, colours and urls, and `current`.
    pub fn check(&self) -> Result<(), Fault> {
        use crate::meta::{color_ok, icon_ok, url_ok};
        use crate::site::{err_code, invalid};
        let services = self.services.as_deref().unwrap_or(&[]);
        let current = self.current.as_deref();
        let mut keys: Vec<&str> = Vec::new();
        for svc in services {
            let at = format!("switcher.services[{:?}]", svc.key);
            if svc.key.is_empty() || svc.key.len() > 32 || !svc.key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_') {
                return invalid(format!("switcher.services: key {:?} must have 1 to 32 characters: lower case letters, digits, - and _", svc.key));
            }
            if keys.contains(&svc.key.as_str()) {
                return err_code(code::DUPLICATE, format!("switcher.services: key {:?} is listed twice", svc.key));
            }
            keys.push(&svc.key);
            if svc.title.trim().is_empty() || svc.title.chars().count() > 40 {
                return invalid(format!("{at}: title has 1 to 40 characters"));
            }
            if let Some(icon) = &svc.icon {
                if !icon_ok(icon) {
                    return invalid(format!("{at}: icon {icon:?} must be a lucide icon name such as \"server\""));
                }
            }
            if let Some(color) = &svc.color {
                if !color_ok(color) {
                    return invalid(format!("{at}: color {color:?} must be #rgb or #rrggbb"));
                }
            }
            if current != Some(svc.key.as_str()) {
                match &svc.url {
                    Some(u) if url_ok(u) && !u.starts_with("http://") => {}
                    Some(u) => return invalid(format!("{at}: url {u:?} must be a path that starts with / or an https URL")),
                    None => return invalid(format!("{at}: url is needed for a service that is not the current one")),
                }
            }
        }
        match current {
            Some(c) if !keys.contains(&c) => invalid(format!("switcher.current {c:?} is not the key of a service in switcher.services")),
            None if !services.is_empty() => invalid("switcher.current is needed when switcher.services is set".to_owned()),
            _ => Ok(()),
        }
    }

    /// The keys of `code` and of `file` together. A key in both is an error that names it.
    pub fn merge(code: Option<&SwitcherConfig>, file: Option<&SwitcherConfig>) -> Result<Option<SwitcherConfig>, String> {
        let (a, b) = match (code, file) {
            (None, None) => return Ok(None),
            (Some(one), None) | (None, Some(one)) => return Ok(Some(one.clone())),
            (Some(a), Some(b)) => (a, b),
        };
        fn pick<T: Clone>(key: &str, a: &Option<T>, b: &Option<T>) -> Result<Option<T>, String> {
            match (a, b) {
                (Some(_), Some(_)) => Err(format!("switcher.{} is set in the code and in monorm.toml: set it in one place", crate::error::opt(key))),
                (x, y) => Ok(x.clone().or_else(|| y.clone())),
            }
        }
        Ok(Some(SwitcherConfig {
            current: pick("current", &a.current, &b.current)?,
            services: pick("services", &a.services, &b.services)?,
            build: SwitcherBuild {
                commit: pick("build.commit", &a.build.commit, &b.build.commit)?,
                built_at: pick("build.built_at", &a.build.built_at, &b.build.built_at)?,
                deployed_at: pick("build.deployed_at", &a.build.deployed_at, &b.build.deployed_at)?,
                tree: pick("build.tree", &a.build.tree, &b.build.tree)?,
            },
        }))
    }
}

/// The short form of the look of a site: the title, the logo and the brand colour. It sets the same values as `brand`
/// (`title` is `brand.name`, `color` is `brand.colors.primary`); a value in both is a config error.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ThemeConfig {
    pub title: Option<String>,
    pub logo: Option<String>,
    pub color: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkConfig {
    pub label: String,
    pub url: String,
}

/// One package as the host sends it. Names in `widgets` and `forms` map to a file of `dir`; `styles` are files of `dir`.
#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct PackageConfig {
    pub name: String,
    pub admin_major: u32,
    pub dir: String,
    #[serde(default)]
    pub widgets: BTreeMap<String, String>,
    #[serde(default)]
    pub forms: BTreeMap<String, String>,
    /// Detail tabs: a name to a module that the detail page shows as a tab.
    #[serde(default)]
    pub tabs: BTreeMap<String, TabConfig>,
    #[serde(default)]
    pub styles: Vec<String>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(untagged)]
pub enum TabConfig {
    Module(String),
    Options(TabOptions),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrameConfig {
    pub name: String,
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub sandbox: Vec<String>,
    pub scripts: Option<bool>,
    pub same_origin: Option<bool>,
    pub height: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fieldset {
    pub name: String,
    pub title: Option<String>,
    pub fields: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbedConfig {
    pub resource: String,
    pub tab: String,
    pub lookup: String,
    pub ancestors: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SectionConfig {
    pub title: Option<String>,
    pub fields: Vec<String>,
    #[serde(default)]
    pub collapsed: bool,
}

/// One service function of a resource. `sync` is a plain Python function; it runs in a thread.
#[derive(Deserialize, Default, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ServiceOp {
    #[serde(default)]
    pub sync: bool,
    /// A sync function that the author marked as threaded: it runs in a thread, and its async database calls go through `ctx.run`.
    #[serde(default)]
    pub threaded: bool,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct TabOptions {
    pub module: String,
    #[serde(default)]
    pub embeddable: bool,
}

fn text() -> String {
    "text".into()
}

fn yes() -> bool {
    true
}
