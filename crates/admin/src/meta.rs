//! What the UI reads about a site: the brand and, for each resource, titles, widgets and links.
//! `Site::build` runs this once; a request only clones the result.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use monorm_core::ir::ColType;
use monorm_core::schema::Schema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::list::{is_text, ranged};
use crate::site::{err, Column, ConfigError, Filter, FilterKind, Resource, Source};

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BrandConfig {
    /// The name in the header and the page title. `Admin` by default.
    name: Option<String>,
    /// The URL of the logo image.
    logo: Option<String>,
    /// `primary` and `accent`, as `#rgb` or `#rrggbb`.
    #[serde(default)]
    colors: BTreeMap<String, String>,
    /// Links in the header.
    #[serde(default)]
    links: Vec<LinkConfig>,
    /// A short label such as `staging`, shown in the header.
    environment: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkConfig {
    label: String,
    url: String,
}

/// A link target that a page may use: a path on this host or an `http` or `https` URL.
fn url_ok(url: &str) -> bool {
    let plain = url.bytes().all(|b| b.is_ascii_graphic());
    plain && ((url.starts_with('/') && !url.starts_with("//")) || url.starts_with("https://") || url.starts_with("http://"))
}

fn color_ok(c: &str) -> bool {
    let digits = c.strip_prefix('#').unwrap_or("");
    matches!(digits.len(), 3 | 6) && digits.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The brand as the UI reads it. A wrong value is a config error that names the setting.
pub fn brand(config: Option<&BrandConfig>) -> Result<Value, ConfigError> {
    let default = BrandConfig::default();
    let c = config.unwrap_or(&default);
    let name = c.name.clone().unwrap_or_else(|| "Admin".to_owned());
    if name.trim().is_empty() || name.chars().count() > 80 {
        return err("brand.name must have 1 to 80 characters".into());
    }
    if let Some(logo) = &c.logo {
        // The CSP allows images from this origin and from https only.
        if !url_ok(logo) || logo.starts_with("http://") {
            return err(format!("brand.logo {logo:?} must be a path that starts with / or an https URL"));
        }
    }
    for (key, value) in &c.colors {
        if !matches!(key.as_str(), "primary" | "accent") {
            return err(format!("brand.colors has {key:?}, not one of primary, accent"));
        }
        if !color_ok(value) {
            return err(format!("brand.colors.{key} {value:?} must be #rgb or #rrggbb"));
        }
    }
    let mut links = Vec::new();
    for l in &c.links {
        if l.label.trim().is_empty() || l.label.chars().count() > 40 {
            return err("brand.links: a label has 1 to 40 characters".into());
        }
        if !url_ok(&l.url) {
            return err(format!("brand.links: {:?} must be a path that starts with / or an http or https URL", l.url));
        }
        links.push(json!({"label": l.label, "url": l.url}));
    }
    if c.environment.as_ref().is_some_and(|e| e.trim().is_empty() || e.chars().count() > 32) {
        return err("brand.environment must have 1 to 32 characters".into());
    }
    Ok(json!({"name": name, "logo": c.logo, "colors": c.colors, "links": links, "environment": c.environment}))
}

/// `created_at` and `BlogPost` become `Created at` and `Blog post`; a `.` of a path becomes a space.
pub fn humanize(name: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for ch in name.chars() {
        if ch == '_' || ch == '.' {
            out.push(' ');
            prev_lower = false;
            continue;
        }
        if ch.is_uppercase() && prev_lower {
            out.push(' ');
        }
        prev_lower = ch.is_lowercase() || ch.is_ascii_digit();
        out.extend(ch.to_lowercase());
    }
    let mut chars = out.trim().chars();
    chars.next().map(|f| f.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

fn widget(c: &Column) -> &'static str {
    if c.ty.enum_idx.is_some() {
        return "select";
    }
    if c.ty.array {
        return "list";
    }
    match c.ty.ty {
        ColType::Bool => "checkbox",
        ColType::Int | ColType::BigInt | ColType::Float | ColType::Decimal => "number",
        ColType::DateTime => "datetime",
        ColType::Date => "date",
        ColType::Text => "textarea",
        ColType::Json => "json",
        ColType::String | ColType::Uuid => "text",
    }
}

/// One column or field: the key in a row, the title, the type and the widget.
pub fn column(schema: &Schema, c: &Column) -> Value {
    let mut out = json!({"label": c.label, "title": humanize(&c.label), "type": c.ty.ty, "nullable": c.nullable, "array": c.ty.array, "widget": widget(c)});
    if let Some(e) = c.ty.enum_idx.and_then(|i| schema.enums.get(i as usize)) {
        out["choices"] = json!(e.values.iter().map(|v| v.name.clone()).collect::<Vec<_>>());
    }
    out
}

/// A column of a list: the title and the empty text that the config gives, and whether the value is computed.
fn list_column(schema: &Schema, r: &Resource, c: &Column) -> Value {
    let mut out = column(schema, c);
    if let Some(title) = r.labels.get(&c.label) {
        out["title"] = json!(title);
    }
    if let Some(text) = r.empty_fields.get(&c.label).or(r.empty.as_ref()) {
        out["empty"] = json!(text);
    }
    if !matches!(c.source, Source::Field) {
        out["computed"] = json!(true);
    }
    out
}

/// One filter of the filter bar: the name in the URL, the kind of value, and the operators that it takes.
fn filter(schema: &Schema, r: &Resource, f: &Filter) -> Value {
    let title = r.labels.get(&f.name).cloned().unwrap_or_else(|| humanize(&f.name));
    match &f.kind {
        FilterKind::Field(c) => {
            let kind = if c.ty.enum_idx.is_some() {
                "enum"
            } else {
                match c.ty.ty {
                    ColType::Bool => "bool",
                    ColType::Int | ColType::BigInt | ColType::Float => "number",
                    ColType::DateTime => "datetime",
                    ColType::Date => "date",
                    ColType::Uuid => "uuid",
                    _ => "text",
                }
            };
            let mut operators = vec!["eq"];
            if ranged(c.ty) {
                operators.extend(["gt", "gte", "lt", "lte"]);
            }
            if c.nullable || is_text(c.ty) {
                operators.extend(["empty", "notempty"]);
            }
            let mut out = json!({"name": f.name, "title": title, "kind": kind, "type": c.ty.ty, "nullable": c.nullable, "operators": operators});
            if let Some(e) = c.ty.enum_idx.and_then(|i| schema.enums.get(i as usize)) {
                out["choices"] = json!(e.values.iter().map(|v| v.name.clone()).collect::<Vec<_>>());
            }
            out
        }
        FilterKind::Relation { target, from, .. } => {
            let nullable = from.iter().all(|c| c.nullable);
            json!({"name": f.name, "title": title, "kind": "relation", "target": target, "nullable": nullable, "operators": if nullable { json!(["eq", "empty", "notempty"]) } else { json!(["eq"]) }})
        }
    }
}

/// The metadata of one resource that does not depend on the user.
pub fn resource(schema: &Schema, r: &Resource, builds: &AtomicUsize) -> Value {
    builds.fetch_add(1, Ordering::SeqCst);
    json!({
        "name": r.name,
        "title": r.title,
        "group": r.group,
        "columns": r.columns.iter().map(|c| list_column(schema, r, c)).collect::<Vec<_>>(),
        "order": r.order.iter().map(|o| json!({"column": o.column.label, "desc": o.desc})).collect::<Vec<_>>(),
        "page_size": r.page_size,
        "max_page_size": crate::site::MAX_PAGE_SIZE,
        "sortable": r.sortable.iter().map(|c| json!({"column": c.column.label, "indexed": c.indexed})).collect::<Vec<_>>(),
        "sortable_explicit": r.sortable_explicit,
        "search": json!({"fields": r.search.iter().map(|f| f.column.label.clone()).collect::<Vec<_>>(), "custom": r.search_with.is_some()}),
        "permissions": ["read"],
        "table": schema.model(r.model).table(),
        "key": r.key_columns.iter().map(|c| c.label.clone()).collect::<Vec<_>>(),
        "fields": r.detail.iter().map(|c| list_column(schema, r, c)).collect::<Vec<_>>(),
        "links": r.links.iter().map(|l| json!({"name": l.name, "title": humanize(&l.name), "target": l.target})).collect::<Vec<_>>(),
        "filters": r.filters.iter().map(|f| filter(schema, r, f)).collect::<Vec<_>>(),
        "named_filters": r.named_filters.iter().map(|n| json!({"name": n.name, "title": n.label, "default": n.default, "choices": n.choices.iter().map(|c| json!({"name": c.name, "title": c.label})).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        "tabs": r.tabs.iter().map(|t| json!({"name": t.name, "title": r.labels.get(&t.name).cloned().unwrap_or_else(|| humanize(&t.name)), "target": t.target})).collect::<Vec<_>>(),
        "empty": r.empty,
    })
}
