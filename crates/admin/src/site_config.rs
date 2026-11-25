//! The tagged values of the config wire, lowered to the plain config that `site.rs` reads.
//!
//! A host sends a value that holds a host object as a tagged object: `{"$qs": {...}}` is a prepared query set, and
//! `{"$fn": "<local id>"}` is a host function. Rust derives the host name of each function (`queryset.<M>`,
//! `search.<M>`, `column.<M>.<name>`, with the inline tag `@<Parent>.<Child>[.<via>]` after `<M>` for a page that an
//! inline wrapper overrides) and returns the table of local id to host name. The binding registers each function
//! under the name of its id. A value without a tag is the plain config and passes through unchanged.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::error::{code, hint, Fault, Hint, PathPart};
use monorm_core::schema::Schema;

/// The host name of each local function id.
pub type Handles = BTreeMap<String, String>;

/// The `$qs` flags that a queryset of the admin refuses, with the call that sets each.
fn refused() -> [(&'static str, String); 5] {
    [("load", "load()".into()), ("slice", "a slice".into()), ("join", hint(Hint::FromJoin)), ("lock", "lock()".into()), ("without_defaults", hint(Hint::WithoutDefaults))]
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

/// The tag of a value: `Some((tag, body))` for an object whose first key starts with `$`.
fn tagged(value: &Value) -> Option<(&str, &Value)> {
    let map = value.as_object()?;
    let (key, body) = map.iter().next()?;
    key.starts_with('$').then_some((key.as_str(), body))
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
        if let Some(value) = list.get("search_with") {
            if let Some(name) = host_function(value, format!("search.{model}{tag}"), handles)? {
                list.insert("search_with".into(), Value::String(name));
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
            let Some(qs) = body.as_object() else { return Err(wrong()) };
            if let Some(of) = qs.get("model").and_then(Value::as_str).filter(|of| *of != model) {
                return Err(Fault::new(code::INVALID_VALUE, format!("queryset is a query set of {of}")));
            }
            for (flag, what) in refused() {
                if qs.get(flag).and_then(Value::as_bool) == Some(true) {
                    return Err(Fault::new(code::UNSUPPORTED, format!("queryset takes a filter and an order; it does not take {what}")));
                }
            }
            let filter = qs.get("filter").filter(|f| !f.is_null()).ok_or_else(|| Fault::new(code::INVALID_VALUE, "queryset needs a filter"))?;
            let slots = qs.get("slots").cloned().unwrap_or_else(|| json!([]));
            let order = qs.get("order").filter(|o| o.as_array().is_some_and(|a| !a.is_empty())).cloned();
            Ok((json!({"filter": filter, "slots": slots}), order))
        }
        _ => Err(wrong()),
    }
}
