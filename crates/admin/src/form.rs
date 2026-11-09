//! The layout, the read-only fields, the widgets and the custom component of the forms.
//! `Site::build` checks the config once; a request only reads the result.

use std::collections::{BTreeMap, BTreeSet};

use monorm_core::ir::{ColType, FieldIr, ValueType};
use monorm_core::schema::Schema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::permissions::Op;
use crate::site::{err, Column, ConfigError};
use crate::write::{has_default, writable};

/// The widgets that the admin has built in.
pub const WIDGETS: [&str; 12] = ["text", "textarea", "number", "date", "datetime", "checkbox", "select", "radio", "json", "list", "email", "url"];

/// The most bytes of a custom form module.
const COMPONENT_MAX: usize = 512 * 1024;

/// The form settings of a resource, as the config gives them.
#[derive(Default)]
pub struct FormConfig {
    pub form: Option<Value>,
    pub readonly: Vec<String>,
    pub widgets: BTreeMap<String, String>,
    pub component: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SectionConfig {
    title: Option<String>,
    fields: Vec<String>,
    #[serde(default)]
    collapsed: bool,
}

#[derive(Debug, Clone)]
pub struct Section {
    pub title: Option<String>,
    pub fields: Vec<String>,
    pub collapsed: bool,
}

impl Section {
    pub fn json(&self) -> Value {
        json!({"title": self.title, "fields": self.fields, "collapsed": self.collapsed})
    }
}

/// What a resource form needs at a request: its sections, its read-only fields, its widgets and its module.
#[derive(Debug, Default)]
pub struct FormSpec {
    pub create: Vec<Section>,
    pub edit: Vec<Section>,
    /// Labels of the fields that a request cannot write.
    pub readonly: BTreeSet<String>,
    /// The widget of a field where the config sets one: label to widget.
    pub widgets: BTreeMap<String, String>,
    /// The source of the module that replaces the form.
    pub component: Option<String>,
}

/// What a widget key names.
#[derive(Debug, Clone, PartialEq)]
pub enum Key {
    Field(String, String),
    Path(String, String),
    Scalar(ColType),
    Enum(String),
    Nested(String),
}

fn scalar(name: &str) -> Option<ColType> {
    Some(match name {
        "String" => ColType::String,
        "Text" => ColType::Text,
        "Int" => ColType::Int,
        "BigInt" => ColType::BigInt,
        "Float" => ColType::Float,
        "Decimal" => ColType::Decimal,
        "Boolean" => ColType::Bool,
        "DateTime" => ColType::DateTime,
        "Date" => ColType::Date,
        "Json" => ColType::Json,
        "Uuid" => ColType::Uuid,
        _ => return None,
    })
}

fn value_type(f: &FieldIr) -> ValueType {
    let mut ty = ValueType::scalar(f.ty);
    ty.array = f.array;
    ty.enum_idx = f.enum_idx;
    ty
}

/// The widgets that can edit a value of the type.
fn fits(widget: &str, ty: ValueType) -> bool {
    let plain = !ty.array && ty.enum_idx.is_none();
    match widget {
        "select" | "radio" => ty.enum_idx.is_some() && !ty.array,
        "list" => ty.array,
        "json" => ty.array || (ty.enum_idx.is_none() && ty.ty == ColType::Json),
        "checkbox" => plain && ty.ty == ColType::Bool,
        "number" => plain && matches!(ty.ty, ColType::Int | ColType::BigInt | ColType::Float | ColType::Decimal),
        "date" => plain && ty.ty == ColType::Date,
        "datetime" => plain && ty.ty == ColType::DateTime,
        "text" | "textarea" | "email" | "url" => plain && matches!(ty.ty, ColType::String | ColType::Text | ColType::Uuid),
        _ => false,
    }
}

/// The type of the subfield at `path` (`geo.lat`) of a nested type: a leaf, or None for another nested type.
fn path_type(schema: &Schema, ty: &str, path: &str) -> Option<Option<ValueType>> {
    let mut cur = schema.types.iter().find(|t| t.name == ty)?;
    let mut parts = path.split('.').peekable();
    while let Some(part) = parts.next() {
        let field = cur.fields.iter().find(|f| f.name == part)?;
        match (&field.type_name, parts.peek()) {
            (Some(next), Some(_)) => cur = schema.types.iter().find(|t| &t.name == next)?,
            (Some(_), None) => return Some(None),
            (None, None) => {
                let leaf = field.leaf.as_ref()?;
                let mut vt = value_type(leaf);
                vt.array = field.list;
                return Some(Some(vt));
            }
            (None, Some(_)) => return None,
        }
    }
    None
}

/// The nested types on the way to a leaf, outermost first, each with the path from it: `Address` and `geo.tag` give `(Address, geo.tag)` and `(Geo, tag)`.
fn path_keys(schema: &Schema, ty: &str, path: &[String]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cur = ty;
    for (i, part) in path.iter().enumerate() {
        out.push((cur.to_owned(), path[i..].join(".")));
        let next = schema.types.iter().find(|t| t.name == cur).and_then(|t| t.fields.iter().find(|f| &f.name == part)).and_then(|f| f.type_name.as_deref());
        match next {
            Some(next) => cur = next,
            None => break,
        }
    }
    out
}

/// What the key names and the type that its widget has to fit: None when the widget is a nested type.
fn classify(schema: &Schema, own: Option<(&str, &[&Column])>, key: &str) -> Result<(Key, Option<ValueType>), String> {
    if let Some((left, right)) = key.split_once('.') {
        if let Some((model, fields)) = own {
            if left == model {
                let c = fields.iter().find(|c| c.label == right).ok_or_else(|| format!("{left}.{right} is not a field of the form"))?;
                return Ok((Key::Field(left.to_owned(), right.to_owned()), Some(c.ty)));
            }
            if schema.models.iter().any(|m| m.ir.name == left) {
                return Err(format!("names a field of {left}, and a resource names only its own fields"));
            }
        }
        if let Some(m) = schema.models.iter().find(|m| m.ir.name == left) {
            let f = m.ir.fields.iter().find(|f| f.name == right).ok_or_else(|| format!("{left} has no field {right:?}"))?;
            return Ok((Key::Field(left.to_owned(), right.to_owned()), Some(value_type(f))));
        }
        if schema.types.iter().any(|t| t.name == left) {
            let ty = path_type(schema, left, right).ok_or_else(|| format!("the nested type {left} has no subfield {right:?}"))?;
            return Ok((Key::Path(left.to_owned(), right.to_owned()), ty));
        }
        return Err("names no model or nested type".into());
    }
    if let Some((model, fields)) = own {
        if let Some(c) = fields.iter().find(|c| c.label == key) {
            return Ok((Key::Field(model.to_owned(), key.to_owned()), Some(c.ty)));
        }
    }
    if let Some(i) = schema.enums.iter().position(|e| e.name == key) {
        return Ok((Key::Enum(key.to_owned()), Some(ValueType { ty: ColType::String, array: false, enum_idx: Some(i as u32) })));
    }
    if schema.types.iter().any(|t| t.name == key) {
        return Ok((Key::Nested(key.to_owned()), None));
    }
    scalar(key).map(|t| (Key::Scalar(t), Some(ValueType::scalar(t)))).ok_or_else(|| "names no field or type".into())
}

/// The keys of a widget config, checked: each names something, and its widget exists and fits it.
fn keys(schema: &Schema, own: Option<(&str, &[&Column])>, widgets: &BTreeMap<String, String>, who: &str) -> Result<Vec<(Key, String)>, ConfigError> {
    let mut out: Vec<(Key, String)> = Vec::new();
    for (key, widget) in widgets {
        if !WIDGETS.contains(&widget.as_str()) {
            return err(format!("{who}: widgets has {widget:?} for {key:?}, not one of {}", WIDGETS.join(", ")));
        }
        let (k, ty) = classify(schema, own, key).map_err(|m| ConfigError(format!("{who}: widgets key {key:?} {m}")))?;
        let ok = match ty {
            Some(ty) => fits(widget, ty),
            None => widget == "json",
        };
        if !ok {
            return err(format!("{who}: the {widget:?} widget does not fit {key:?}"));
        }
        if out.iter().any(|(o, _)| *o == k) {
            return err(format!("{who}: widgets names {key:?} twice (as a field name and as Model.field)"));
        }
        out.push((k, widget.clone()));
    }
    Ok(out)
}

/// The global `widgets` of the site, checked against the schema.
pub fn global(schema: &Schema, widgets: &BTreeMap<String, String>) -> Result<Vec<(Key, String)>, ConfigError> {
    keys(schema, None, widgets, "widgets")
}

fn sections(name: &str, which: &str, layout: Option<&Value>, fields: &[&Column]) -> Result<Vec<Section>, ConfigError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for s in layout.map(|v| v.as_array().map(Vec::as_slice).unwrap_or_default()).unwrap_or_default() {
        let c: SectionConfig = serde_json::from_value(s.clone()).map_err(|e| ConfigError(format!("{name}: form {which}: {e}")))?;
        if c.fields.is_empty() {
            return err(format!("{name}: form {which}: a section has no fields"));
        }
        if c.title.as_ref().is_some_and(|t| t.trim().is_empty() || t.chars().count() > 80) {
            return err(format!("{name}: form {which}: a title has 1 to 80 characters"));
        }
        if c.collapsed && c.title.is_none() {
            return err(format!("{name}: form {which}: a collapsed section needs a title"));
        }
        for f in &c.fields {
            if !fields.iter().any(|d| &d.label == f) {
                return err(format!("{name}: form {which}: {f:?} is not a field of the form"));
            }
            if !seen.insert(f.clone()) {
                return err(format!("{name}: form {which}: {f:?} is in two places"));
            }
        }
        out.push(Section { title: c.title, fields: c.fields, collapsed: c.collapsed });
    }
    let rest: Vec<String> = fields.iter().filter(|c| !seen.contains(&c.label)).map(|c| c.label.clone()).collect();
    if !rest.is_empty() {
        out.push(Section { title: None, fields: rest, collapsed: false });
    }
    Ok(out)
}

/// The form of one resource, checked. `detail` is the detail columns of the resource.
pub fn build(schema: &Schema, idx: usize, name: &str, detail: &[Column], allow: &[Op], config: &FormConfig, global: &[(Key, String)]) -> Result<FormSpec, ConfigError> {
    let model = schema.model(idx);
    let fields: Vec<&Column> = detail.iter().filter(|c| writable(c)).collect();

    let mut readonly = BTreeSet::new();
    for n in &config.readonly {
        let Some(c) = fields.iter().find(|c| &c.label == n) else {
            return err(format!("{name}: readonly names {n:?}, which is not a field of the form"));
        };
        if allow.contains(&Op::Create) && model.field(&c.field).is_ok_and(|f| !f.nullable && !has_default(f)) {
            return err(format!("{name}: {n:?} is read-only and required with no default, so a new record cannot get a value"));
        }
        readonly.insert(n.clone());
    }

    let (create, edit) = match &config.form {
        None => (None, None),
        Some(Value::Array(_)) => (config.form.as_ref(), config.form.as_ref()),
        Some(Value::Object(o)) => {
            if let Some(k) = o.keys().find(|k| !matches!(k.as_str(), "create" | "edit")) {
                return err(format!("{name}: form has {k:?}, not one of create, edit"));
            }
            (o.get("create"), o.get("edit"))
        }
        Some(_) => return err(format!("{name}: form is a list of sections, or an object with create and edit")),
    };
    for (which, v) in [("create", create), ("edit", edit)] {
        if v.is_some_and(|v| !v.is_array()) {
            return err(format!("{name}: form {which} is a list of sections"));
        }
    }
    let create = sections(name, "create", create, &fields)?;
    let edit = sections(name, "edit", edit, &fields)?;

    let own = keys(schema, Some((name, &fields)), &config.widgets, name)?;
    let mut widgets = BTreeMap::new();
    for c in &fields {
        let Ok(f) = model.field(&c.field) else { continue };
        let by_field = |k: &Key| matches!(k, Key::Field(m, l) if m == name && *l == c.label);
        let by_type = |k: &Key| match k {
            Key::Scalar(t) => *t == c.ty.ty && !c.ty.array && c.ty.enum_idx.is_none() && f.nested.is_none(),
            Key::Enum(e) => !c.ty.array && c.ty.enum_idx.and_then(|i| schema.enums.get(i as usize)).is_some_and(|x| &x.name == e),
            Key::Nested(t) => f.nested.as_ref().is_some_and(|n| &n.ty == t && n.path.is_empty()),
            _ => false,
        };
        // The field beats the path, the path beats the type; the resource beats the site.
        let pick = |pred: &dyn Fn(&Key) -> bool| own.iter().chain(global).find(|(k, _)| pred(k)).map(|(_, w)| w.clone());
        // A path key names the subfield from any type on the way; the outer type wins.
        let paths = f.nested.as_ref().map(|n| path_keys(schema, &n.ty, &n.path)).unwrap_or_default();
        let by_path = || paths.iter().find_map(|(t, p)| pick(&|k: &Key| matches!(k, Key::Path(kt, kp) if kt == t && kp == p)));
        if let Some(w) = pick(&by_field).or_else(by_path).or_else(|| pick(&by_type)) {
            widgets.insert(c.label.clone(), w);
        }
    }

    let component = match &config.component {
        None => None,
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| ConfigError(format!("{name}: form_component {path:?}: {e}")))?;
            if text.trim().is_empty() || text.len() > COMPONENT_MAX {
                return err(format!("{name}: form_component {path:?} must hold 1 byte to {COMPONENT_MAX} bytes of JavaScript"));
            }
            Some(text)
        }
    };
    Ok(FormSpec { create, edit, readonly, widgets, component })
}
